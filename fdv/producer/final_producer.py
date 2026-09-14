#!/usr/bin/env python3
"""Real SQLite final inventory -> isolated transactional journal; NO egress.

stdlib only. Source always URI mode=ro + query_only, no immutable flag. Full
snapshot reconciliation, no monotonic cursor. The journal contains real final
text when pointed at the authorized real source; receipts expose aggregates and
hashes only. No TTS, player, network, RPC, clipboard or configuration integration.
"""
import argparse
from collections import Counter
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import stat
import uuid

if __package__:
    from .admission_gate import evaluate_final_admission, frozen_turns, is_frozen
else:
    from admission_gate import evaluate_final_admission, frozen_turns, is_frozen

ROOT = Path(__file__).resolve().parent
SQL = '''SELECT t.thread_id,t.turn_id,t.final_agent_item_id,
 t.rollout_ordinal,t.started_at,t.completed_at,t.first_user_item_id,
 i.item_type,i.item_json,i.updated_at_ordinal,u.item_json AS first_user_json
 FROM thread_turns AS t
 LEFT JOIN thread_items AS i ON i.thread_id=t.thread_id AND i.turn_id=t.turn_id
   AND i.item_id=t.final_agent_item_id
 LEFT JOIN thread_items AS u ON u.thread_id=t.thread_id AND u.turn_id=t.turn_id
   AND u.item_id=t.first_user_item_id
 WHERE t.thread_id=? AND t.status='completed' AND t.final_agent_item_id IS NOT NULL
 ORDER BY COALESCE(t.completed_at,t.started_at,t.rollout_ordinal),
   t.turn_id,t.final_agent_item_id'''

JOURNAL_DDL = '''
CREATE TABLE metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL);
CREATE TABLE polls(
 run_id TEXT PRIMARY KEY,snapshot_sha256 TEXT NOT NULL,source_rows INTEGER NOT NULL,
 new_jobs INTEGER NOT NULL DEFAULT 0,new_versions INTEGER NOT NULL DEFAULT 0,
 conflict_jobs INTEGER NOT NULL DEFAULT 0);
CREATE TABLE observations(
 thread_id TEXT NOT NULL,turn_id TEXT NOT NULL,item_id TEXT NOT NULL,
 version_sha256 TEXT NOT NULL,item_json TEXT,text TEXT,text_sha256 TEXT,
 classification TEXT NOT NULL,first_user_item_id TEXT,first_user_sha256 TEXT,
 first_poll TEXT NOT NULL,last_poll TEXT NOT NULL,
 PRIMARY KEY(thread_id,turn_id,item_id,version_sha256));
CREATE TABLE jobs(
 thread_id TEXT NOT NULL,turn_id TEXT NOT NULL,item_id TEXT NOT NULL,
 original_version_sha256 TEXT NOT NULL,text TEXT NOT NULL,text_sha256 TEXT NOT NULL,
 status TEXT NOT NULL CHECK(status IN
  ('HOLD_NC_PROVENANCE','HOLD_SOURCE_CONFLICT','HOLD_SOURCE_UNAVAILABLE',
   'HOLD_HISTORICAL','ELIGIBLE_FAKE_ONLY')),
 first_poll TEXT NOT NULL,last_poll TEXT NOT NULL,
 PRIMARY KEY(thread_id,turn_id,item_id));
CREATE TABLE admission_audit(
 thread_id TEXT NOT NULL,turn_id TEXT NOT NULL,item_id TEXT NOT NULL,
 generation_snapshot INTEGER,original_receipt_sha256 TEXT,
 latest_decision_json TEXT NOT NULL,last_poll TEXT NOT NULL,
 PRIMARY KEY(thread_id,turn_id,item_id));
'''


def sha(value):
    if isinstance(value, str): value = value.encode('utf-8')
    return hashlib.sha256(value).hexdigest()


def canonical(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(',', ':'))


def no_duplicate_keys(pairs):
    result = {}
    for key, value in pairs:
        if key in result: raise ValueError('DUPLICATE_JSON_KEY')
        result[key] = value
    return result


def local_output(path):
    """All writes confined to this new producer directory; no symlink aliases."""
    path = Path(path).absolute()
    if not path.is_relative_to(ROOT) or path == ROOT:
        raise ValueError('OUTPUT_MUST_BE_INSIDE_NEW_PRODUCER_DIRECTORY')
    for part in [path, *path.parents]:
        if part.is_symlink(): raise ValueError('OUTPUT_SYMLINK_FORBIDDEN')
        if part == ROOT: break
    if path.resolve() != path or not path.parent.is_dir():
        raise ValueError('OUTPUT_PARENT_MUST_EXIST_WITHOUT_PATH_ALIAS')
    if path.exists():
        info = path.stat()
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
            raise ValueError('OUTPUT_MUST_BE_SINGLE_LINK_REGULAR_FILE')
    return path


def schema_of(connection):
    return [{'name': r[0], 'sql': r[1]} for r in connection.execute(
        "SELECT name,sql FROM sqlite_master WHERE type='table' "
        "AND name IN ('thread_turns','thread_items') ORDER BY name")]


def connect_source(path):
    path = Path(path).resolve(strict=True)
    if not path.is_file(): raise ValueError('SOURCE_REGULAR_FILE_REQUIRED')
    connection = sqlite3.connect(path.as_uri() + '?mode=ro', uri=True,
                                 isolation_level=None, timeout=5)
    connection.execute('PRAGMA query_only=ON')
    if connection.execute('PRAGMA query_only').fetchone()[0] != 1:
        connection.close(); raise ValueError('READ_ONLY_GUARD_FAILED')
    return connection


def classify(row):
    identity = tuple(row[k] for k in ('thread_id', 'turn_id', 'final_agent_item_id'))
    if any(not isinstance(x, str) or not x for x in identity):
        raise ValueError('SOURCE_IDENTITY_INVALID')
    raw = row['item_json']
    text = None
    final_item_id = None
    reason = 'EXCLUDED_MISSING_FINAL_ITEM'
    if raw is not None:
        try:
            item = json.loads(raw, object_pairs_hook=no_duplicate_keys)
            if not isinstance(item, dict): raise ValueError('ITEM_NOT_OBJECT')
            final_item_id = item.get('id')
            if isinstance(item.get('text'), str): text = item['text']
            if item.get('id') != identity[2]: reason = 'EXCLUDED_ITEM_ID_MISMATCH'
            elif item.get('type') != 'agentMessage' or row['item_type'] != 'agentMessage':
                reason = 'EXCLUDED_NON_BACKEND_AGENT_MESSAGE'
            elif text is None or not text.strip(): reason = 'EXCLUDED_NO_FINAL_TEXT'
            elif item.get('delivery') is not None or item.get('questions'):
                reason = 'EXCLUDED_ASYNC_OR_QUESTIONS'
            elif item.get('phase') != 'final_answer' or 'delivery' not in item:
                reason = 'HOLD_NC_FINAL_SEMANTICS'
            else: reason = 'BACKEND_FINAL_PROVENANCE_NC'
        except (ValueError, TypeError, UnicodeError):
            reason = 'EXCLUDED_UNPARSEABLE_OR_AMBIGUOUS_JSON'
    # Hash context without copying the user's prompt to the journal or receipt.
    user_hash = sha(row['first_user_json']) if row['first_user_json'] is not None else None
    first_user = {'valid': False}
    if row['first_user_json'] is not None:
        try:
            user = json.loads(row['first_user_json'], object_pairs_hook=no_duplicate_keys)
            if not isinstance(user, dict): raise ValueError('USER_ITEM_NOT_OBJECT')
            # Actual DB projection uses camelCase clientId. Do not silently
            # accept snake_case aliases or infer origin from the identifier.
            first_user = {'id': user.get('id'), 'type': user.get('type'),
                          'client_id': user.get('clientId'),
                          'valid': ('clientId' in user and 'client_id' not in user
                                    and user.get('type') == 'userMessage'
                                    and user.get('id') == row['first_user_item_id'])}
        except (ValueError, TypeError, UnicodeError):
            first_user = {'valid': False}
    version = sha(canonical({'item_json': raw, 'item_type': row['item_type'],
                             'first_user_item_id': row['first_user_item_id'],
                             'first_user_sha256': user_hash}))
    return {'identity': identity, 'item_json': raw, 'text': text,
            'text_sha256': sha(text) if text is not None else None,
            'classification': reason, 'version_sha256': version,
            'first_user_item_id': row['first_user_item_id'],
            'first_user_sha256': user_hash, 'first_user': first_user,
            'final_pointer': identity[2], 'final_item_id': final_item_id}


def read_snapshot(source_path, thread_id):
    if not isinstance(thread_id, str) or not thread_id:
        raise ValueError('EXPLICIT_THREAD_REQUIRED')
    connection = connect_source(source_path)
    connection.row_factory = sqlite3.Row
    try:
        connection.execute('BEGIN')
        schema = schema_of(connection)
        schema_hash = sha(canonical(schema))
        expected = json.loads((ROOT / 'source-schema.json').read_text())['schema_sha256']
        if schema_hash != expected: raise ValueError('SOURCE_SCHEMA_CHANGED_STOP')
        records = [classify(row) for row in connection.execute(SQL, (thread_id,))]
        # End the source read transaction before opening any journal transaction.
        connection.rollback()
    finally:
        connection.close()
    digest = sha(canonical(sorted([(r['identity'], r['version_sha256']) for r in records])))
    return {'records': records, 'schema_sha256': schema_hash,
            'snapshot_sha256': digest, 'classifications': dict(Counter(
                r['classification'] for r in records)), 'source_rows': len(records)}


def expected_binding(source_path, thread_id, schema_hash):
    return {'schema': 'fdv.candidate.final.capture.journal.v2',
            'source_path': str(Path(source_path).resolve(strict=True)),
            'thread_id': thread_id, 'source_schema_sha256': schema_hash,
            'egress': 'DISABLED_REAL_EGRESS_ELIGIBILITY_IS_FAKE_ONLY'}


def ownership_proof(path, binding):
    """Linux cooperative ownership, not authentication against the same UID."""
    info = path.stat()
    if info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o600:
        raise ValueError('JOURNAL_MUST_BE_OWNED_PRIVATE_0600_FILE')
    return {'schema': 'vanius.candidate.journal.ownership.v1',
            'journal_path': str(path), 'device': info.st_dev, 'inode': info.st_ino,
            'owner_uid': info.st_uid, 'binding': binding}


def verify_ownership_proof(path, binding):
    proof_path = local_output(Path(str(path) + '.ownership.json'))
    expected = ownership_proof(path, binding)
    proof_info = proof_path.stat()
    if proof_info.st_uid != os.getuid() or stat.S_IMODE(proof_info.st_mode) != 0o600:
        raise ValueError('OWNERSHIP_PROOF_MUST_BE_PRIVATE_0600')
    proof = json.loads(proof_path.read_text(), object_pairs_hook=no_duplicate_keys)
    if proof != expected: raise ValueError('OWNERSHIP_PROOF_BINDING_OR_FILE_MISMATCH')


def establish_ownership_proof(path, binding):
    proof_path = local_output(Path(str(path) + '.ownership.json'))
    if not proof_path.exists():
        write_receipt(proof_path, ownership_proof(path, binding))
    verify_ownership_proof(path, binding)


def journal_metadata(path, read_only):
    c = connect_source(path) if read_only else sqlite3.connect(
        path.as_uri() + '?mode=rw', uri=True, isolation_level=None, timeout=5)
    try:
        # In the RW branch, this SELECT can perform SQLite hot-journal rollback.
        # That branch requires an independent, matching ownership proof FIRST.
        return dict(c.execute('SELECT key,value FROM metadata'))
    finally: c.close()


def open_journal(path, binding):
    path = local_output(path)
    if path == Path(binding['source_path']): raise ValueError('SOURCE_CANNOT_BE_JOURNAL')
    fresh = not path.exists()
    if fresh:
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        os.close(fd)
        establish_ownership_proof(path, binding)
    else:
        # Normal validation is RO. A hot journal may REQUIRE a write to roll
        # back spilled pages; only this exact error and the preexisting proof
        # authorize recovery of OUR candidate file, never the canonical source.
        try:
            stored = journal_metadata(path, read_only=True)
        except sqlite3.OperationalError as exc:
            if getattr(exc, 'sqlite_errorname', None) != 'SQLITE_READONLY_ROLLBACK':
                raise
            verify_ownership_proof(path, binding)
            stored = journal_metadata(path, read_only=False)
        if stored != binding: raise ValueError('JOURNAL_SOURCE_OR_THREAD_BINDING_MISMATCH')
        # A legacy candidate with readable matching metadata may be adopted;
        # an unreadable/hot file without prior proof is never adopted by guess.
        establish_ownership_proof(path, binding)
    c = sqlite3.connect(path.as_uri() + '?mode=rw', uri=True,
                        isolation_level=None, timeout=5)
    try:
        if c.execute('PRAGMA journal_mode=DELETE').fetchone()[0] != 'delete':
            raise ValueError('JOURNAL_MODE_GUARD')
        c.execute('PRAGMA synchronous=FULL')
        if c.execute('PRAGMA synchronous').fetchone()[0] != 2:
            raise ValueError('JOURNAL_SYNCHRONOUS_GUARD')
        if fresh:
            c.executescript('BEGIN IMMEDIATE;\n' + JOURNAL_DDL)
            c.executemany('INSERT INTO metadata VALUES(?,?)', sorted(binding.items()))
            c.commit()
        return c
    except BaseException:
        c.close(); raise


def poll_once(source_path, thread_id, journal_path, _test_hook=None,
              *, admission_receipts=None, authorization=None):
    frozen_turns()  # A missing/changed historical freeze is a STOP, not fallback.
    snapshot = read_snapshot(source_path, thread_id)
    binding = expected_binding(source_path, thread_id, snapshot['schema_sha256'])
    c = open_journal(journal_path, binding)
    run_id = uuid.uuid4().hex
    new_jobs = new_versions = conflicts = 0
    try:
        c.execute('BEGIN IMMEDIATE')
        c.execute('INSERT INTO polls(run_id,snapshot_sha256,source_rows) VALUES(?,?,?)',
                  (run_id, snapshot['snapshot_sha256'], snapshot['source_rows']))
        seen = set()
        for r in snapshot['records']:
            key = r['identity']; seen.add(key)
            added = c.execute('''INSERT OR IGNORE INTO observations VALUES
              (?,?,?,?,?,?,?,?,?,?,?,?)''', (*key, r['version_sha256'], r['item_json'],
                r['text'], r['text_sha256'], r['classification'],
                r['first_user_item_id'], r['first_user_sha256'], run_id, run_id)).rowcount
            new_versions += added
            c.execute('''UPDATE observations SET last_poll=? WHERE thread_id=?
                AND turn_id=? AND item_id=? AND version_sha256=?''',
                      (run_id, *key, r['version_sha256']))
            existing = c.execute('''SELECT original_version_sha256,status FROM jobs
                 WHERE thread_id=? AND turn_id=? AND item_id=?''', key).fetchone()
            prior = c.execute('''SELECT generation_snapshot,original_receipt_sha256
              FROM admission_audit WHERE thread_id=? AND turn_id=? AND item_id=?''', key).fetchone()
            scoped_record = dict(r)
            if prior is not None:
                scoped_record['job_voice_session_generation'] = prior[0]
                scoped_record['job_receipt_sha256'] = prior[1]
            decision = evaluate_final_admission(scoped_record, admission_receipts, authorization)
            status = decision['status']
            if existing is not None:
                changed = existing[0] != r['version_sha256']
                if is_frozen(key[0], key[1]):
                    status = 'HOLD_HISTORICAL'
                elif changed or existing[1] == 'HOLD_SOURCE_CONFLICT':
                    status = 'HOLD_SOURCE_CONFLICT'
                elif (existing[1] == 'HOLD_SOURCE_UNAVAILABLE'
                      or r['classification'] != 'BACKEND_FINAL_PROVENANCE_NC'):
                    status = 'HOLD_SOURCE_UNAVAILABLE'
                c.execute('''UPDATE jobs SET status=?,last_poll=?
                  WHERE thread_id=? AND turn_id=? AND item_id=?''', (status, run_id, *key))
                if changed: conflicts += 1
            elif r['classification'] == 'BACKEND_FINAL_PROVENANCE_NC':
                c.execute('INSERT INTO jobs VALUES(?,?,?,?,?,?,?,?,?)',
                          (*key, r['version_sha256'], r['text'], r['text_sha256'],
                           status, run_id, run_id))
                new_jobs += 1
            if existing is not None or r['classification'] == 'BACKEND_FINAL_PROVENANCE_NC':
                if decision['status'] != status:
                    decision = dict(decision, status=status, eligible_fake_only=False,
                                    reason='SOURCE_HISTORY_OR_CONFLICT_OVERRIDES_RECEIPT')
                generation = prior[0] if prior else None
                receipt_sha = prior[1] if prior else None
                if decision['eligible_fake_only'] and generation is None:
                    generation = decision['voice_session_generation']
                    receipt_sha = decision['receipt_sha256']
                c.execute('''INSERT INTO admission_audit VALUES(?,?,?,?,?,?,?)
                  ON CONFLICT(thread_id,turn_id,item_id) DO UPDATE SET
                  latest_decision_json=excluded.latest_decision_json,
                  last_poll=excluded.last_poll,
                  generation_snapshot=COALESCE(admission_audit.generation_snapshot,excluded.generation_snapshot),
                  original_receipt_sha256=COALESCE(admission_audit.original_receipt_sha256,excluded.original_receipt_sha256)''',
                          (*key, generation, receipt_sha, canonical(decision), run_id))
        for key in c.execute('SELECT thread_id,turn_id,item_id FROM jobs').fetchall():
            if tuple(key) not in seen:
                c.execute('''UPDATE jobs SET status='HOLD_SOURCE_UNAVAILABLE'
                  WHERE thread_id=? AND turn_id=? AND item_id=?
                  AND status NOT IN ('HOLD_SOURCE_CONFLICT','HOLD_HISTORICAL') ''', key)
                effective_status = c.execute('''SELECT status FROM jobs WHERE
                  thread_id=? AND turn_id=? AND item_id=?''', key).fetchone()[0]
                absent_decision = {'status': effective_status,
                                   'eligible_fake_only': False, 'egress_authorized': False,
                                   'reason': 'SOURCE_NOT_IN_CURRENT_SNAPSHOT',
                                   'source_payload_digest_verified': False}
                c.execute('''UPDATE admission_audit SET latest_decision_json=?,last_poll=?
                  WHERE thread_id=? AND turn_id=? AND item_id=?''',
                          (canonical(absent_decision), run_id, *key))
        c.execute('UPDATE polls SET new_jobs=?,new_versions=?,conflict_jobs=? WHERE run_id=?',
                  (new_jobs, new_versions, conflicts, run_id))
        if _test_hook: _test_hook('before_commit')
        c.commit()
        if _test_hook: _test_hook('after_commit')
        summary = dict(c.execute('SELECT status,count(*) FROM jobs GROUP BY status'))
        job_count = sum(summary.values())
        version_count = c.execute('SELECT count(*) FROM observations').fetchone()[0]
        # Job order is intentionally not a Voice execution order.
        journal_digest = sha(canonical(c.execute('''SELECT thread_id,turn_id,item_id,
            original_version_sha256,text_sha256,status FROM jobs
            ORDER BY thread_id,turn_id,item_id''').fetchall()))
    except BaseException:
        c.rollback(); raise
    finally:
        c.close()
    return {'result': 'CAPTURE_COMMITTED_EGRESS_HOLD_NC', 'run_id': run_id,
            'source_rows': snapshot['source_rows'], 'source_classifications': snapshot['classifications'],
            'source_schema_sha256': snapshot['schema_sha256'],
            'source_snapshot_sha256': snapshot['snapshot_sha256'],
            'source_access': 'URI_MODE_RO_QUERY_ONLY_TRANSACTION_NO_IMMUTABLE',
            'new_jobs': new_jobs, 'new_versions': new_versions,
            'conflict_jobs_observed': conflicts, 'journal_jobs': job_count,
            'journal_versions': version_count, 'journal_status_counts': summary,
            'journal_records_sha256': journal_digest, 'egress_authorized': False,
            'all_jobs_hold': summary.get('ELIGIBLE_FAKE_ONLY', 0) == 0,
            'fake_eligible_jobs': summary.get('ELIGIBLE_FAKE_ONLY', 0),
            'eligibility_scope': 'FAKE_ONLY_RECHECK_AUTHORIZATION_AT_PLAY_BOUNDARY',
            'source_payload_digest_verified': False,
            'text_normalization': 'NONE_FULL_TEXT_PRESERVED',
            'tail_source_exclusion_from_inventory': 'NC_NO_STRUCTURAL_PROVENANCE',
            'tail_egress': 'DISABLED_WITH_ALL_OTHER_UNPROVEN_JOBS',
            'windows_assertions': 0}


def write_receipt(path, result):
    path = local_output(path)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, 'w', encoding='utf-8') as f:
        json.dump(result, f, ensure_ascii=False, indent=2); f.write('\n')
        f.flush(); os.fsync(f.fileno())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, required=True)
    parser.add_argument('--thread', required=True)
    parser.add_argument('--journal', type=Path, required=True)
    parser.add_argument('--receipt', type=Path, required=True)
    parser.add_argument('--admission-receipts', type=Path)
    parser.add_argument('--authorization', type=Path)
    args = parser.parse_args()
    receipts = (json.loads(args.admission_receipts.read_text(), object_pairs_hook=no_duplicate_keys)
                if args.admission_receipts else None)
    authorization = (json.loads(args.authorization.read_text(), object_pairs_hook=no_duplicate_keys)
                     if args.authorization else None)
    result = poll_once(args.source, args.thread, args.journal,
                      admission_receipts=receipts, authorization=authorization)
    result.update(schema='vanius.real.final.producer.receipt.v1',
                  producer_sha256=sha(Path(__file__).read_bytes()),
                  sqlite_version=sqlite3.sqlite_version,
                  source_thread_sha256=sha(args.thread),
                  journal_file_sha256=sha(args.journal.read_bytes()),
                  journal_distribution='LOCAL_ONLY_CONTAINS_FULL_FINAL_TEXT_NOT_FOR_SHARING',
                  real_tts_voice_audio_rpc=False)
    write_receipt(args.receipt, result)
    print(canonical(result))  # aggregate counts/hashes; never final or prompt text


if __name__ == '__main__':
    main()

#!/usr/bin/env python3
"""Real SQLite tests with synthetic rows and benign Linux subprocess crashes.
Every writable file is inside a fresh test directory beneath this producer.
No canonical source writes, network, RPC, TTS, audio, clipboard or Windows calls.
"""
from __future__ import annotations
from collections import Counter
import json
from pathlib import Path
import sqlite3
import subprocess
import sys
import uuid
from final_producer import (ROOT, canonical, connect_source, poll_once, open_journal, expected_binding,
                            read_snapshot, schema_of, sha, write_receipt)

sys.dont_write_bytecode = True
RUN = ROOT / ('tests-' + uuid.uuid4().hex)
RUN.mkdir(exist_ok=False)
ASSERTIONS = 0
SUBPROCESSES = 0
RESULTS = []


def check(condition, message):
    global ASSERTIONS
    ASSERTIONS += 1
    if not condition: raise AssertionError(message)


def fails(fn, expected):
    try: fn()
    except Exception as exc:
        check(expected in str(exc), 'expected failure reason: ' + expected)
    else: check(False, 'expected operation to fail: ' + expected)


def case(name):
    def wrapped(fn):
        before = ASSERTIONS
        try:
            fn()
            RESULTS.append({'name': name, 'result': 'PASS', 'assertions': ASSERTIONS - before})
        except Exception as exc:
            RESULTS.append({'name': name, 'result': 'FAIL', 'assertions': ASSERTIONS - before,
                            'error': type(exc).__name__ + ': ' + str(exc)})
        return fn
    return wrapped


def fixture():
    folder = RUN / ('case-' + uuid.uuid4().hex)
    folder.mkdir(exist_ok=False)
    write_receipt(folder / 'SYNTHETIC-FIXTURE.json', {'synthetic_only': True})
    c = sqlite3.connect(folder / 'source.sqlite')
    for table in json.loads((ROOT / 'source-schema.json').read_text())['tables']:
        c.execute(table['sql'])
    c.commit(); c.close()
    return folder


def add(folder, turn='T1', item='I1', text='FAKE exact final.\n', ordinal=1,
        completed=10, status='completed', thread='FAKE_THREAD', changes=None,
        item_type='agentMessage', user_text='FAKE request', omit_item=False):
    payload = {'id': item, 'type': 'agentMessage', 'text': text,
               'phase': 'final_answer', 'delivery': None, 'questions': [],
               'memoryCitation': None}
    if changes: payload.update(changes)
    c = sqlite3.connect(folder / 'source.sqlite')
    c.execute('''INSERT INTO thread_turns(thread_id,turn_id,rollout_ordinal,status,
      started_at,completed_at,first_user_item_id,final_agent_item_id)
      VALUES(?,?,?,?,?,?,?,?)''',
              (thread, turn, ordinal, status, ordinal, completed, 'U-' + turn, item))
    c.execute('''INSERT INTO thread_items(thread_id,turn_id,item_id,rollout_ordinal,
      created_at_ms,item_json,item_type) VALUES(?,?,?,?,?,?,?)''',
              (thread, turn, 'U-' + turn, ordinal, ordinal,
               canonical({'id': 'U-' + turn, 'type': 'userMessage', 'clientId': None,
                          'content': [{'type': 'text', 'text': user_text, 'text_elements': []}]}),
               'userMessage'))
    if not omit_item:
        c.execute('''INSERT INTO thread_items(thread_id,turn_id,item_id,rollout_ordinal,
          created_at_ms,item_json,item_type) VALUES(?,?,?,?,?,?,?)''',
                  (thread, turn, item, ordinal, ordinal, canonical(payload), item_type))
    c.commit(); c.close()
    return payload


def modify(folder, sql, params=()):
    c = sqlite3.connect(folder / 'source.sqlite')
    c.execute(sql, params); c.commit(); c.close()


def poll(folder):
    return poll_once(folder / 'source.sqlite', 'FAKE_THREAD', folder / 'journal.sqlite')


def rows(folder, sql, params=()):
    c = connect_source(folder / 'journal.sqlite')
    try: return c.execute(sql, params).fetchall()
    finally: c.close()


def child(args):
    global SUBPROCESSES
    SUBPROCESSES += 1
    return subprocess.run([sys.executable, '-B', *args], cwd=ROOT,
                          capture_output=True, text=True, timeout=20, check=False)


@case('real_schema_fixture_and_source_connection_are_read_only')
def _():
    f = fixture(); add(f)
    before = sha((f / 'source.sqlite').read_bytes())
    c = connect_source(f / 'source.sqlite')
    check(c.execute('PRAGMA query_only').fetchone()[0] == 1, 'query_only enabled')
    expected = json.loads((ROOT / 'source-schema.json').read_text())['schema_sha256']
    check(sha(canonical(schema_of(c))) == expected, 'fixture uses exact real DDL')
    fails(lambda: c.execute("UPDATE thread_turns SET status='interrupted'"), 'readonly')
    fails(lambda: c.execute("INSERT INTO thread_turns(thread_id,turn_id,rollout_ordinal,status) VALUES('FAKE','FAKE',1,'completed')"), 'readonly')
    c.close(); poll(f)
    check(sha((f / 'source.sqlite').read_bytes()) == before, 'candidate never modified fixture source bytes')


@case('two_finals_between_polls_and_idempotent_full_rescan')
def _():
    f = fixture(); empty = poll(f)
    check(empty['journal_jobs'] == 0, 'empty initial source')
    add(f, 'Z-TURN', 'Z-ITEM', 'FAKE first', 20, 100)
    add(f, 'A-TURN', 'A-ITEM', 'FAKE second', 10, 100)
    first = poll(f); second = poll(f)
    check(first['new_jobs'] == 2 and first['journal_jobs'] == 2, 'both complete finals captured')
    check(second['new_jobs'] == 0 and second['new_versions'] == 0, 'replay deduplicated by triple and version')
    check(second['journal_records_sha256'] == first['journal_records_sha256'], 'job contents stable across scans')
    check(rows(f, 'SELECT count(*) FROM jobs WHERE status!=?', ('HOLD_NC_PROVENANCE',))[0][0] == 0,
          'no captured final was authorized to speak')


@case('late_completion_with_older_timestamp_and_item_ordinal_is_not_lost')
def _():
    f = fixture()
    add(f, 'OLDER', 'A', 'FAKE late', 1, 1, status='inProgress')
    add(f, 'NEWER', 'Z', 'FAKE early', 90, 100)
    check(poll(f)['new_jobs'] == 1, 'only current completed row first')
    modify(f, "UPDATE thread_turns SET status='completed' WHERE turn_id='OLDER'")
    later = poll(f)
    check(later['new_jobs'] == 1 and later['journal_jobs'] == 2, 'late completion behind timestamp recovered')
    check({r[0] for r in rows(f, 'SELECT item_id FROM jobs')} == {'A', 'Z'}, 'out-of-order IDs preserved')


@case('real_new_process_restart_recovers_backlog_without_duplicate')
def _():
    f = fixture(); add(f); poll(f)
    add(f, 'T2', 'I2', 'FAKE offline backlog', 2, 2)
    completed = child([str(ROOT / 'final_producer.py'), '--source', str(f / 'source.sqlite'),
                       '--thread', 'FAKE_THREAD', '--journal', str(f / 'journal.sqlite'),
                       '--receipt', str(f / 'RESTART.json')])
    check(completed.returncode == 0, 'new real Python process completed successfully')
    receipt = json.loads((f / 'RESTART.json').read_text())
    check(receipt['new_jobs'] == 1 and receipt['journal_jobs'] == 2, 'restart backlog captured')
    check(poll(f)['new_jobs'] == 0, 'next new connection remains idempotent')
    check(not receipt['egress_authorized'], 'restart grants no voice authority')


@case('same_identity_changed_text_keeps_original_and_records_conflict_version')
def _():
    f = fixture(); original = add(f, text='FAKE original\n[COMPLETE] literal'); poll(f)
    changed = dict(original, text='FAKE changed body')
    modify(f, "UPDATE thread_items SET item_json=? WHERE item_id='I1'", (canonical(changed),))
    result = poll(f)
    job = rows(f, 'SELECT text,text_sha256,status FROM jobs')[0]
    check(job == (original['text'], sha(original['text']), 'HOLD_SOURCE_CONFLICT'),
          'original canonical job text is not overwritten')
    check(result['journal_jobs'] == 1 and result['journal_versions'] == 2, 'new version recorded without duplicate job')
    check(poll(f)['new_versions'] == 0, 'repeated conflicting version deduplicated')
    check({r[0] for r in rows(f, 'SELECT text FROM observations')} == {original['text'], changed['text']},
          'both source versions preserved')
    modify(f, "UPDATE thread_items SET item_json=? WHERE item_id='I1'", (canonical(original),))
    check(poll(f)['new_versions'] == 0, 'return to original payload is not a third version')
    check(rows(f, 'SELECT status FROM jobs')[0][0] == 'HOLD_SOURCE_CONFLICT',
          'reverting source bytes cannot silently clear prior conflict')


@case('pointer_change_and_status_regression_hold_old_job_without_deleting_text')
def _():
    f = fixture(); old = add(f); poll(f)
    new = dict(old, id='I2', text='FAKE replacement final')
    modify(f, '''INSERT INTO thread_items(thread_id,turn_id,item_id,rollout_ordinal,
      created_at_ms,item_json,item_type) VALUES('FAKE_THREAD','T1','I2',2,2,?,'agentMessage')''', (canonical(new),))
    modify(f, "UPDATE thread_turns SET final_agent_item_id='I2'")
    check(poll(f)['new_jobs'] == 1, 'new pointer gets distinct identity')
    check(rows(f, "SELECT text,status FROM jobs WHERE item_id='I1'")[0] ==
          (old['text'], 'HOLD_SOURCE_UNAVAILABLE'), 'prior pointer retained but no longer current')
    modify(f, "UPDATE thread_turns SET status='interrupted'")
    check(poll(f)['source_rows'] == 0, 'interrupted turn has no terminal inventory candidate')
    check(rows(f, "SELECT status FROM jobs WHERE item_id='I2'")[0][0] == 'HOLD_SOURCE_UNAVAILABLE',
          'regressed turn cannot leave a current job')


@case('missing_projection_item_can_arrive_later_under_same_final_pointer')
def _():
    f = fixture(); payload = add(f, omit_item=True)
    check(poll(f)['journal_jobs'] == 0, 'dangling pointer is not a final job')
    modify(f, '''INSERT INTO thread_items(thread_id,turn_id,item_id,rollout_ordinal,
      created_at_ms,item_json,item_type) VALUES('FAKE_THREAD','T1','I1',1,1,?,'agentMessage')''', (canonical(payload),))
    check(poll(f)['new_jobs'] == 1, 'late projection item captured without cursor loss')
    check(rows(f, 'SELECT count(*) FROM observations')[0][0] == 2, 'missing and filled observations retained')


@case('frontend_async_questions_and_ambiguous_json_never_become_jobs')
def _():
    f = fixture()
    add(f, 'T1', 'I1', changes={'type': 'realtime'}, item_type='realtime')
    add(f, 'T2', 'I2', changes={'delivery': 'async'})
    add(f, 'T3', 'I3', changes={'questions': ['FAKE question']})
    add(f, 'T4', 'I4', changes={'phase': 'commentary'})
    add(f, 'T5', 'I5', changes={'id': 'WRONG'})
    add(f, 'T6', 'I6')
    modify(f, "UPDATE thread_items SET item_json=? WHERE item_id='I6'",
           ('{"id":"I6","id":"OTHER","type":"agentMessage","text":"FAKE"}',))
    result = poll(f)
    check(result['source_rows'] == 6 and result['journal_jobs'] == 0, 'no invalid origin/finality became job')
    check(result['source_classifications']['EXCLUDED_ASYNC_OR_QUESTIONS'] == 2, 'async and questions explicitly excluded')
    check(result['source_classifications']['EXCLUDED_UNPARSEABLE_OR_AMBIGUOUS_JSON'] == 1, 'duplicate keys fail closed')
    check(result['journal_versions'] == 6, 'excluded source observations preserved for audit')


@case('tail_or_delegation_text_cannot_authorize_and_complete_is_never_stripped')
def _():
    f = fixture()
    texts = ['[COMPLETE] literal response\n', 'FAKE final with <source>delegation</source>',
             'FAKE response after tail', 'FAKE Unicode: Vânius — ação.\n\n']
    for n, text in enumerate(texts):
        prompt = '<realtime_conversation><source>transcript_tail_flush</source>FAKE</realtime_conversation>'
        add(f, 'T' + str(n), 'I' + str(n), text, n + 1, n + 1, user_text=prompt)
    result = poll(f)
    check(result['journal_jobs'] == 4, 'backend finals inventoried despite unproven voice provenance')
    check(result['tail_source_exclusion_from_inventory'].startswith('NC_'), 'no invented structural tail classification')
    check(not result['egress_authorized'] and result['all_jobs_hold'], 'tail and every unproven final excluded from egress')
    check([r[0] for r in rows(f, 'SELECT text FROM jobs ORDER BY item_id')] == texts,
          'COMPLETE, wrappers, Unicode and whitespace remain exact')
    check(rows(f, "SELECT count(*) FROM jobs WHERE status='HOLD_NC_PROVENANCE'")[0][0] == 4,
          'textual source hints do not escape provenance HOLD')


@case('journal_binding_prevents_mixing_threads_or_sources')
def _():
    f = fixture(); add(f); add(f, 'OTHER', 'OTHER', thread='OTHER_THREAD'); poll(f)
    check(rows(f, 'SELECT count(*) FROM jobs')[0][0] == 1, 'explicit thread filter honored')
    digest = sha((f / 'journal.sqlite').read_bytes())
    fails(lambda: poll_once(f / 'source.sqlite', 'OTHER_THREAD', f / 'journal.sqlite'),
          'JOURNAL_SOURCE_OR_THREAD_BINDING_MISMATCH')
    other = fixture(); add(other)
    fails(lambda: poll_once(other / 'source.sqlite', 'FAKE_THREAD', f / 'journal.sqlite'),
          'JOURNAL_SOURCE_OR_THREAD_BINDING_MISMATCH')
    check(sha((f / 'journal.sqlite').read_bytes()) == digest, 'wrong binding causes no journal mutation')


@case('schema_drift_stops_before_journal_creation')
def _():
    f = fixture(); add(f)
    modify(f, 'ALTER TABLE thread_items ADD COLUMN unknown_future_field TEXT')
    fails(lambda: poll(f), 'SOURCE_SCHEMA_CHANGED_STOP')
    check(not (f / 'journal.sqlite').exists(), 'unknown source schema cannot create a journal')


@case('real_abrupt_process_exit_before_commit_rolls_back_entire_poll')
def _():
    f = fixture(); add(f); add(f, 'T2', 'I2', ordinal=2)
    completed = child([str(ROOT / 'crash_worker.py'), str(f), 'before_commit'])
    check(completed.returncode == 91, 'exact deliberate precommit process exit observed')
    check(json.loads((f / 'REACHED-before_commit.json').read_text())['phase'] == 'before_commit',
          'child reached the intended transaction boundary')
    for table in ('polls', 'observations', 'jobs'):
        check(rows(f, 'SELECT count(*) FROM ' + table)[0][0] == 0, 'no partial ' + table + ' after crash')
    result = poll(f)
    check(result['new_jobs'] == 2 and result['journal_versions'] == 2, 'fresh process/connection recovers full backlog')


@case('real_abrupt_process_exit_after_commit_retains_jobs_and_deduplicates')
def _():
    f = fixture(); add(f); add(f, 'T2', 'I2', ordinal=2)
    completed = child([str(ROOT / 'crash_worker.py'), str(f), 'after_commit'])
    check(completed.returncode == 92, 'exact deliberate postcommit process exit observed')
    check(json.loads((f / 'REACHED-after_commit.json').read_text())['phase'] == 'after_commit',
          'child reached committed boundary')
    check(rows(f, 'SELECT count(*) FROM jobs')[0][0] == 2, 'committed jobs survive process exit')
    result = poll(f)
    check(result['new_jobs'] == 0 and result['new_versions'] == 0, 'committed rows not duplicated on restart')
    check(result['journal_jobs'] == 2 and result['all_jobs_hold'], 'restart grants no egress')


@case('hot_rollback_recovery_requires_matching_independent_ownership_proof')
def _():
    f = fixture()
    for n in range(8):
        add(f, 'T' + str(n), 'I' + str(n), 'FAKE SPILL ' + ('x' * 32768), n + 1, n + 1)
    crashed = child([str(ROOT / 'crash_worker.py'), str(f), 'before_commit_spill'])
    check(crashed.returncode == 93, 'exact cache-spill precommit crash observed')
    check(json.loads((f / 'REACHED-before_commit_spill.json').read_text())['phase'] == 'before_commit_spill',
          'spill child reached exact boundary')
    check((f / 'journal.sqlite-journal').is_file(), 'rollback journal exists after real crash')
    try:
        probe = connect_source(f / 'journal.sqlite')
        try: probe.execute('SELECT key,value FROM metadata').fetchall()
        finally: probe.close()
    except sqlite3.OperationalError as exc:
        check(getattr(exc, 'sqlite_errorname', None) == 'SQLITE_READONLY_ROLLBACK',
              'specific readonly rollback failure reproduced before recovery')
    else: check(False, 'test must actually spill and require rollback')
    snapshot = read_snapshot(f / 'source.sqlite', 'FAKE_THREAD')
    binding = expected_binding(f / 'source.sqlite', 'FAKE_THREAD', snapshot['schema_sha256'])
    before = sha((f / 'journal.sqlite').read_bytes())
    wrong = dict(binding, thread_id='OTHER_THREAD')
    fails(lambda: open_journal(f / 'journal.sqlite', wrong), 'OWNERSHIP_PROOF_BINDING_OR_FILE_MISMATCH')
    check(sha((f / 'journal.sqlite').read_bytes()) == before, 'wrong binding cannot trigger recovery writes')
    # Missing proof is a separate fail-closed control; restore only our exact
    # synthetic bytes afterward. No canonical file participates in this test.
    proof_path = Path(str(f / 'journal.sqlite') + '.ownership.json')
    original_proof = proof_path.read_bytes(); proof_path.rename(f / 'PROOF-HELD.json')
    try:
        try: open_journal(f / 'journal.sqlite', binding)
        except FileNotFoundError: check(True, 'hot file without prior proof refused')
        else: check(False, 'missing proof must not authorize recovery')
        check(sha((f / 'journal.sqlite').read_bytes()) == before, 'missing proof caused no main DB mutation')
    finally: (f / 'PROOF-HELD.json').rename(proof_path)
    check(proof_path.read_bytes() == original_proof, 'exact synthetic proof restored')
    recovered = open_journal(f / 'journal.sqlite', binding)
    try:
        for table in ('polls', 'observations', 'jobs'):
            check(recovered.execute('SELECT count(*) FROM ' + table).fetchone()[0] == 0,
                  'spilled uncommitted ' + table + ' rolled back entirely')
    finally: recovered.close()
    result = poll(f)
    check(result['new_jobs'] == 8 and result['journal_versions'] == 8, 'full backlog captured after real recovery')
    check(poll(f)['new_jobs'] == 0, 'post-recovery rescan deduplicates')


def main():
    failed = sum(r['result'] == 'FAIL' for r in RESULTS)
    result = {'schema': 'vanius.real.sqlite.producer.tests.v1',
              'result': 'PASS' if RESULTS and ASSERTIONS and not failed else 'FAIL',
              'assertions': ASSERTIONS, 'scenario_count': len(RESULTS),
              'failed_scenarios': failed, 'real_linux_subprocesses': SUBPROCESSES,
              'sqlite_version': sqlite3.sqlite_version,
              'fixtures': 'SYNTHETIC_ROWS_IN_EXACT_REAL_SOURCE_DDL',
              'journal': 'REAL_ISOLATED_SQLITE_DELETE_JOURNAL_SYNCHRONOUS_FULL',
              'windows_assertions': 0, 'voice_tts_audio_network_rpc': False,
              'power_failure_durability_proved': False,
              'real_voice_provenance_proved': False,
              'source_sha256': {name: sha((ROOT / name).read_bytes()) for name in
                                ('final_producer.py', 'crash_worker.py', 'test_producer.py', 'source-schema.json')},
              'cases': RESULTS}
    write_receipt(RUN / 'TEST-RECEIPT.json', result)
    print(canonical({'result': result['result'], 'assertions': ASSERTIONS,
                     'scenarios': len(RESULTS), 'failed': failed,
                     'real_linux_subprocesses': SUBPROCESSES,
                     'receipt': str(RUN / 'TEST-RECEIPT.json'),
                     'receipt_sha256': sha((RUN / 'TEST-RECEIPT.json').read_bytes())}))
    return 0 if result['result'] == 'PASS' else 1


if __name__ == '__main__':
    raise SystemExit(main())

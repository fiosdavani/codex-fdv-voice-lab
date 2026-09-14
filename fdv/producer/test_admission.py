#!/usr/bin/env python3
"""Real isolated SQLite + proposed receipt/auth contract; no real egress.
All source bodies are synthetic. Historical IDs/hashes are read from the frozen
ledger, never its original private text. No network, TTS, audio or RPC.
"""
import copy
import json
from pathlib import Path
import sqlite3
import sys
import uuid

from admission_gate import evaluate_final_admission, frozen_turns, parse_freeze
from final_producer import (ROOT, canonical, connect_source, poll_once,
                            read_playback_evidence, read_snapshot, sha, write_receipt)

sys.dont_write_bytecode = True
RUN = ROOT / ('admission-tests-' + uuid.uuid4().hex)
RUN.mkdir(exist_ok=False)
ASSERTIONS = 0
RESULTS = []


def check(value, message):
    global ASSERTIONS
    ASSERTIONS += 1
    if not value: raise AssertionError(message)


def case(name):
    def wrap(fn):
        before = ASSERTIONS
        try:
            fn(); RESULTS.append({'name': name, 'result': 'PASS', 'assertions': ASSERTIONS-before})
        except Exception as exc:
            RESULTS.append({'name': name, 'result': 'FAIL', 'assertions': ASSERTIONS-before,
                            'error': type(exc).__name__ + ': ' + str(exc)})
        return fn
    return wrap


def contracts(thread='FAKE_THREAD', turn='FAKE_TURN', generation=1, input_digest=None):
    origin = 'opaque_origin_without_generation_syntax'
    receipt = {'schema': 'fdv.voice.admission.v1', 'receipt_id': 'FAKE_QUEUE',
               'thread_id': thread, 'native_session_id': 'FAKE_NATIVE_SESSION',
               'voice_session_generation': generation,
               'origin_id': origin, 'handoff_id': None, 'item_id': None,
               'client_id': origin, 'queued_item_id': 'FAKE_QUEUE',
               'turn_id': turn, 'admission_result': 'Started',
               'attempt_id': 'FAKE_ATTEMPT', 'input_digest': input_digest}
    auth = {'schema': 'fdv.voice.authorization.v1', 'state': 'Active', 'job_generation': 0,
            **{k: receipt[k] for k in ('thread_id', 'native_session_id', 'voice_session_generation',
                                      'origin_id', 'client_id', 'queued_item_id', 'input_digest')}}
    return receipt, auth


def fixture(thread='FAKE_THREAD', turn='FAKE_TURN', item='FAKE_FINAL',
            text='FAKE final text.\n', user_patch=None, final_patch=None,
            snake_only=False, duplicate_client=False):
    folder = RUN / ('case-' + uuid.uuid4().hex); folder.mkdir(exist_ok=False)
    receipt, auth = contracts(thread, turn)
    user = {'id': 'FAKE_USER', 'type': 'userMessage', 'clientId': receipt['client_id'],
            'content': [{'type': 'text', 'text': 'FAKE input body', 'text_elements': []}]}
    if user_patch: user.update(user_patch)
    if snake_only: user['client_id'] = user.pop('clientId')
    user_json = canonical(user)
    if duplicate_client:
        user_json = user_json[:-1] + ',"clientId":"DUPLICATE"}'
    final = {'id': item, 'type': 'agentMessage', 'text': text, 'phase': 'final_answer',
             'delivery': None, 'questions': [], 'memoryCitation': None}
    if final_patch: final.update(final_patch)
    c = sqlite3.connect(folder / 'source.sqlite')
    for table in json.loads((ROOT / 'source-schema.json').read_text())['tables']:
        c.execute(table['sql'])
    c.execute('''INSERT INTO thread_turns(thread_id,turn_id,rollout_ordinal,status,
      first_user_item_id,final_agent_item_id) VALUES(?,?,1,'completed','FAKE_USER',?)''',
              (thread, turn, item))
    c.execute('''INSERT INTO thread_items(thread_id,turn_id,item_id,rollout_ordinal,
      created_at_ms,item_json,item_type) VALUES(?,?,'FAKE_USER',1,1,?,'userMessage')''',
              (thread, turn, user_json))
    c.execute('''INSERT INTO thread_items(thread_id,turn_id,item_id,rollout_ordinal,
      created_at_ms,item_json,item_type) VALUES(?,?,?,2,2,?,'agentMessage')''',
              (thread, turn, item, canonical(final)))
    c.commit(); c.close()
    return folder, receipt, auth, final


def poll(f, r=None, a=None, thread='FAKE_THREAD'):
    return poll_once(f / 'source.sqlite', thread, f / 'journal.sqlite',
                     admission_receipts=None if r is None else [r], authorization=a)


def query(f, sql):
    c = connect_source(f / 'journal.sqlite')
    try: return c.execute(sql).fetchall()
    finally: c.close()


def modify(f, sql, params=()):
    c = sqlite3.connect(f / 'source.sqlite'); c.execute(sql, params); c.commit(); c.close()


@case('exact_started_receipt_and_nullable_digest_enable_fake_eligibility_only')
def _():
    f, r, a, final = fixture(text='[COMPLETE] literal must remain unchanged.\n')
    result = poll(f, r, a)
    check(result['fake_eligible_jobs'] == 1, 'exact Started receipt + SQL + current authorization matches')
    check(not result['egress_authorized'] and not result['source_payload_digest_verified'],
          'eligibility never claims real egress or input digest source verification')
    check(query(f, 'SELECT text,status FROM jobs')[0] == (final['text'], 'ELIGIBLE_FAKE_ONLY'),
          'full COMPLETE body retained without stripping')
    generation, receipt_hash, raw = query(f, 'SELECT generation_snapshot,original_receipt_sha256,latest_decision_json FROM admission_audit')[0]
    check(generation == 1 and receipt_hash is not None, 'first positive admission/generation bound transactionally')
    check(json.loads(raw)['eligible_fake_only'], 'journal contains matching fake decision')
    check(poll(f, r, a)['new_jobs'] == 0, 'same receipt rescan remains idempotent')


@case('scope_mutations_in_receipt_cannot_authorize_backend_final')
def _():
    mutations = {'thread_id': 'OTHER_THREAD', 'turn_id': 'OTHER_TURN',
                 'native_session_id': 'WRONG_NATIVE_SESSION',
                 'voice_session_generation': 2, 'origin_id': 'OTHER_ORIGIN',
                 'client_id': 'OTHER_CLIENT', 'queued_item_id': 'OTHER_QUEUE',
                 'receipt_id': 'OTHER_RECEIPT', 'input_digest': 'OTHER_DIGEST',
                 'schema': 'UNKNOWN_SCHEMA', 'attempt_id': 19}
    for field, value in mutations.items():
        f, r, a, _ = fixture(); r[field] = value
        result = poll(f, r, a)
        check(result['fake_eligible_jobs'] == 0, 'receipt mutation blocked: ' + field)
        check(query(f, 'SELECT status FROM jobs')[0][0] == 'HOLD_NC_PROVENANCE',
              'receipt mismatch holds intact captured text: ' + field)


@case('queued_claimed_ambiguous_rejected_cancelled_and_steered_never_authorize')
def _():
    for state in ('Queued', 'Claimed', 'Ambiguous', 'Rejected', 'Cancelled', 'Steered', None):
        f, r, a, _ = fixture(); r['admission_result'] = state
        check(poll(f, r, a)['fake_eligible_jobs'] == 0, 'only Started may satisfy admission: ' + str(state))
        check(query(f, 'SELECT generation_snapshot FROM admission_audit')[0][0] is None,
              'nonstarted receipt never binds an accepted generation')


@case('generation_type_and_authorization_scope_are_strict')
def _():
    variants = [{'voice_session_generation': True}, {'voice_session_generation': 0},
                {'voice_session_generation': '1'}, {'voice_session_generation': 2},
                {'state': 'Revoked'}, {'state': 'Unknown'}, {'thread_id': 'OTHER_THREAD'},
                {'client_id': 'OTHER_CLIENT'}, {'origin_id': 'OTHER_ORIGIN'},
                {'queued_item_id': 'OTHER_QUEUE'}, {'schema': 'UNKNOWN'},
                {'native_session_id': 'WRONG_NATIVE_SESSION'},
                {'native_session_id': ''}, {'job_generation': -1},
                {'job_generation': True}, {'job_generation': '0'},
                {'job_generation': 9007199254740992}]
    for changes in variants:
        f, r, a, _ = fixture(); a.update(changes)
        check(poll(f, r, a)['fake_eligible_jobs'] == 0, 'invalid or different current authorization blocked')
    f, r, a, _ = fixture(); r['voice_session_generation'] = True
    check(poll(f, r, a)['fake_eligible_jobs'] == 0, 'bool receipt generation is not integer one')


@case('missing_receipt_or_authorization_never_replays_new_unfrozen_turn')
def _():
    for missing in ('receipt', 'authorization', 'both'):
        f, r, a, _ = fixture()
        result = poll(f, None if missing != 'authorization' else r,
                      None if missing != 'receipt' else a)
        check(result['new_jobs'] == 1 and result['fake_eligible_jobs'] == 0,
              'new final captured with no automatic eligibility: ' + missing)
        check(query(f, 'SELECT status FROM jobs')[0][0] == 'HOLD_NC_PROVENANCE',
              'unfrozen does not mean authorized')


@case('first_user_exact_camel_clientid_and_pointer_are_required')
def _():
    options = [{'user_patch': {'clientId': 'OTHER_CLIENT'}},
               {'user_patch': {'clientId': None}}, {'user_patch': {'clientId': ''}},
               {'user_patch': {'clientId': 12}}, {'user_patch': {'id': 'OTHER_USER'}},
               {'user_patch': {'type': 'agentMessage'}}, {'snake_only': True},
               {'user_patch': {'client_id': 'opaque_origin_without_generation_syntax'}},
               {'duplicate_client': True}]
    for kwargs in options:
        f, r, a, _ = fixture(**kwargs)
        check(poll(f, r, a)['fake_eligible_jobs'] == 0, 'first user identity/schema mismatch blocked')
    f, r, a, _ = fixture()
    modify(f, "UPDATE thread_turns SET first_user_item_id='ABSENT_USER'")
    check(poll(f, r, a)['fake_eligible_jobs'] == 0, 'missing exact first-user join blocked')


@case('valid_receipt_cannot_override_final_id_type_phase_or_turn_join')
def _():
    for changes in ({'id': 'OTHER_ITEM'}, {'type': 'realtime'}, {'phase': 'commentary'},
                    {'delivery': 'async'}, {'questions': ['FAKE question']}):
        f, r, a, _ = fixture(final_patch=changes)
        result = poll(f, r, a)
        check(result['fake_eligible_jobs'] == 0 and result['journal_jobs'] == 0,
              'receipt cannot override backend final validation')
    f, r, a, _ = fixture()
    modify(f, "UPDATE thread_items SET turn_id='OTHER_TURN' WHERE item_id='FAKE_FINAL'")
    check(poll(f, r, a)['journal_jobs'] == 0, 'item in another turn cannot join final pointer')


@case('input_digest_is_nullable_present_and_opaque_exact_match_only')
def _():
    for receipt_digest, auth_digest, expected in ((None, None, 1), ('opaque digest', 'opaque digest', 1),
                                                 (None, 'opaque', 0), ('opaque', None, 0),
                                                 ('a', 'b', 0), (12, 12, 0)):
        f, r, a, _ = fixture(); r['input_digest'] = receipt_digest; a['input_digest'] = auth_digest
        result = poll(f, r, a)
        check(result['fake_eligible_jobs'] == expected, 'input digest null/opaque equality')
        check(not result['source_payload_digest_verified'], 'equality is not source payload verification')
    for missing in ('receipt', 'authorization'):
        f, r, a, _ = fixture(); del (r if missing == 'receipt' else a)['input_digest']
        check(poll(f, r, a)['fake_eligible_jobs'] == 0, 'absent digest differs from explicit null')


@case('duplicate_receipt_is_idempotent_conflicting_receipt_is_held')
def _():
    f, r, a, _ = fixture(); record = read_snapshot(f / 'source.sqlite', 'FAKE_THREAD')['records'][0]
    check(evaluate_final_admission(record, [r, copy.deepcopy(r)], a)['eligible_fake_only'],
          'identical duplicate receipt allowed')
    conflict = dict(r, turn_id='OTHER_TURN')
    check(not evaluate_final_admission(record, [r, conflict], a)['eligible_fake_only'],
          'conflicting duplicate queue receipt blocked')


@case('current_authorization_rechecked_and_job_generation_cannot_be_rebound')
def _():
    f, r, a, _ = fixture(); check(poll(f, r, a)['fake_eligible_jobs'] == 1, 'initial current generation eligible')
    revoked = dict(a, state='Revoked')
    check(poll(f, r, revoked)['fake_eligible_jobs'] == 0, 'revocation snapshot removes eligibility')
    check(query(f, 'SELECT generation_snapshot FROM admission_audit')[0][0] == 1,
          'generation snapshot retained across revocation')
    r2 = dict(r, voice_session_generation=2); a2 = dict(a, voice_session_generation=2)
    check(poll(f, r2, a2)['fake_eligible_jobs'] == 0, 'matching rewritten receipt/auth cannot rebind old job to new generation')
    raw = query(f, 'SELECT latest_decision_json FROM admission_audit')[0][0]
    check(json.loads(raw)['reason'] == 'JOB_GENERATION_SNAPSHOT_MISMATCH', 'journal explains generation rejection')


@case('committed_admission_receipt_cannot_change_silently')
def _():
    f, r, a, _ = fixture(); poll(f, r, a)
    changed = dict(r, attempt_id='OTHER_ATTEMPT')
    check(poll(f, changed, a)['fake_eligible_jobs'] == 0, 'changed admitted receipt cannot silently replace first proof')
    raw = query(f, 'SELECT latest_decision_json FROM admission_audit')[0][0]
    check(json.loads(raw)['reason'] == 'COMMITTED_ADMISSION_RECEIPT_CHANGED', 'receipt identity guard explains hold')


@case('all_93_historical_turns_hold_even_with_matching_started_authorization')
def _():
    check(len(frozen_turns()) == 93, 'exact frozen historical turn count')
    for thread, turn in frozen_turns():
        r, a = contracts(thread, turn)
        record = {'identity': (thread, turn, 'SYNTHETIC_REPLACEMENT_POINTER')}
        decision = evaluate_final_admission(record, [r], a)
        check(decision['status'] == 'HOLD_HISTORICAL' and not decision['eligible_fake_only'],
              'historical turn cannot be authorized even with replaced final pointer')
    thread, turn = sorted(frozen_turns())[0]
    f, r, a, original = fixture(thread=thread, turn=turn, text='FAKE HISTORICAL TEST BODY')
    check(poll(f, r, a, thread)['journal_status_counts'] == {'HOLD_HISTORICAL': 1},
          'real SQLite historic gate persisted')
    changed = dict(original, id='OTHER_FINAL', text='FAKE REPLACEMENT BODY')
    modify(f, '''INSERT INTO thread_items(thread_id,turn_id,item_id,rollout_ordinal,
      created_at_ms,item_json,item_type) VALUES(?,?,'OTHER_FINAL',3,3,?,'agentMessage')''',
           (thread, turn, canonical(changed)))
    modify(f, "UPDATE thread_turns SET final_agent_item_id='OTHER_FINAL'")
    result = poll(f, r, a, thread)
    check(result['journal_status_counts'] == {'HOLD_HISTORICAL': 2}, 'old and changed pointers permanently historical')
    check(query(f, "SELECT text FROM jobs WHERE item_id='FAKE_FINAL'")[0][0] == original['text'],
          'historical original body remains untouched in synthetic journal')


@case('frozen_ledger_tampering_is_stop_not_fallback')
def _():
    raw = (ROOT / 'historical_freeze.json').read_bytes()
    try: parse_freeze(raw + b' ')
    except ValueError as exc: check(str(exc) == 'HISTORICAL_FREEZE_SHA_MISMATCH_STOP', 'exact freeze hash enforced')
    else: check(False, 'tampered historical ledger accepted')


@case('source_change_and_disappearance_override_prior_eligibility_and_audit')
def _():
    f, r, a, original = fixture(); poll(f, r, a)
    changed = dict(original, text='FAKE CHANGED SOURCE')
    modify(f, "UPDATE thread_items SET item_json=? WHERE item_id='FAKE_FINAL'", (canonical(changed),))
    result = poll(f, r, a)
    check(result['fake_eligible_jobs'] == 0 and result['journal_status_counts'] == {'HOLD_SOURCE_CONFLICT': 1},
          'source conflict defeats valid receipt')
    check(query(f, 'SELECT text FROM jobs')[0][0] == original['text'], 'original source text preserved')
    modify(f, "UPDATE thread_turns SET status='interrupted'")
    check(poll(f, r, a)['fake_eligible_jobs'] == 0, 'missing current source cannot retain eligibility')
    check(not json.loads(query(f, 'SELECT latest_decision_json FROM admission_audit')[0][0])['eligible_fake_only'],
          'audit cannot advertise stale eligibility after source disappears')



@case('native_session_required_and_wrong_session_holds_same_thread_and_generation')
def _():
    for side in ('receipt', 'authorization'):
        for value in (None, '', 'WRONG_NATIVE_SESSION'):
            f, r, a, _ = fixture()
            (r if side == 'receipt' else a)['native_session_id'] = value
            check(poll(f, r, a)['fake_eligible_jobs'] == 0,
                  'same thread/generation wrong or invalid native session holds')
        f, r, a, _ = fixture(); del (r if side == 'receipt' else a)['native_session_id']
        check(poll(f, r, a)['fake_eligible_jobs'] == 0, 'missing native binding is not wildcard')
    f, r, a, _ = fixture(); poll(f, r, a)
    r2 = dict(r, native_session_id='SUCCESSOR_NATIVE'); a2 = dict(a, native_session_id='SUCCESSOR_NATIVE')
    check(poll(f, r2, a2)['fake_eligible_jobs'] == 0,
          'matching rewritten receipt/auth cannot rebind old final into new native session')
    decision = json.loads(query(f, 'SELECT latest_decision_json FROM admission_audit')[0][0])
    check(decision['reason'] == 'JOB_NATIVE_SESSION_SNAPSHOT_MISMATCH', 'explicit native snapshot guard')


@case('job_generation_is_persisted_and_cannot_be_refreshed_after_speech_onset')
def _():
    f, r, a, _ = fixture(); check(poll(f, r, a)['fake_eligible_jobs'] == 1, 'initial generation accepted')
    check(query(f, 'SELECT native_session_snapshot,job_generation_snapshot FROM admission_audit')[0]
          == ('FAKE_NATIVE_SESSION', 0), 'native and job generation stored in transaction')
    advanced = dict(a, job_generation=1)
    check(poll(f, r, advanced)['fake_eligible_jobs'] == 0, 'new boundary generation cannot reauthorize old final')
    decision = json.loads(query(f, 'SELECT latest_decision_json FROM admission_audit')[0][0])
    check(decision['reason'] == 'JOB_PLAYBACK_GENERATION_SNAPSHOT_MISMATCH', 'job generation rejection explicit')
    check(query(f, 'SELECT job_generation_snapshot FROM admission_audit')[0][0] == 0,
          'first job generation remains immutable')
    f, r, a, _ = fixture(); del a['job_generation']
    check(poll(f, r, a)['fake_eligible_jobs'] == 0, 'absence of job generation is not wildcard')


@case('playback_packet_has_exact_join_and_is_provenance_only_not_audio_permission')
def _():
    f, r, a, final = fixture(); poll(f, r, a)
    packet = read_playback_evidence(f / 'source.sqlite', 'FAKE_THREAD', f / 'journal.sqlite',
                                   ('FAKE_THREAD', 'FAKE_TURN', 'FAKE_FINAL'),
                                   admission_receipts=[r], authorization=a)
    check(packet['identity'] == {
        'thread_id': 'FAKE_THREAD', 'native_session_id': 'FAKE_NATIVE_SESSION',
        'voice_session_generation': 1, 'origin_id': r['origin_id'], 'client_id': r['client_id'],
        'queued_item_id': r['queued_item_id'], 'turn_id': 'FAKE_TURN',
        'final_agent_item_id': 'FAKE_FINAL', 'first_user_item_id': 'FAKE_USER', 'job_generation': 0,
    }, 'packet binds each required identity')
    check(packet['provenance_only'] and not packet['egress_authorized'], 'packet is never permission to play')
    check(packet['journal']['job_generation'] == 0 and packet['journal']['status'] == 'ELIGIBLE_FAKE_ONLY',
          'packet carries immutable journal rather than caller refreshed generation')
    check(packet['source']['first_user']['client_id'] == r['client_id'], 'first user proof included')
    check(packet['source']['final_item']['text'] == final['text'], 'text retained without heuristic stripping')
    check(packet['receipt_sha256'] == sha(canonical(r)) and packet['authorization_sha256'] == sha(canonical(a)),
          'snapshot canonical hashes match exact packets')
    r['native_session_id'] = 'MUTATED_AFTER_PACKET'; a['job_generation'] = 1
    check(packet['admission_receipt']['native_session_id'] == 'FAKE_NATIVE_SESSION'
          and packet['authorization']['job_generation'] == 0, 'packet detached from later caller mutation')
    write_receipt(RUN / 'PLAYBACK-EVIDENCE-FAKE.json', packet)


@case('playback_reader_refuses_new_generation_source_conflicts_and_unpolled_final')
def _():
    f, r, a, final = fixture(); poll(f, r, a)
    def read(auth):
        return read_playback_evidence(f / 'source.sqlite', 'FAKE_THREAD', f / 'journal.sqlite',
                                     ('FAKE_THREAD', 'FAKE_TURN', 'FAKE_FINAL'),
                                     admission_receipts=[r], authorization=auth)
    for changed_auth in (dict(a, job_generation=1), dict(a, native_session_id='OTHER_NATIVE'),
                         dict(a, state='Revoked')):
        try: read(changed_auth)
        except ValueError: check(True, 'current read refuses invalid authorization even before next poll')
        else: check(False, 'invalid current authorization passed packet builder')
    changed = dict(final, text='FAKE CHANGED AFTER LAST POLL')
    modify(f, "UPDATE thread_items SET item_json=? WHERE item_id='FAKE_FINAL'", (canonical(changed),))
    try: read(a)
    except ValueError: check(True, 'current source changed after poll invalidates packet')
    else: check(False, 'stale journal falsely authorized changed source')
    poll(f, r, a)
    modify(f, "UPDATE thread_items SET item_json=? WHERE item_id='FAKE_FINAL'", (canonical(final),))
    try: read(a)
    except ValueError: check(True, 'restored text does not erase journal HOLD_SOURCE_CONFLICT')
    else: check(False, 'original text restoration bypassed persistent conflict')



@case('missing_optional_questions_still_builds_packet_without_relaxing_delivery')
def _():
    f, r, a, final = fixture()
    final.pop('questions')
    modify(f, "UPDATE thread_items SET item_json=? WHERE item_id='FAKE_FINAL'", (canonical(final),))
    check(poll(f, r, a)['fake_eligible_jobs'] == 1, 'schema permits absent optional questions')
    packet = read_playback_evidence(f / 'source.sqlite', 'FAKE_THREAD', f / 'journal.sqlite',
                                   ('FAKE_THREAD', 'FAKE_TURN', 'FAKE_FINAL'),
                                   admission_receipts=[r], authorization=a)
    check(packet['source']['final_item']['questions'] is None, 'absence explicitly projected as nullable questions')
    check(packet['source']['final_item']['delivery'] is None, 'mandatory delivery remains present')
    f2, r2, a2, final2 = fixture(); final2.pop('delivery')
    modify(f2, "UPDATE thread_items SET item_json=? WHERE item_id='FAKE_FINAL'", (canonical(final2),))
    check(poll(f2, r2, a2)['fake_eligible_jobs'] == 0, 'missing mandatory delivery still holds')


def main():
    failed = sum(x['result'] == 'FAIL' for x in RESULTS)
    result = {'schema': 'fdv.producer.admission.tests.v1',
              'result': 'PASS' if RESULTS and ASSERTIONS and not failed else 'FAIL',
              'assertions': ASSERTIONS, 'scenario_count': len(RESULTS), 'failed_scenarios': failed,
              'source_fixture': 'REAL_SQLITE_REAL_DDL_SYNTHETIC_BODIES',
              'historical_identifiers': '93_REAL_FROZEN_TURNS_HASHES_ONLY_NO_PRIVATE_TEXT',
              'receipt_and_authorization': 'PROPOSED_CONTRACT_SYNTHETIC_FIXTURES',
              'eligibility_scope': 'FAKE_ONLY_NO_REAL_EGRESS', 'windows_assertions': 0,
              'source_payload_digest_verified': False, 'voice_tts_audio_network_rpc': False,
              'code_sha256': {name: sha((ROOT / name).read_bytes()) for name in
                              ('final_producer.py', 'admission_gate.py', 'test_admission.py',
                               'historical_freeze.json', 'final_producer_base.py')},
              'cases': RESULTS}
    write_receipt(RUN / 'TEST-RECEIPT.json', result)
    print(canonical({'result': result['result'], 'assertions': ASSERTIONS,
                     'scenarios': len(RESULTS), 'failed': failed,
                     'receipt': str(RUN / 'TEST-RECEIPT.json'),
                     'receipt_sha256': sha((RUN / 'TEST-RECEIPT.json').read_bytes())}))
    return 0 if result['result'] == 'PASS' else 1


if __name__ == '__main__':
    raise SystemExit(main())

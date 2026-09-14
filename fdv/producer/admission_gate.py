"""Receipt/SQL eligibility gate for the OFFLINE candidate, never real egress.

Authorization and receipt inputs are trusted local contract snapshots supplied
by the caller; this module does not authenticate a producer or a live session.
Playback must recheck current authorization at its own boundary.
"""
from functools import lru_cache
import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent
FREEZE_SHA256 = '7a7c48d0c9d67b4f1e281306264b40e240d7c51e380a5cff6ec0aaf0df624bcb'
MATCH_FIELDS = ('thread_id', 'voice_session_generation', 'origin_id',
                'client_id', 'queued_item_id', 'input_digest')
RECEIPT_REQUIRED = ('schema', 'receipt_id', *MATCH_FIELDS,
                    'turn_id', 'admission_result')


def digest(value):
    raw = json.dumps(value, ensure_ascii=False, sort_keys=True,
                     separators=(',', ':')).encode('utf-8')
    return hashlib.sha256(raw).hexdigest()


def nonempty(value):
    return isinstance(value, str) and bool(value.strip())


def parse_freeze(raw):
    if hashlib.sha256(raw).hexdigest() != FREEZE_SHA256:
        raise ValueError('HISTORICAL_FREEZE_SHA_MISMATCH_STOP')
    data = json.loads(raw)
    if data['schema'] != 'fdv.historical.freeze.v1' or data['count'] != 93:
        raise ValueError('HISTORICAL_FREEZE_CONTRACT_STOP')
    records = data['records']
    keys = {(r['thread_id'], r['turn_id']) for r in records}
    if len(records) != 93 or len(keys) != 93:
        raise ValueError('HISTORICAL_FREEZE_IDENTITIES_STOP')
    return frozenset(keys)


@lru_cache(maxsize=1)
def frozen_turns():
    return parse_freeze((ROOT / 'historical_freeze.json').read_bytes())


def is_frozen(thread_id, turn_id):
    return (thread_id, turn_id) in frozen_turns()


def verdict(status, reason, receipt=None, authorization=None):
    return {'status': status, 'reason': reason,
            'eligible_fake_only': status == 'ELIGIBLE_FAKE_ONLY',
            'egress_authorized': False,
            'source_payload_digest_verified': False,
            'receipt_sha256': digest(receipt) if receipt is not None else None,
            'authorization_sha256': digest(authorization) if authorization is not None else None,
            'voice_session_generation': receipt.get('voice_session_generation') if receipt else None,
            'origin_id': receipt.get('origin_id') if receipt else None,
            'queued_item_id': receipt.get('queued_item_id') if receipt else None}


def valid_generation(value):
    return type(value) is int and value >= 1


def evaluate_final_admission(record, receipts, authorization):
    """Return fake eligibility only; use records from final_producer.read_snapshot.

    authorization.schema='fdv.voice.authorization.v1', state='Active'; its
    voice_session_generation is the caller's CURRENT session snapshot. Optional
    record.job_voice_session_generation is the immutable first eligibility
    generation already captured in the candidate job journal.

    receipts is a snapshot of latest receipts, not an append-only state history.
    Identical duplicates are idempotent; conflicting receipts for one queue ID
    hold. input_digest is required but nullable; null matches only present null.
    """
    if not isinstance(record, dict):
        return verdict('HOLD_NC_PROVENANCE', 'INVALID_SOURCE_RECORD')
    identity = record.get('identity')
    if (not isinstance(identity, (tuple, list)) or len(identity) != 3
            or not all(nonempty(x) for x in identity)):
        return verdict('HOLD_NC_PROVENANCE', 'INVALID_SOURCE_IDENTITY')
    thread, turn, item_id = identity
    if is_frozen(thread, turn):
        return verdict('HOLD_HISTORICAL', 'FROZEN_HISTORICAL_TURN_NEVER_ELIGIBLE')
    if (record.get('classification') != 'BACKEND_FINAL_PROVENANCE_NC'
            or record.get('final_pointer') != item_id
            or record.get('final_item_id') != item_id):
        return verdict('HOLD_NC_PROVENANCE', 'BACKEND_FINAL_JOIN_NOT_VERIFIED')
    first = record.get('first_user')
    if (not isinstance(first, dict) or not first.get('valid')
            or first.get('type') != 'userMessage'
            or first.get('id') != record.get('first_user_item_id')
            or not nonempty(first.get('client_id'))):
        return verdict('HOLD_NC_PROVENANCE', 'FIRST_USER_CLIENT_ID_NOT_VERIFIED')
    if not isinstance(authorization, dict):
        return verdict('HOLD_NC_PROVENANCE', 'NO_CURRENT_AUTHORIZATION')
    if (authorization.get('schema') != 'fdv.voice.authorization.v1'
            or authorization.get('state') != 'Active'
            or any(k not in authorization for k in MATCH_FIELDS)
            or not valid_generation(authorization.get('voice_session_generation'))
            or any(not nonempty(authorization.get(k)) for k in
                   ('thread_id', 'origin_id', 'client_id', 'queued_item_id'))
            or authorization['client_id'] != authorization['origin_id']
            or (authorization['input_digest'] is not None
                and not nonempty(authorization['input_digest']))):
        return verdict('HOLD_NC_PROVENANCE', 'AUTHORIZATION_INACTIVE_OR_INVALID')
    if authorization['thread_id'] != thread:
        return verdict('HOLD_NC_PROVENANCE', 'AUTHORIZATION_THREAD_MISMATCH')
    job_generation = record.get('job_voice_session_generation')
    if job_generation is not None and (
            not valid_generation(job_generation)
            or job_generation != authorization['voice_session_generation']):
        return verdict('HOLD_NC_PROVENANCE', 'JOB_GENERATION_SNAPSHOT_MISMATCH')
    if not isinstance(receipts, (list, tuple)):
        return verdict('HOLD_NC_PROVENANCE', 'RECEIPT_SNAPSHOT_REQUIRED')
    relevant = [r for r in receipts if isinstance(r, dict)
                and r.get('queued_item_id') == authorization['queued_item_id']]
    if not relevant:
        return verdict('HOLD_NC_PROVENANCE', 'NO_MATCHING_QUEUE_RECEIPT')
    if len({digest(r) for r in relevant}) != 1:
        return verdict('HOLD_NC_PROVENANCE', 'CONFLICTING_RECEIPT_SNAPSHOT')
    receipt = relevant[0]
    if (any(k not in receipt for k in RECEIPT_REQUIRED)
            or receipt.get('schema') != 'fdv.voice.admission.v1'
            or receipt.get('receipt_id') != receipt.get('queued_item_id')
            or not valid_generation(receipt.get('voice_session_generation'))
            or any(not nonempty(receipt.get(k)) for k in
                   ('receipt_id', 'thread_id', 'origin_id', 'client_id', 'queued_item_id'))
            or receipt.get('client_id') != receipt.get('origin_id')
            or any(receipt.get(k) is not None and not nonempty(receipt[k])
                   for k in ('handoff_id', 'item_id', 'attempt_id'))
            or (receipt.get('input_digest') is not None
                and not nonempty(receipt.get('input_digest')))):
        return verdict('HOLD_NC_PROVENANCE', 'INVALID_ADMISSION_RECEIPT')
    if receipt['admission_result'] != 'Started':
        return verdict('HOLD_NC_PROVENANCE', 'ADMISSION_NOT_STARTED')
    if any(receipt[k] != authorization[k] for k in MATCH_FIELDS):
        return verdict('HOLD_NC_PROVENANCE', 'RECEIPT_AUTHORIZATION_SCOPE_MISMATCH')
    if receipt['turn_id'] != turn or not nonempty(receipt['turn_id']):
        return verdict('HOLD_NC_PROVENANCE', 'ADMITTED_TURN_MISMATCH')
    if first['client_id'] != receipt['client_id']:
        return verdict('HOLD_NC_PROVENANCE', 'FIRST_USER_CLIENT_ID_MISMATCH')
    previous_receipt = record.get('job_receipt_sha256')
    if previous_receipt is not None and previous_receipt != digest(receipt):
        return verdict('HOLD_NC_PROVENANCE', 'COMMITTED_ADMISSION_RECEIPT_CHANGED')
    return verdict('ELIGIBLE_FAKE_ONLY', 'EXACT_RECEIPT_AUTHORIZATION_AND_SQL_JOIN',
                   receipt, authorization)

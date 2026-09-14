// Structural verifier for a trusted producer readback, not an authentication layer.
// It cannot release audio alone; live session and local fences belong to index.mjs.
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';

const FREEZE_SHA = '7a7c48d0c9d67b4f1e281306264b40e240d7c51e380a5cff6ec0aaf0df624bcb';
const hash = value => createHash('sha256').update(value).digest('hex');
const rawFreeze = readFileSync(new URL('../producer/historical_freeze.json', import.meta.url));
if (hash(rawFreeze) !== FREEZE_SHA) throw new Error('HISTORICAL_FREEZE_SHA_MISMATCH_STOP');
const freeze = JSON.parse(rawFreeze);
const frozen = new Set(freeze.records.map(x => JSON.stringify([x.thread_id, x.turn_id])));
if (freeze.schema !== 'fdv.historical.freeze.v1' || freeze.count !== 93 || frozen.size !== 93 || freeze.records.length !== 93) throw new Error('HISTORICAL_FREEZE_INVALID_STOP');

function canonical(value) {
  if (value === null || typeof value === 'string' || typeof value === 'boolean') return JSON.stringify(value);
  if (typeof value === 'number' && Number.isSafeInteger(value)) return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonical).join(',')}]`;
  if (value && Object.getPrototypeOf(value) === Object.prototype) {
    return `{${Object.keys(value).sort().map(k => `${JSON.stringify(k)}:${canonical(value[k])}`).join(',')}}`;
  }
  throw new Error('NON_CANONICAL_PROOF');
}
const nonempty = x => typeof x === 'string' && x.trim().length > 0;
const sha = x => typeof x === 'string' && /^[a-f0-9]{64}$/.test(x);
const integer = (x, minimum) => Number.isSafeInteger(x) && x >= minimum;
function requireThat(condition, reason) { if (!condition) throw new Error(reason); }

export function verifyPlaybackEvidence(evidence) {
  requireThat(evidence?.schema === 'fdv.voice.playback.evidence.v1'
    && evidence.provenance_only === true && evidence.egress_authorized === false, 'PRODUCER_EVIDENCE_REQUIRED');
  const p = evidence.identity, s = evidence.source, r = evidence.admission_receipt;
  const a = evidence.authorization, j = evidence.journal;
  requireThat(p && s && r && a && j, 'PROOF_COMPONENT_MISSING');
  for (const field of ['thread_id', 'native_session_id', 'origin_id', 'client_id', 'queued_item_id', 'turn_id', 'final_agent_item_id', 'first_user_item_id']) {
    requireThat(nonempty(p[field]), `INVALID_${field}`);
  }
  requireThat(!frozen.has(JSON.stringify([p.thread_id, p.turn_id])), 'HISTORICAL_TURN_PERMANENT_HOLD');
  requireThat(integer(p.voice_session_generation, 1) && integer(p.job_generation, 0), 'INVALID_GENERATION');
  requireThat(r.schema === 'fdv.voice.admission.v1' && r.admission_result === 'Started'
    && r.receipt_id === p.queued_item_id && r.turn_id === p.turn_id, 'ADMISSION_RECEIPT_NOT_STARTED_OR_WRONG_TURN');
  requireThat(a.schema === 'fdv.voice.authorization.v1' && a.state === 'Active', 'AUTHORIZATION_NOT_ACTIVE');
  for (const field of ['thread_id', 'native_session_id', 'voice_session_generation', 'origin_id', 'client_id', 'queued_item_id']) {
    requireThat(r[field] === p[field] && a[field] === p[field], `IDENTITY_MISMATCH_${field}`);
  }
  requireThat(p.client_id === p.origin_id && a.job_generation === p.job_generation, 'CLIENT_OR_JOB_GENERATION_MISMATCH');
  requireThat(Object.hasOwn(r, 'input_digest') && Object.hasOwn(a, 'input_digest')
    && r.input_digest === a.input_digest && (r.input_digest === null || nonempty(r.input_digest)), 'INPUT_DIGEST_MISMATCH');
  for (const field of ['handoff_id', 'item_id', 'attempt_id']) {
    requireThat(r[field] === undefined || r[field] === null || nonempty(r[field]), 'INVALID_OPTIONAL_RECEIPT_ID');
  }
  requireThat(sha(evidence.receipt_sha256) && hash(canonical(r)) === evidence.receipt_sha256, 'RECEIPT_SNAPSHOT_CHANGED');
  requireThat(sha(evidence.authorization_sha256) && hash(canonical(a)) === evidence.authorization_sha256, 'AUTHORIZATION_SNAPSHOT_CHANGED');
  requireThat(j.status === 'ELIGIBLE_FAKE_ONLY' && j.original_receipt_sha256 === evidence.receipt_sha256, 'JOURNAL_NOT_ELIGIBLE_OR_RECEIPT_CHANGED');
  for (const field of ['native_session_id', 'voice_session_generation', 'job_generation']) {
    requireThat(j[field] === p[field], `IMMUTABLE_JOURNAL_MISMATCH_${field}`);
  }
  requireThat(sha(s.version_sha256) && j.original_version_sha256 === s.version_sha256
    && sha(s.text_sha256) && j.text_sha256 === s.text_sha256, 'JOURNAL_SOURCE_VERSION_MISMATCH');
  requireThat(s.classification === 'BACKEND_FINAL_PROVENANCE_NC' && s.turn_status === 'completed'
    && s.final_pointer === p.final_agent_item_id && s.final_item_id === p.final_agent_item_id, 'FINAL_POINTER_MISMATCH');
  const u = s.first_user, final = s.final_item;
  requireThat(s.first_user_item_id === p.first_user_item_id && u?.valid === true && u.type === 'userMessage'
    && u.id === p.first_user_item_id && u.client_id === p.client_id, 'FIRST_USER_CLIENT_ID_MISMATCH');
  requireThat(final?.type === 'agentMessage' && final.id === p.final_agent_item_id
    && final.phase === 'final_answer' && final.delivery === null
    && (final.questions === null || (Array.isArray(final.questions) && final.questions.length === 0))
    && nonempty(final.text) && hash(final.text) === s.text_sha256, 'BACKEND_FINAL_CONTENT_INVALID');
  return Object.freeze({ ...p, text_sha256: s.text_sha256, receipt_sha256: evidence.receipt_sha256 });
}

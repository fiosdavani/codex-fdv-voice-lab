// Offline candidate. This module does not discover or connect to a Desktop host.
import { randomUUID } from 'node:crypto';

export const CANDIDATE_VERSION = 'FDV_NATIVE_VOICE_BOUNDARY_V1';
const SCOPE_KEYS = ['threadId', 'nativeSessionId', 'voiceGeneration', 'ownerId'];
const METHODS = ['setOutputMuted', 'playNativeOutput', 'invalidateJobs', 'stopPlayer', 'teardownVoice', 'reconcilePresentation'];
const INDEPENDENT_COMPOSER_BLOCKERS = ['backendBusy', 'permissionPrompt', 'threadReadOnly'];

function scopeOf(value) {
  if (!value || typeof value !== 'object') throw new TypeError('scope required');
  for (const key of ['threadId', 'nativeSessionId', 'ownerId']) {
    if (typeof value[key] !== 'string' || !value[key] || value[key].length > 256) throw new TypeError(`invalid ${key}`);
  }
  if (!Number.isSafeInteger(value.voiceGeneration) || value.voiceGeneration < 1) throw new TypeError('invalid voiceGeneration');
  return Object.freeze(Object.fromEntries(SCOPE_KEYS.map(key => [key, value[key]])));
}

function sameScope(a, b) {
  return Boolean(a && b && SCOPE_KEYS.every(key => a[key] === b[key]));
}

function id(value, field) {
  if (typeof value !== 'string' || !value || value.length > 256) throw new TypeError(`invalid ${field}`);
  return value;
}

function copy(value) { return JSON.parse(JSON.stringify(value)); }

/** Explicit naming adapter for the producer; it grants no final/utterance provenance. */
export function toProducerScope(scope) {
  const s = scopeOf(scope);
  return Object.freeze({
    thread_id: s.threadId,
    native_session_id: s.nativeSessionId,
    voice_session_generation: s.voiceGeneration,
    owner_id: s.ownerId,
  });
}

export function createNativeVoiceBoundary({ adapter, timeoutMs = 1000 } = {}) {
  if (!adapter || !['FAKE', 'NATIVE_PRIVATE_SEAM'].includes(adapter.kind)) throw new TypeError('explicit adapter kind required');
  for (const method of METHODS) if (typeof adapter[method] !== 'function') throw new TypeError(`missing adapter.${method}`);
  if (!Number.isFinite(timeoutMs) || timeoutMs <= 0 || timeoutMs > 60000) throw new TypeError('invalid timeoutMs');
  const current = new Map();
  const records = new Map();
  const journal = [];
  const keyOf = scope => JSON.stringify(SCOPE_KEYS.map(k => scope[k]));

  function emit(record, type, fields = {}) {
    journal.push(Object.freeze({
      sequence: journal.length + 1,
      type,
      scope: record.scope,
      jobGeneration: record.jobGeneration,
      adapterKind: adapter.kind,
      ...fields,
    }));
  }

  function active(record) { return current.get(record.scope.threadId) === record; }
  function result(record, status, fields = {}) { return Object.freeze({ status, scope: record.scope, jobGeneration: record.jobGeneration, ...fields }); }
  function find(scope) { return records.get(keyOf(scope)); }
  function hold(record, reason) {
    if (record.state !== 'CLOSED') record.state = 'HOLD';
    record.mutedAck = null;
    record.errors.push(reason);
    emit(record, 'HOLD', { reason });
  }

  async function request(record, method, expectedType, fields = {}, checks = {}) {
    const operationId = randomUUID();
    const req = Object.freeze({ scope: record.scope, operationId, ...fields });
    let timer;
    try {
      const response = await Promise.race([
        Promise.resolve().then(() => adapter[method](req)),
        new Promise((_, reject) => { timer = setTimeout(() => reject(new Error(`${method}:TIMEOUT`)), timeoutMs); }),
      ]);
      if (!response || response.type !== expectedType || response.operationId !== operationId || !sameScope(response.scope, record.scope)) throw new Error(`${method}:INVALID_ACK_IDENTITY`);
      for (const [field, expected] of Object.entries(checks)) if (response[field] !== expected) throw new Error(`${method}:INVALID_ACK_${field}`);
      return Object.freeze({ ...copy(response), scope: record.scope });
    } finally { clearTimeout(timer); }
  }

  function beginSession(value) {
    const scope = scopeOf(value);
    const existing = find(scope);
    if (existing) return active(existing) && ['MUTING', 'READY_MUTED'].includes(existing.state)
      ? existing.beginPromise
      : Promise.resolve(result(existing, 'REJECTED_RETIRED_OR_HELD'));
    const previous = current.get(scope.threadId);
    if (previous && scope.voiceGeneration <= previous.scope.voiceGeneration) return Promise.resolve({ status: 'REJECTED_GENERATION', scope });
    if (previous && previous.state !== 'CLOSED') return Promise.resolve({ status: 'REJECTED_PREVIOUS_NOT_CLOSED', scope });
    const record = { scope, state: 'MUTING', mutedAck: null, jobGeneration: 0, pendingOnsets: 0, seenOnsets: new Map(), errors: [], composer: null, closePromise: null, beginPromise: null, pendingPlay: null };
    records.set(keyOf(scope), record);
    current.set(scope.threadId, record);
    emit(record, 'VOICE_BEGIN');
    record.beginPromise = (async () => {
      try {
        const ack = await request(record, 'setOutputMuted', 'OUTPUT_MUTED_ACK', { muted: true }, { muted: true, observation: 'SINK_READBACK' });
        if (!active(record) || record.state !== 'MUTING') return result(record, 'CANCELLED');
        record.mutedAck = ack;
        record.state = 'READY_MUTED';
        emit(record, 'OUTPUT_MUTED_ACK', { operationId: ack.operationId, muted: true });
        return result(record, 'READY_MUTED', { acknowledgement: ack });
      } catch (error) {
        // A cancelled preparation cannot re-open or corrupt a completed close.
        if (active(record) && record.state === 'MUTING') hold(record, String(error.message));
        return result(record, record.state === 'CLOSED' || record.state === 'CLOSING' ? 'CANCELLED' : 'HOLD');
      }
    })();
    return record.beginPromise;
  }

  function gateVaiJob({ scope: value, jobGeneration }) {
    const scope = scopeOf(value), record = find(scope);
    if (!record || !active(record)) return { allowed: false, reason: 'STALE_OR_UNKNOWN_SCOPE' };
    if (record.state !== 'READY_MUTED' || !record.mutedAck) return { allowed: false, reason: 'OUTPUT_NOT_CONFIRMED_MUTED' };
    if (record.pendingOnsets !== 0) return { allowed: false, reason: 'PRIOR_PLAYBACK_STOP_UNCONFIRMED' };
    if (!Number.isSafeInteger(jobGeneration) || jobGeneration !== record.jobGeneration) return { allowed: false, reason: 'STALE_JOB_GENERATION' };
    // Necessary playback fence only. Producer admission/final proof remains mandatory.
    return { allowed: true, reason: 'BOUNDARY_FENCE_ONLY', finalProvenanceAuthorized: false, scope: record.scope, jobGeneration };
  }

  async function playNativeOutput(value) {
    const scope = scopeOf(value), record = find(scope);
    if (!record || !active(record) || record.state !== 'READY_MUTED' || !record.mutedAck) return { status: 'REJECTED_NOT_MUTED', scope };
    if (record.pendingPlay) return record.pendingPlay;
    const fence = record.jobGeneration;
    record.pendingPlay = (async () => {
      try {
        const ack = await request(record, 'playNativeOutput', 'NATIVE_OUTPUT_PLAY_ACK', { requiredMuted: true, mutedOperationId: record.mutedAck.operationId }, { muted: true });
        if (!active(record) || record.state !== 'READY_MUTED' || fence !== record.jobGeneration) return result(record, 'STALE_ACK');
        emit(record, 'NATIVE_OUTPUT_PLAY_ACK', { operationId: ack.operationId, muted: true });
        return result(record, 'PLAYED_MUTED');
      } catch (error) {
        if (active(record) && record.state === 'READY_MUTED') hold(record, String(error.message));
        return result(record, 'HOLD');
      } finally { record.pendingPlay = null; }
    })();
    return record.pendingPlay;
  }

  async function stopAndInvalidate(record, reason) {
    const generation = record.jobGeneration;
    // Logical fence was already advanced synchronously. Stop need not wait for downstream journal I/O.
    const outcomes = await Promise.allSettled([
      request(record, 'invalidateJobs', 'JOBS_INVALIDATED_ACK', { nextJobGeneration: generation, reason }, { jobGeneration: generation }),
      request(record, 'stopPlayer', 'PLAYER_STOP_ACK', { nextJobGeneration: generation, reason }, { stopped: true, jobGeneration: generation }),
    ]);
    const failures = outcomes.filter(x => x.status === 'rejected').map(x => String(x.reason?.message ?? x.reason));
    if (failures.length) throw new Error(failures.join('|'));
  }

  function speechOnset({ scope: value, eventId, origin }) {
    const scope = scopeOf(value); id(eventId, 'eventId');
    const permitted = adapter.kind === 'FAKE' ? ['FAKE_FIXTURE'] : ['NATIVE_VAD_V2', 'NATIVE_VAD_V3'];
    if (!permitted.includes(origin)) throw new TypeError('onset origin not allowed for adapter');
    const record = find(scope);
    if (!record || !active(record) || !['MUTING', 'READY_MUTED'].includes(record.state)) return Promise.resolve({ status: 'IGNORED_STALE_OR_INACTIVE', scope });
    if (record.seenOnsets.has(eventId)) return record.seenOnsets.get(eventId);
    record.jobGeneration += 1;
    const onsetGeneration = record.jobGeneration;
    record.pendingOnsets += 1;
    emit(record, 'SPEECH_ONSET', { eventId, origin });
    const receipt = (async () => {
      try {
        await stopAndInvalidate(record, 'SPEECH_ONSET');
        return result(record, record.state === 'HOLD' ? 'HOLD' : 'ONSET_HANDLED', { eventId, origin, jobGeneration: onsetGeneration });
      } catch (error) {
        if (active(record) && record.state !== 'CLOSED') hold(record, String(error.message));
        return result(record, 'HOLD', { eventId, origin, jobGeneration: onsetGeneration });
      } finally { record.pendingOnsets -= 1; }
    })();
    record.seenOnsets.set(eventId, receipt);
    return receipt;
  }

  function closeVoice(value) {
    const scope = scopeOf(value), record = find(scope);
    if (!record || !active(record)) return Promise.resolve({ status: 'IGNORED_STALE_OR_UNKNOWN', scope });
    if (record.closePromise) return record.closePromise;
    record.state = 'CLOSING';
    record.mutedAck = null;
    record.jobGeneration += 1;
    emit(record, 'VOICE_CLOSE');
    record.closePromise = (async () => {
      const failures = [];
      try { await stopAndInvalidate(record, 'VOICE_CLOSE'); } catch (error) { failures.push(String(error.message)); }
      // Still retire the Voice generation after a player failure; never declare completion from partial acks.
      try {
        await request(record, 'teardownVoice', 'VOICE_TEARDOWN_ACK', {}, { transportRetired: true, voiceOwnerReleased: true });
      } catch (error) { failures.push(String(error.message)); }
      if (failures.length === 0) {
        try {
          const ack = await request(record, 'reconcilePresentation', 'PRESENTATION_ACK', {}, { presentationDetached: true, threadPreserved: true, backendSubscriptionsPreserved: true });
          if (typeof ack.composerUsable !== 'boolean' || !Array.isArray(ack.independentBlockers)) throw new Error('reconcilePresentation:COMPOSER_STATE_MISSING');
          if (ack.independentBlockers.some(blocker => !INDEPENDENT_COMPOSER_BLOCKERS.includes(blocker)) || new Set(ack.independentBlockers).size !== ack.independentBlockers.length) throw new Error('reconcilePresentation:INVALID_INDEPENDENT_BLOCKER');
          const composer = Object.freeze({
            composerUsable: ack.composerUsable,
            independentBlockers: Object.freeze([...ack.independentBlockers]),
            composerReady: ack.composerUsable && ack.independentBlockers.length === 0,
          });
          record.composer = composer;
          emit(record, 'COMPOSER_OBSERVED', { operationId: ack.operationId, ...composer });
          if (!ack.composerUsable && ack.independentBlockers.length === 0) throw new Error('reconcilePresentation:COMPOSER_NOT_USABLE_WITHOUT_BLOCKER');
        } catch (error) { failures.push(String(error.message)); }
      }
      if (failures.length) {
        if (active(record)) hold(record, failures.join('|'));
        return result(record, 'HOLD');
      }
      if (!active(record)) return result(record, 'STALE_ACK');
      record.state = 'CLOSED';
      emit(record, 'VOICE_CLOSE_ACK', { transportRetired: true, voiceOwnerReleased: true, threadPreserved: true, backendSubscriptionsPreserved: true, ...record.composer });
      return result(record, 'CLOSED', { ...record.composer });
    })();
    return record.closePromise;
  }

  function snapshot(value) {
    const scope = scopeOf(value), record = find(scope);
    if (!record) return null;
    return Object.freeze({ scope: record.scope, state: record.state, isCurrent: active(record), jobGeneration: record.jobGeneration, outputMutedConfirmed: Boolean(record.mutedAck), pendingOnsets: record.pendingOnsets, seenOnsets: record.seenOnsets.size, errors: [...record.errors], composer: record.composer, adapterKind: adapter.kind });
  }

  return Object.freeze({ beginSession, playNativeOutput, speechOnset, closeVoice, gateVaiJob, snapshot, events: () => copy(journal) });
}

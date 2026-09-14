// Offline candidate. No socket, Desktop discovery, TTS, player or transport is
// installed here. The host must own/authenticate the observer subscription.
import { createNativeVoiceBoundary } from './index.mjs';

const text = (v, k) => {
  if (typeof v !== 'string' || !v || v.length > 256) throw new TypeError(`invalid ${k}`);
  return v;
};
const generation = v => {
  if (!Number.isSafeInteger(v) || v < 1) throw new TypeError('invalid generation');
  return v;
};
const scopeKeys = ['threadId', 'nativeSessionId', 'voiceGeneration', 'ownerId'];
const sameScope = (a, b) => a && b && scopeKeys.every(k => a[k] === b[k]);
const hold = reason => Object.freeze({ status: 'HOLD', reason, readyForTts: false, egressAuthorized: false });
const FENCE = 'ALL_NATIVE_SINKS_BEFORE_FIRST_PLAY';

/** One owner subscription for one running Core lifetime. No restart authority
 * is inferred: a fresh instance cannot authorize restored receipts on its own.
 * Native adapter methods are trusted hooks, not caller-supplied ACK packets.
 */
export function createNativeSignalController({ adapter, ownerId, subscriptionId,
  onsetSource = 'UNAVAILABLE', timeoutMs = 1000 } = {}) {
  text(ownerId, 'ownerId'); text(subscriptionId, 'subscriptionId');
  if (!adapter || !['FAKE', 'NATIVE_PRIVATE_SEAM'].includes(adapter.kind)) throw new TypeError('adapter kind required');
  const fake = adapter.kind === 'FAKE';
  if (!['UNAVAILABLE', 'FAKE_FIXTURE', 'NATIVE_VAD_V2', 'NATIVE_VAD_V3'].includes(onsetSource)
      || (!fake && onsetSource === 'FAKE_FIXTURE')) throw new TypeError('unvalidated onset producer');
  // Core Audio GetMute(existing sessions)=true is not this fence. The private
  // host hook must cover session/sink creation BEFORE first output can play.
  const guarded = { kind: adapter.kind };
  for (const method of ['observeLiveSession', 'setOutputMuted', 'playNativeOutput', 'invalidateJobs',
    'stopPlayer', 'teardownVoice', 'reconcilePresentation']) {
    if (typeof adapter[method] !== 'function') throw new TypeError(`missing ${method}`);
    guarded[method] = async request => {
      const response = await adapter[method](request);
      if (['setOutputMuted', 'observeLiveSession'].includes(method)
          && (response?.coverage !== FENCE || response?.preFirstPlayFence !== true)) {
        throw new Error('NATIVE_PRE_FIRST_PLAY_FENCE_UNPROVED');
      }
      if (!fake && method === 'observeLiveSession'
          && (response?.onsetSubscriptionActive !== true || response?.onsetSource !== onsetSource)) {
        throw new Error('REAL_ONSET_SUBSCRIPTION_UNPROVED');
      }
      if (method === 'teardownVoice' && response?.scopeComparedAtomically !== true) {
        throw new Error('SCOPED_TEARDOWN_UNPROVED');
      }
      return response;
    };
  }
  const boundary = createNativeVoiceBoundary({ adapter: guarded, timeoutMs });
  const sessions = new Map(), highWater = new Map(), current = new Map();
  const key = e => JSON.stringify([e.threadId, e.startId, e.voiceGeneration]);
  function identity(e) { text(e?.threadId, 'threadId'); text(e.startId, 'startId'); generation(e.voiceGeneration); }
  function subscription(s) { if (s !== subscriptionId) throw new Error('UNTRUSTED_SUBSCRIPTION'); }
  function eligible(e) {
    identity(e);
    const record = sessions.get(key(e));
    return record && current.get(e.threadId) === record && record.state === 'SEALED'
      && sameScope(record.scope, e) ? record : null;
  }

  async function coreSignal(event, fromSubscription) {
    subscription(fromSubscription); identity(event);
    const k = key(event), record = sessions.get(k);
    if (event.type === 'nativeSessionStarting') {
      if (record) return { status: 'DUPLICATE_START', retired: record.state === 'RETIRED' };
      if (event.voiceGeneration <= (highWater.get(event.threadId) ?? 0)) return hold('STALE_START');
      const previous = current.get(event.threadId);
      if (previous && previous.state !== 'RETIRED') return hold('PREDECESSOR_NOT_RETIRED');
      const next = { threadId: event.threadId, startId: event.startId, voiceGeneration: event.voiceGeneration,
        state: 'STARTING', scope: null, onsetSequence: 0, begin: null };
      highWater.set(event.threadId, event.voiceGeneration); sessions.set(k, next); current.set(event.threadId, next);
      return { status: 'WAITING_FOR_PROVIDER_ID' };
    }
    if (!record || current.get(event.threadId) !== record || record.state === 'RETIRED') return hold('STALE_OR_UNKNOWN_START');
    if (event.type === 'nativeSessionReady') {
      text(event.nativeSessionId, 'nativeSessionId');
      if (record.scope) return record.scope.nativeSessionId === event.nativeSessionId
        ? record.begin : hold('PROVIDER_ID_REBIND_FORBIDDEN');
      record.scope = Object.freeze({ threadId: event.threadId, nativeSessionId: event.nativeSessionId,
        voiceGeneration: event.voiceGeneration, ownerId });
      record.state = 'SEALED';
      record.begin = boundary.beginSession(record.scope);
      return record.begin;
    }
    if (event.type === 'nativeSessionClosed') {
      if (record.scope && event.nativeSessionId !== record.scope.nativeSessionId) return hold('CLOSE_PROVIDER_ID_MISMATCH');
      if (!record.scope && event.nativeSessionId !== null) return hold('CLOSE_WITHOUT_PROVIDER_SEAL');
      record.state = 'RETIRED'; // synchronous fence; never wait for a player ACK
      if (!record.scope) return { status: 'RETIRED_BEFORE_PROVIDER' };
      // Core signal proves Core retirement only. Adapter must separately prove
      // scoped teardown/owner/presentation; {} from thread/realtime/stop cannot.
      return boundary.closeVoice(record.scope);
    }
    return hold('UNKNOWN_CORE_SIGNAL');
  }

  async function speechOnset(event, fromSubscription) {
    subscription(fromSubscription);
    const record = eligible(event);
    if (!record) return hold('STALE_OR_UNKNOWN_ONSET_SCOPE');
    generation(event.onsetSequence);
    if (!Number.isSafeInteger(event.timestampUnixMs) || event.timestampUnixMs < 0) return hold('INVALID_ONSET_TIMESTAMP');
    if (event.onsetSequence <= record.onsetSequence) return { status: 'DUPLICATE_OR_OLD_ONSET', egressAuthorized: false };
    if (onsetSource === 'UNAVAILABLE') return hold('REAL_ONSET_PRODUCER_NC');
    record.onsetSequence = event.onsetSequence;
    return boundary.speechOnset({ scope: record.scope, eventId: `onset:${event.onsetSequence}`,
      origin: fake ? 'FAKE_FIXTURE' : onsetSource });
  }

  async function authorizeVaiPlayback({ scope, startId, readProducerEvidence } = {}) {
    if (onsetSource === 'UNAVAILABLE') return hold('REAL_ONSET_PRODUCER_NC');
    let record;
    try { record = eligible({ ...scope, startId }); } catch { return hold('INVALID_CAPTURED_SCOPE'); }
    if (!record) return hold('STALE_OR_UNKNOWN_CAPTURED_SCOPE');
    const result = await boundary.authorizeVaiPlayback({ scope: record.scope, readProducerEvidence });
    // If Core close arrived during readback, never reuse even a positive result.
    if (record.state !== 'SEALED' || current.get(record.threadId) !== record) return hold('LIFECYCLE_RETIRED_DURING_GATE');
    // The await above adds a microtask boundary after the inner gate's final
    // check. An onset queued by the reader can run there; recheck its fence.
    if (result.readyForTts) {
      const fence = boundary.gateVaiJob({ scope: record.scope, jobGeneration: result.jobGeneration });
      if (!fence.allowed) return hold(fence.reason);
    }
    return result;
  }

  function snapshot(e) {
    identity(e); const record = sessions.get(key(e));
    if (!record) return null;
    return { state: record.state, scope: record.scope, onsetSequence: record.onsetSequence,
      boundary: record.scope ? boundary.snapshot(record.scope) : null };
  }
  return Object.freeze({ coreSignal, speechOnset, authorizeVaiPlayback, snapshot,
    events: () => boundary.events(), egressAuthorized: false,
    runtimeTransportInstalled: false, processRestartAuthorityProved: false });
}

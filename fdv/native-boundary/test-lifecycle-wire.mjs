import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createFakeAdapter } from './fake-adapter.mjs';
import { createNativeSignalController } from './lifecycle-wire.mjs';

const fixture = JSON.parse(readFileSync(new URL('./fixtures/PLAYBACK-EVIDENCE-FAKE.json', import.meta.url)));
const packet = () => structuredClone(fixture);
const ownerId = 'WIRE_FAKE_OWNER', subscriptionId = 'CORE_OBSERVER_FIXTURE';
const p = packet();
const scope = { threadId: p.identity.thread_id, nativeSessionId: p.identity.native_session_id,
  voiceGeneration: p.identity.voice_session_generation, ownerId };
const start = { threadId: scope.threadId, startId: 'CORE_START_OP_A', voiceGeneration: scope.voiceGeneration };
const signal = (type, fields = {}) => ({ type, ...start, ...fields });
const fence = { coverage: 'ALL_NATIVE_SINKS_BEFORE_FIRST_PLAY', preFirstPlayFence: true };

function setup(overrides = {}, options = {}) {
  const adapter = createFakeAdapter({
    setOutputMuted: async (r, a, ack) => ack(r, 'OUTPUT_MUTED_ACK', { muted: true, observation: 'SINK_READBACK', ...fence }),
    observeLiveSession: async (r, a, ack) => ack(r, 'LIVE_SESSION_ACK', { active: true, ownerCurrent: true,
      outputMuted: true, observation: 'SESSION_AND_SINK_READBACK', ...fence }),
    teardownVoice: async (r, a, ack) => ack(r, 'VOICE_TEARDOWN_ACK', {
      transportRetired: true, voiceOwnerReleased: true, scopeComparedAtomically: true }),
    ...overrides,
  });
  const wire = createNativeSignalController({ adapter, ownerId, subscriptionId, onsetSource: 'FAKE_FIXTURE', ...options });
  const send = e => wire.coreSignal(e, subscriptionId);
  const begin = async () => {
    assert.equal((await send(signal('nativeSessionStarting'))).status, 'WAITING_FOR_PROVIDER_ID');
    return send(signal('nativeSessionReady', { nativeSessionId: scope.nativeSessionId }));
  };
  const gate = () => wire.authorizeVaiPlayback({ scope, startId: start.startId, readProducerEvidence: packet });
  return { adapter, wire, send, begin, gate };
}

test('Core start then provider seal is required; final cannot invent scope', async () => {
  const { wire, send, gate, adapter } = setup();
  assert.equal((await gate()).status, 'HOLD');
  assert.equal((await send(signal('nativeSessionReady', { nativeSessionId: scope.nativeSessionId }))).status, 'HOLD');
  assert.equal(adapter.calls.length, 0);
  await send(signal('nativeSessionStarting'));
  assert.equal((await gate()).status, 'HOLD');
  assert.equal(wire.snapshot(start).scope, null);
  await send(signal('nativeSessionReady', { nativeSessionId: scope.nativeSessionId }));
  assert.equal((await gate()).status, 'READY_FOR_TTS');
  assert.equal((await gate()).egressAuthorized, false);
  assert.equal(wire.runtimeTransportInstalled, false);
  assert.equal(wire.processRestartAuthorityProved, false);
});

test('subscription mismatch rejected before state/effects', async () => {
  const { wire, adapter } = setup();
  await assert.rejects(wire.coreSignal(signal('nativeSessionStarting'), 'OTHER'), /UNTRUSTED_SUBSCRIPTION/);
  assert.equal(wire.snapshot(start), null); assert.equal(adapter.calls.length, 0);
});

test('provider ID immutable; duplicate ready does not repeat mute', async () => {
  const { send, begin, adapter } = setup(); await begin();
  assert.equal((await send(signal('nativeSessionReady', { nativeSessionId: 'OTHER' }))).reason, 'PROVIDER_ID_REBIND_FORBIDDEN');
  await send(signal('nativeSessionReady', { nativeSessionId: scope.nativeSessionId }));
  assert.equal(adapter.calls.filter(x => x.method === 'setOutputMuted').length, 1);
});

test('generation current/missing/unsafe is not accepted', async () => {
  const { send } = setup();
  for (const g of ['current', null, 0, -1, 2 ** 53]) {
    await assert.rejects(send(signal('nativeSessionStarting', { voiceGeneration: g })), /generation/);
  }
});

test('CoreAudio true readback for only existing sinks does not grant native fence', async () => {
  const { begin, gate } = setup({ setOutputMuted: async (r, a, ack) => ack(r, 'OUTPUT_MUTED_ACK', {
    muted: true, observation: 'SINK_READBACK', coverage: 'EXISTING_RENDER_SESSIONS_ONLY', preFirstPlayFence: false }) });
  assert.equal((await begin()).status, 'HOLD'); assert.equal((await gate()).status, 'HOLD');
});

test('mute SET true precedes any readback/gate; no sink play executed', async () => {
  const { adapter, begin, gate } = setup(); await begin(); await gate();
  assert.deepEqual(adapter.calls.map(x => x.method), ['setOutputMuted', 'observeLiveSession']);
  assert.equal(adapter.calls[0].request.muted, true);
});

test('fence lost on fresh observation denies previously muted job', async () => {
  const { begin, gate } = setup({ observeLiveSession: async (r, a, ack) => ack(r, 'LIVE_SESSION_ACK', {
    active: true, ownerCurrent: true, outputMuted: true, observation: 'SESSION_AND_SINK_READBACK',
    coverage: 'EXISTING_RENDER_SESSIONS_ONLY', preFirstPlayFence: false }) });
  await begin(); assert.equal((await gate()).reason, 'NATIVE_PRE_FIRST_PLAY_FENCE_UNPROVED');
});

test('scoped onset dedup invalidates job only; backend turn never cancelled', async () => {
  const { wire, begin, adapter, gate } = setup(); await begin();
  const onset = { ...scope, startId: start.startId, onsetSequence: 1, timestampUnixMs: 1789400000000 };
  assert.equal((await wire.speechOnset(onset, subscriptionId)).status, 'ONSET_HANDLED');
  assert.equal((await wire.speechOnset(onset, subscriptionId)).status, 'DUPLICATE_OR_OLD_ONSET');
  assert.equal(wire.snapshot(start).boundary.jobGeneration, 1);
  assert.equal((await gate()).status, 'HOLD');
  assert.equal(adapter.calls.filter(x => x.method === 'stopPlayer').length, 1);
  assert.equal(adapter.calls.filter(x => x.method === 'invalidateJobs').length, 1);
  assert.ok(!adapter.calls.some(x => /turn|cancel/i.test(x.method)));
});

test('unknown or wrong-session onset cannot be relabeled as real VAD', async () => {
  const { wire, begin, adapter, gate } = setup({}, { onsetSource: 'UNAVAILABLE' }); await begin();
  assert.equal((await gate()).reason, 'REAL_ONSET_PRODUCER_NC');
  const onset = { ...scope, startId: start.startId, onsetSequence: 1, timestampUnixMs: 1789400000000 };
  assert.equal((await wire.speechOnset(onset, subscriptionId)).reason, 'REAL_ONSET_PRODUCER_NC');
  assert.equal((await wire.speechOnset({ ...onset, nativeSessionId: 'WRONG' }, subscriptionId)).status, 'HOLD');
  assert.equal(adapter.calls.filter(x => x.method === 'stopPlayer').length, 0);
  assert.throws(() => setup({}, { onsetSource: 'RMS_ACOUSTIC_ACTIVITY' }), /unvalidated onset/);
});

test('close before provider retires start without constructing fake provider ID', async () => {
  const { wire, send, adapter } = setup(); await send(signal('nativeSessionStarting'));
  assert.equal((await send(signal('nativeSessionClosed', { nativeSessionId: null }))).status, 'RETIRED_BEFORE_PROVIDER');
  assert.equal((await send(signal('nativeSessionReady', { nativeSessionId: 'LATE' }))).status, 'HOLD');
  assert.equal(wire.snapshot(start).scope, null); assert.equal(adapter.calls.length, 0);
});

test('unscoped stop response cannot acknowledge scoped teardown or presentation', async () => {
  const { wire, send, begin, adapter } = setup({ teardownVoice: async () => ({}) }); await begin();
  assert.equal((await send(signal('nativeSessionClosed', { nativeSessionId: scope.nativeSessionId }))).status, 'HOLD');
  assert.equal(wire.snapshot(start).state, 'RETIRED');
  assert.equal(adapter.calls.filter(x => x.method === 'reconcilePresentation').length, 0);
});

test('N closed; N+1 sealed; late close/receipt/onset N cannot gain N+1 authority', async () => {
  const { wire, send, begin, gate } = setup(); await begin();
  assert.equal((await send(signal('nativeSessionClosed', { nativeSessionId: scope.nativeSessionId }))).status, 'CLOSED');
  const next = { ...start, startId: 'CORE_START_OP_B', voiceGeneration: start.voiceGeneration + 1 };
  await send({ type: 'nativeSessionStarting', ...next });
  await send({ type: 'nativeSessionReady', ...next, nativeSessionId: 'PROVIDER_B' });
  assert.equal((await send(signal('nativeSessionClosed', { nativeSessionId: scope.nativeSessionId }))).status, 'HOLD');
  assert.equal((await gate()).status, 'HOLD');
  assert.equal((await wire.speechOnset({ ...scope, startId: start.startId, onsetSequence: 99,
    timestampUnixMs: 1789400000000 }, subscriptionId)).status, 'HOLD');
  assert.equal(wire.snapshot(next).scope.nativeSessionId, 'PROVIDER_B');
  assert.equal(wire.snapshot(next).boundary.jobGeneration, 0);
});

test('no silent successor while predecessor live; reused lower generation cannot start', async () => {
  const { send, begin } = setup(); await begin();
  assert.equal((await send(signal('nativeSessionStarting', { startId: 'B', voiceGeneration: start.voiceGeneration + 1 }))).reason, 'PREDECESSOR_NOT_RETIRED');
  assert.equal((await send(signal('nativeSessionStarting', { startId: 'B' }))).reason, 'STALE_START');
});

test('onset in inner gate return microtask denies stale READY result', async () => {
  const { wire, begin } = setup(); await begin();
  let onset;
  const result = await wire.authorizeVaiPlayback({ scope, startId: start.startId,
    readProducerEvidence: () => {
      queueMicrotask(() => { onset = wire.speechOnset({ ...scope, startId: start.startId,
        onsetSequence: 1, timestampUnixMs: 1789400000000 }, subscriptionId); });
      return packet();
    } });
  assert.equal(result.status, 'HOLD'); await onset;
  assert.equal(wire.snapshot(start).boundary.jobGeneration, 1);
});

test('native onset source label without live subscription ACK cannot authorize', async () => {
  const fake = createFakeAdapter({
    setOutputMuted: async (r, a, ack) => ack(r, 'OUTPUT_MUTED_ACK', { muted: true, observation: 'SINK_READBACK', ...fence }),
    observeLiveSession: async (r, a, ack) => ack(r, 'LIVE_SESSION_ACK', { active: true, ownerCurrent: true,
      outputMuted: true, observation: 'SESSION_AND_SINK_READBACK', ...fence }),
  });
  const wire = createNativeSignalController({ adapter: { ...fake, kind: 'NATIVE_PRIVATE_SEAM' },
    ownerId, subscriptionId, onsetSource: 'NATIVE_VAD_V3' });
  await wire.coreSignal(signal('nativeSessionStarting'), subscriptionId);
  await wire.coreSignal(signal('nativeSessionReady', { nativeSessionId: scope.nativeSessionId }), subscriptionId);
  const result = await wire.authorizeVaiPlayback({ scope, startId: start.startId, readProducerEvidence: packet });
  assert.equal(result.reason, 'REAL_ONSET_SUBSCRIPTION_UNPROVED');
});

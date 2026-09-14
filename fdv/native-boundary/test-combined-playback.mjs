import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { createNativeVoiceBoundary } from './index.mjs';
import { createFakeAdapter } from './fake-adapter.mjs';

const fixture = JSON.parse(readFileSync(new URL('./fixtures/PLAYBACK-EVIDENCE-FAKE.json', import.meta.url)));
const clone = x => JSON.parse(JSON.stringify(x));
const packet = () => clone(fixture);
const scope = p => ({ threadId: p.identity.thread_id, nativeSessionId: p.identity.native_session_id,
  voiceGeneration: p.identity.voice_session_generation, ownerId: 'COMBINED_FAKE_OWNER' });
const canonical = x => x && typeof x === 'object' ? Array.isArray(x) ? `[${x.map(canonical).join(',')}]`
  : `{${Object.keys(x).sort().map(k => `${JSON.stringify(k)}:${canonical(x[k])}`).join(',')}}` : JSON.stringify(x);
const sha = x => createHash('sha256').update(x).digest('hex');
const reseal = p => { p.receipt_sha256 = sha(canonical(p.admission_receipt)); p.authorization_sha256 = sha(canonical(p.authorization)); p.journal.original_receipt_sha256 = p.receipt_sha256; return p; };
const tick = () => new Promise(resolve => setImmediate(resolve));
const deferred = () => { let resolve; const promise = new Promise(r => { resolve = r; }); return { promise, resolve }; };
async function bench(overrides = {}) {
  const p = packet(), s = scope(p), a = createFakeAdapter(overrides), b = createNativeVoiceBoundary({ adapter: a });
  assert.equal((await b.beginSession(s)).status, 'READY_MUTED');
  return { p, s, a, b, call: readProducerEvidence => b.authorizeVaiPlayback({ scope: s, readProducerEvidence }) };
}

test('positive: actual Python source+journal fixture passes only combined five-way gate', async () => {
  assert.notEqual(process.env.FDV_COMBINED_TEST_NEGATIVE, '1', 'INTENTIONAL_NEGATIVE_CONTROL');
  const { p, s, a, b, call } = await bench();
  assert.equal(b.gateVaiJob({ scope: s, jobGeneration: 0 }).finalProvenanceAuthorized, false);
  const result = await call(() => p);
  assert.equal(result.status, 'READY_FOR_TTS');
  for (const key of ['producerProvenance', 'admissionReceipt', 'liveNativeSession', 'boundaryGeneration', 'muteFence']) assert.equal(result[key], 'PASS');
  assert.equal(result.egressAuthorized, false); assert.equal(result.executionScope, 'OFFLINE_FAKE');
  assert.deepEqual(a.calls.map(c => c.method), ['setOutputMuted', 'observeLiveSession']);
  assert.equal(a.playerHandles.size, 0); // Gate did not itself create a player.
});

for (const field of ['thread_id', 'native_session_id', 'voice_session_generation', 'origin_id', 'client_id', 'turn_id', 'final_agent_item_id', 'job_generation']) {
  test(`negative: inconsistent ${field} cannot become READY`, async () => {
    const { p, call } = await bench(); p.identity[field] = typeof p.identity[field] === 'number' ? 99 : 'WRONG';
    assert.equal((await call(() => p)).status, 'HOLD');
  });
}
test('same thread+generation, consistently wrong native session across packet still denied by live session', async () => {
  const { p, call } = await bench();
  for (const obj of [p.identity, p.admission_receipt, p.authorization, p.journal]) obj.native_session_id = 'WRONG_NATIVE_SESSION';
  reseal(p);
  const result = await call(() => p);
  assert.equal(result.status, 'HOLD'); assert.equal(result.reason, 'LIVE_NATIVE_SESSION_IDENTITY_MISMATCH');
});

for (const mutate of [
  p => { p.authorization.state = 'Closed'; reseal(p); },
  p => { p.admission_receipt.admission_result = 'Queued'; reseal(p); },
  p => { p.source.first_user.client_id = 'wrong-client'; },
  p => { p.source.final_item.text += 'tampered'; },
  p => { p.source.final_item.phase = 'commentary'; },
  p => { p.source.final_pointer = 'another-final'; },
  p => { p.journal.status = 'HOLD_NC_PROVENANCE'; },
  p => { p.journal.original_version_sha256 = 'a'.repeat(64); },
  p => { p.journal.job_generation = 1; },
  p => { delete p.admission_receipt.native_session_id; reseal(p); },
]) {
  test(`negative: producer/source/journal mutation ${mutate.toString()}`, async () => {
    const { p, call } = await bench(); mutate(p); assert.equal((await call(() => p)).status, 'HOLD');
  });
}

for (const field of ['active', 'ownerCurrent', 'outputMuted']) {
  test(`negative live readback ${field}=false`, async () => {
    const { p, call } = await bench({ observeLiveSession: async (r, a, ack) => ack(r, 'LIVE_SESSION_ACK', {
      active: true, ownerCurrent: true, outputMuted: true, observation: 'SESSION_AND_SINK_READBACK', [field]: false }) });
    assert.equal((await call(() => p)).status, 'HOLD');
  });
}
test('no mute ACK means no live read and no combined readiness', async () => {
  const d = deferred(), p = packet(), s = scope(p), a = createFakeAdapter({ setOutputMuted: async (r, a, ack) => {
    await d.promise; return ack(r, 'OUTPUT_MUTED_ACK', { muted: true, observation: 'SINK_READBACK' });
  } });
  const b = createNativeVoiceBoundary({ adapter: a }), begin = b.beginSession(s);
  assert.equal((await b.authorizeVaiPlayback({ scope: s, readProducerEvidence: () => p })).status, 'HOLD');
  assert.equal(a.calls.filter(c => c.method === 'observeLiveSession').length, 0);
  d.resolve(); await begin;
});
test('onset during live readback fences old job and late callback; duplicate onset does not renew it', async () => {
  const d = deferred();
  const { p, s, b, call } = await bench({ observeLiveSession: async (r, a, ack) => {
    await d.promise; return ack(r, 'LIVE_SESSION_ACK', { active: true, ownerCurrent: true, outputMuted: true, observation: 'SESSION_AND_SINK_READBACK' });
  } });
  const pending = call(() => p); await tick();
  const event = { scope: s, eventId: 'ONSET', origin: 'FAKE_FIXTURE' };
  await b.speechOnset(event); const generation = b.snapshot(s).jobGeneration;
  await b.speechOnset(event); assert.equal(b.snapshot(s).jobGeneration, generation);
  d.resolve(); assert.equal((await pending).status, 'HOLD');
  assert.equal((await call(() => p)).status, 'HOLD');
});
test('close and successor during pending live ACK cannot authorize predecessor', async () => {
  const d = deferred();
  const { p, s, b, call } = await bench({ observeLiveSession: async (r, a, ack) => {
    await d.promise; return ack(r, 'LIVE_SESSION_ACK', { active: true, ownerCurrent: true, outputMuted: true, observation: 'SESSION_AND_SINK_READBACK' });
  } });
  const pending = call(() => p); await tick(); await b.closeVoice(s);
  const next = { ...s, voiceGeneration: s.voiceGeneration + 1, nativeSessionId: 'SUCCESSOR' };
  await b.beginSession(next); d.resolve(); assert.equal((await pending).status, 'HOLD');
  assert.equal(b.snapshot(next).state, 'READY_MUTED');
});
test('read current producer AFTER native await: revocation before reader returns HOLD', async () => {
  const d = deferred();
  const { p, call } = await bench({ observeLiveSession: async (r, a, ack) => {
    await d.promise; return ack(r, 'LIVE_SESSION_ACK', { active: true, ownerCurrent: true, outputMuted: true, observation: 'SESSION_AND_SINK_READBACK' });
  } });
  let current = p; const pending = call(() => current); await tick(); current = { status: 'PASS' };
  d.resolve(); assert.equal((await pending).status, 'HOLD');
});
test('reentrant reader triggering onset is checked again before readiness', async () => {
  const { p, s, b, call } = await bench(); let onset;
  assert.equal((await call(() => { onset = b.speechOnset({ scope: s, eventId: 'reentrant', origin: 'FAKE_FIXTURE' }); return p; })).status, 'HOLD');
  await onset;
});
test('bare PASS, async reader and absent reader never authorize alone', async () => {
  const { p, call, b, s } = await bench();
  assert.equal((await call(() => ({ status: 'PASS' }))).status, 'HOLD');
  assert.equal((await call(async () => p)).status, 'HOLD');
  assert.equal((await call(async () => { throw new Error('REJECTED_ASYNC_READER'); })).status, 'HOLD');
  await tick(); // A rejected invalid reader must not become an unhandled rejection.
  assert.equal((await b.authorizeVaiPlayback({ scope: s })).status, 'HOLD');
});
test('all 93 historical identities denied again by combined gate', async () => {
  const freeze = JSON.parse(readFileSync(new URL('../producer/historical_freeze.json', import.meta.url)));
  const { p, call } = await bench(); let denied = 0;
  for (const row of freeze.records) {
    const old = clone(p); old.identity.thread_id = row.thread_id; old.identity.turn_id = row.turn_id;
    const result = await call(() => old);
    assert.equal(result.reason, 'HISTORICAL_TURN_PERMANENT_HOLD'); denied++;
  }
  assert.equal(denied, 93);
});

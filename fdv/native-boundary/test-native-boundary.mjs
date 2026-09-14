import test from 'node:test';
import assert from 'node:assert/strict';
import { createNativeVoiceBoundary, toProducerScope } from './index.mjs';
import { createFakeAdapter } from './fake-adapter.mjs';

const scope = (voiceGeneration = 1, threadId = 'fixture-thread') => ({ threadId, nativeSessionId: `fixture-session-${voiceGeneration}`, voiceGeneration, ownerId: 'fixture-owner' });
const job = (s, jobGeneration = 0) => ({ scope: s, jobGeneration });
const onset = (s, eventId = 'fixture-onset-1') => ({ scope: s, eventId, origin: 'FAKE_FIXTURE' });
const deferred = () => { let resolve, reject; const promise = new Promise((a, b) => { resolve = a; reject = b; }); return { promise, resolve, reject }; };
const counts = (a, method) => a.calls.filter(c => c.method === method).length;
const tick = () => new Promise(resolve => setImmediate(resolve));

test('control positivo: muted readback precede primeiro play; mic e Core preservados', async () => {
  assert.notEqual(process.env.FDV_BOUNDARY_TEST_NEGATIVE, '1', 'INTENTIONAL_NEGATIVE_CONTROL');
  const a = createFakeAdapter(), b = createNativeVoiceBoundary({ adapter: a }), s = scope();
  assert.equal((await b.beginSession(s)).status, 'READY_MUTED');
  assert.equal((await b.playNativeOutput(s)).status, 'PLAYED_MUTED');
  assert.deepEqual(a.calls.map(c => c.method), ['setOutputMuted', 'playNativeOutput']);
  assert.equal(a.sinks.values().next().value.muted, true);
  assert.equal(a.microphoneActive, true);
  assert.equal(a.coreSubscriptionIdentity, 'FAKE_CORE_SUBSCRIPTION_UNCHANGED');
  assert.equal(b.gateVaiJob(job(s)).allowed, true);
  assert.equal(b.gateVaiJob(job(s)).finalProvenanceAuthorized, false);
  assert.ok(b.events().every(e => e.adapterKind === 'FAKE'));
});

test('nenhum play antes do ACK mesmo com criação assíncrona do sink', async () => {
  const d = deferred();
  const a = createFakeAdapter({ setOutputMuted: async (r, a, ack) => { await d.promise; return ack(r, 'OUTPUT_MUTED_ACK', { muted: true, observation: 'SINK_READBACK' }); } });
  const b = createNativeVoiceBoundary({ adapter: a }), s = scope();
  const begin = b.beginSession(s);
  assert.equal((await b.playNativeOutput(s)).status, 'REJECTED_NOT_MUTED');
  assert.equal(b.gateVaiJob(job(s)).allowed, false);
  assert.equal(counts(a, 'playNativeOutput'), 0);
  d.resolve();
  assert.equal((await begin).status, 'READY_MUTED');
});

for (const variant of ['false', 'missing', 'old-operation', 'old-generation', 'old-owner', 'no-readback']) {
  test(`controle negativo: ACK ${variant} nunca abre playback`, async () => {
    const a = createFakeAdapter({ setOutputMuted: async (r, a, ack) => {
      const value = ack(r, 'OUTPUT_MUTED_ACK', { muted: true, observation: 'SINK_READBACK' });
      if (variant === 'false') value.muted = false;
      if (variant === 'missing') return undefined;
      if (variant === 'old-operation') value.operationId = 'older-operation';
      if (variant === 'old-generation') value.scope = { ...r.scope, voiceGeneration: 999 };
      if (variant === 'old-owner') value.scope = { ...r.scope, ownerId: 'older-owner' };
      if (variant === 'no-readback') value.observation = 'INTENT_ONLY';
      return value;
    } });
    const b = createNativeVoiceBoundary({ adapter: a }), s = scope();
    assert.equal((await b.beginSession(s)).status, 'HOLD');
    assert.equal((await b.playNativeOutput(s)).status, 'REJECTED_NOT_MUTED');
    assert.equal(b.gateVaiJob(job(s)).allowed, false);
    assert.equal(counts(a, 'playNativeOutput'), 0);
  });
}

test('timeout de mute permanece HOLD após resposta tardia', async () => {
  const d = deferred();
  const a = createFakeAdapter({ setOutputMuted: async (r, a, ack) => { await d.promise; return ack(r, 'OUTPUT_MUTED_ACK', { muted: true, observation: 'SINK_READBACK' }); } });
  const b = createNativeVoiceBoundary({ adapter: a, timeoutMs: 15 }), s = scope();
  assert.equal((await b.beginSession(s)).status, 'HOLD');
  d.resolve(); await tick();
  assert.equal(b.snapshot(s).state, 'HOLD');
  assert.equal(counts(a, 'playNativeOutput'), 0);
});

test('begin duplicado não cria segundo efeito no sink', async () => {
  const a = createFakeAdapter(), b = createNativeVoiceBoundary({ adapter: a }), s = scope();
  const results = await Promise.all([b.beginSession(s), b.beginSession(s)]);
  assert.deepEqual(results[0], results[1]);
  assert.equal(counts(a, 'setOutputMuted'), 1);
});

test('drift do sink antes de play falha fechado no adapter que detém o elemento', async () => {
  const a = createFakeAdapter(), b = createNativeVoiceBoundary({ adapter: a }), s = scope();
  await b.beginSession(s);
  const sink = a.sinks.values().next().value;
  sink.muted = false;
  assert.equal((await b.playNativeOutput(s)).status, 'HOLD');
  assert.equal(sink.playing, false);
  assert.equal(b.gateVaiJob(job(s)).allowed, false);
});

test('onset invalida gate imediatamente, espera stop e cancela só os handles anteriores', async () => {
  const wait = deferred();
  const a = createFakeAdapter({ stopPlayer: async (r, a, ack) => {
    await wait.promise;
    for (const h of a.playerHandles.values()) if (JSON.stringify(h.scope) === JSON.stringify(r.scope) && h.jobGeneration < r.nextJobGeneration) h.stopped = true;
    return ack(r, 'PLAYER_STOP_ACK', { stopped: true, jobGeneration: r.nextJobGeneration });
  } });
  const b = createNativeVoiceBoundary({ adapter: a }), s = scope(), other = scope(1, 'other-thread');
  await b.beginSession(s);
  a.registerFakePlayer('old', s, 0); a.registerFakePlayer('next', s, 1); a.registerFakePlayer('unrelated', other, 0);
  const pending = b.speechOnset(onset(s));
  assert.equal(b.snapshot(s).jobGeneration, 1);
  assert.equal(b.gateVaiJob(job(s, 0)).allowed, false);
  assert.equal(b.gateVaiJob(job(s, 1)).reason, 'PRIOR_PLAYBACK_STOP_UNCONFIRMED');
  wait.resolve();
  assert.equal((await pending).status, 'ONSET_HANDLED');
  assert.equal(a.playerHandles.get('old').stopped, true);
  assert.equal(a.playerHandles.get('next').stopped, false);
  assert.equal(a.playerHandles.get('unrelated').stopped, false);
  assert.equal(b.gateVaiJob(job(s, 1)).allowed, true);
});

test('onset duplicado é deduplicado por ID na geração, sem segundo stop', async () => {
  const a = createFakeAdapter(), b = createNativeVoiceBoundary({ adapter: a }), s = scope();
  await b.beginSession(s);
  const values = await Promise.all([b.speechOnset(onset(s)), b.speechOnset(onset(s))]);
  assert.equal(values[0].jobGeneration, 1);
  assert.deepEqual(values[0], values[1]);
  assert.equal(counts(a, 'stopPlayer'), 1);
  assert.equal(counts(a, 'invalidateJobs'), 1);
});

test('onsets distintos concorrentes conservam cada receipt e tornam callback antigo inofensivo', async () => {
  const first = deferred();
  const a = createFakeAdapter({ stopPlayer: async (r, a, ack) => {
    if (r.nextJobGeneration === 1) await first.promise;
    return ack(r, 'PLAYER_STOP_ACK', { stopped: true, jobGeneration: r.nextJobGeneration });
  } });
  const b = createNativeVoiceBoundary({ adapter: a }), s = scope(); await b.beginSession(s);
  const one = b.speechOnset(onset(s, 'one'));
  const two = await b.speechOnset(onset(s, 'two'));
  assert.equal(two.jobGeneration, 2);
  assert.equal(b.gateVaiJob(job(s, 2)).allowed, false);
  first.resolve();
  assert.equal((await one).jobGeneration, 1);
  assert.equal(b.snapshot(s).jobGeneration, 2);
  assert.equal(b.gateVaiJob(job(s, 1)).allowed, false);
  assert.equal(b.gateVaiJob(job(s, 2)).allowed, true);
});

test('falha de stop conserva incerteza e impede novo áudio', async () => {
  const a = createFakeAdapter({ stopPlayer: async () => { throw new Error('FAKE_HANDLE_UNCONFIRMED'); } });
  const b = createNativeVoiceBoundary({ adapter: a }), s = scope(); await b.beginSession(s);
  assert.equal((await b.speechOnset(onset(s))).status, 'HOLD');
  assert.equal(b.gateVaiJob(job(s, 1)).allowed, false);
});

test('close cerca jobs antes de I/O e ACK exige teardown e reconciliação preservando backend', async () => {
  const a = createFakeAdapter(), b = createNativeVoiceBoundary({ adapter: a }), s = scope(); await b.beginSession(s);
  const close = b.closeVoice(s);
  assert.equal(b.snapshot(s).state, 'CLOSING');
  assert.equal(b.gateVaiJob(job(s, 1)).allowed, false);
  assert.equal((await close).status, 'CLOSED');
  assert.deepEqual(a.calls.slice(1).map(c => c.method), ['invalidateJobs', 'stopPlayer', 'teardownVoice', 'reconcilePresentation']);
  assert.equal(a.coreSubscriptionIdentity, 'FAKE_CORE_SUBSCRIPTION_UNCHANGED');
  assert.equal(b.events().at(-1).type, 'VOICE_CLOSE_ACK');
  assert.equal(b.events().at(-1).composerReady, true);
  assert.deepEqual(b.events().at(-1).independentBlockers, []);
});

test('composer indisponível sem outro blocker mantém HOLD e não emite close ACK', async () => {
  const a = createFakeAdapter({ reconcilePresentation: async (r, a, ack) => ack(r, 'PRESENTATION_ACK', {
    presentationDetached: true, threadPreserved: true, backendSubscriptionsPreserved: true,
    composerUsable: false, independentBlockers: [],
  }) });
  const b = createNativeVoiceBoundary({ adapter: a }), s = scope(); await b.beginSession(s);
  assert.equal((await b.closeVoice(s)).status, 'HOLD');
  assert.equal(b.events().filter(e => e.type === 'VOICE_CLOSE_ACK').length, 0);
  assert.deepEqual(b.snapshot(s).composer, { composerUsable: false, composerReady: false, independentBlockers: [] });
});

for (const missing of ['composerUsable', 'independentBlockers']) {
  test(`ACK sem ${missing} não presume composer pronto`, async () => {
    const a = createFakeAdapter({ reconcilePresentation: async (r, a, ack) => {
      const value = ack(r, 'PRESENTATION_ACK', { presentationDetached: true, threadPreserved: true, backendSubscriptionsPreserved: true, composerUsable: true, independentBlockers: [] });
      delete value[missing];
      return value;
    } });
    const b = createNativeVoiceBoundary({ adapter: a }), s = scope(); await b.beginSession(s);
    assert.equal((await b.closeVoice(s)).status, 'HOLD');
    assert.equal(b.events().filter(e => e.type === 'VOICE_CLOSE_ACK').length, 0);
  });
}

test('blocker desconhecido não justifica composer indisponível', async () => {
  const a = createFakeAdapter({ reconcilePresentation: async (r, a, ack) => ack(r, 'PRESENTATION_ACK', {
    presentationDetached: true, threadPreserved: true, backendSubscriptionsPreserved: true,
    composerUsable: false, independentBlockers: ['voiceStillReserved'],
  }) });
  const b = createNativeVoiceBoundary({ adapter: a }), s = scope(); await b.beginSession(s);
  assert.equal((await b.closeVoice(s)).status, 'HOLD');
  assert.equal(b.events().filter(e => e.type === 'VOICE_CLOSE_ACK').length, 0);
});

test('backend ocupado permite Voice CLOSED sem declarar composer pronto nem forçar idle', async () => {
  const a = createFakeAdapter({ reconcilePresentation: async (r, a, ack) => ack(r, 'PRESENTATION_ACK', {
    presentationDetached: true, threadPreserved: true, backendSubscriptionsPreserved: true,
    composerUsable: false, independentBlockers: ['backendBusy'],
  }) });
  const b = createNativeVoiceBoundary({ adapter: a }), s = scope(); await b.beginSession(s);
  const closed = await b.closeVoice(s);
  assert.equal(closed.status, 'CLOSED');
  assert.equal(closed.composerReady, false);
  assert.equal(closed.composerUsable, false);
  assert.deepEqual(closed.independentBlockers, ['backendBusy']);
  assert.equal(b.events().at(-1).type, 'VOICE_CLOSE_ACK');
  assert.equal(b.events().at(-1).composerReady, false);
  assert.equal(a.coreSubscriptionIdentity, 'FAKE_CORE_SUBSCRIPTION_UNCHANGED');
  assert.equal(a.calls.some(c => /idle|interrupt/i.test(c.method)), false);
});

test('close repetido não repete stop/teardown/reconciliação', async () => {
  const a = createFakeAdapter(), b = createNativeVoiceBoundary({ adapter: a }), s = scope(); await b.beginSession(s);
  const values = await Promise.all([b.closeVoice(s), b.closeVoice(s)]);
  assert.deepEqual(values[0], values[1]);
  await b.closeVoice(s);
  assert.equal(counts(a, 'teardownVoice'), 1);
  assert.equal(counts(a, 'reconcilePresentation'), 1);
});

for (const method of ['teardownVoice', 'reconcilePresentation']) {
  test(`close sem ACK ${method} mantém HOLD e bloqueia geração seguinte`, async () => {
    const a = createFakeAdapter({ [method]: async () => undefined });
    const b = createNativeVoiceBoundary({ adapter: a }), s = scope(); await b.beginSession(s);
    assert.equal((await b.closeVoice(s)).status, 'HOLD');
    assert.equal((await b.beginSession(scope(2))).status, 'REJECTED_PREVIOUS_NOT_CLOSED');
    assert.equal(b.events().filter(e => e.type === 'VOICE_CLOSE_ACK').length, 0);
  });
}

test('backend alterado no ACK de reconciliação é rejeitado', async () => {
  const a = createFakeAdapter({ reconcilePresentation: async (r, a, ack) => ack(r, 'PRESENTATION_ACK', { presentationDetached: true, threadPreserved: true, backendSubscriptionsPreserved: false }) });
  const b = createNativeVoiceBoundary({ adapter: a }), s = scope(); await b.beginSession(s);
  assert.equal((await b.closeVoice(s)).status, 'HOLD');
});

test('close N e onset N atrasados não tocam owner N+1', async () => {
  const a = createFakeAdapter(), b = createNativeVoiceBoundary({ adapter: a }), one = scope(), two = scope(2);
  await b.beginSession(one); await b.closeVoice(one); await b.beginSession(two);
  const count = a.calls.length;
  assert.equal((await b.closeVoice(one)).status, 'IGNORED_STALE_OR_UNKNOWN');
  assert.equal((await b.speechOnset(onset(one))).status, 'IGNORED_STALE_OR_INACTIVE');
  assert.equal((await b.beginSession(one)).status, 'REJECTED_RETIRED_OR_HELD');
  assert.equal(a.calls.length, count);
  assert.equal(b.snapshot(two).state, 'READY_MUTED');
  assert.equal(b.gateVaiJob(job(two)).allowed, true);
});

test('ACK do mute atrasado após close e nova geração não reabre a antiga', async () => {
  const wait = deferred();
  const a = createFakeAdapter({ setOutputMuted: async (r, a, ack) => {
    if (r.scope.voiceGeneration === 1) await wait.promise;
    return ack(r, 'OUTPUT_MUTED_ACK', { muted: true, observation: 'SINK_READBACK' });
  } });
  const b = createNativeVoiceBoundary({ adapter: a }), one = scope(), two = scope(2);
  const begin = b.beginSession(one); await tick();
  assert.equal((await b.closeVoice(one)).status, 'CLOSED');
  assert.equal((await b.beginSession(two)).status, 'READY_MUTED');
  wait.resolve();
  assert.equal((await begin).status, 'CANCELLED');
  assert.equal(b.snapshot(one).state, 'CLOSED');
  assert.equal(b.snapshot(two).state, 'READY_MUTED');
});

test('geração/owner regressivos e nova sessão sobre ativa são rejeitados', async () => {
  const a = createFakeAdapter(), b = createNativeVoiceBoundary({ adapter: a }), s = scope(); await b.beginSession(s);
  assert.equal((await b.beginSession({ ...s, ownerId: 'other-owner' })).status, 'REJECTED_GENERATION');
  assert.equal((await b.beginSession(scope(2))).status, 'REJECTED_PREVIOUS_NOT_CLOSED');
  assert.equal(counts(a, 'setOutputMuted'), 1);
});

test('fronteira não fabrica prova de onset V3 real e exige adapter explícito', async () => {
  assert.throws(() => createNativeVoiceBoundary({ adapter: {} }), /explicit adapter/);
  const a = createFakeAdapter(), b = createNativeVoiceBoundary({ adapter: a }), s = scope(); await b.beginSession(s);
  assert.throws(() => b.speechOnset({ ...onset(s), origin: 'NATIVE_VAD_V3' }), /origin/);
  assert.equal(b.snapshot(s).jobGeneration, 0);
  assert.equal(counts(a, 'stopPlayer'), 0);
});

test('mapeamento snake_case conserva sessão/owner/generation sem autorizar provenance', () => {
  assert.deepEqual(toProducerScope(scope(7)), { thread_id: 'fixture-thread', native_session_id: 'fixture-session-7', voice_session_generation: 7, owner_id: 'fixture-owner' });
  assert.throws(() => toProducerScope({ ...scope(), voiceGeneration: NaN }), /voiceGeneration/);
});

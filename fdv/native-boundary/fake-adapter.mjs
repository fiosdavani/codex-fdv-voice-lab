// FAKE: memory-only control plane. No audio device, process, network or Desktop access.
export function createFakeAdapter(overrides = {}) {
  const calls = [], sinks = new Map(), playerHandles = new Map();
  const key = scope => JSON.stringify([scope.threadId, scope.nativeSessionId, scope.voiceGeneration, scope.ownerId]);
  const ack = (request, type, fields = {}) => ({ type, scope: request.scope, operationId: request.operationId, ...fields });
  const adapter = {
    kind: 'FAKE',
    calls,
    sinks,
    playerHandles,
    registerFakePlayer(jobId, scope, jobGeneration) { playerHandles.set(jobId, { scope, jobGeneration, stopped: false }); },
    microphoneActive: true,
    coreSubscriptionIdentity: 'FAKE_CORE_SUBSCRIPTION_UNCHANGED',
    async setOutputMuted(request) {
      const sink = { muted: true, retired: false, playing: false };
      sinks.set(key(request.scope), sink);
      return ack(request, 'OUTPUT_MUTED_ACK', { muted: sink.muted, observation: 'SINK_READBACK' });
    },
    async playNativeOutput(request) {
      const sink = sinks.get(key(request.scope));
      if (!sink || sink.retired || sink.muted !== true || request.requiredMuted !== true) throw new Error('FAKE_SINK_NOT_MUTED');
      sink.playing = true;
      return ack(request, 'NATIVE_OUTPUT_PLAY_ACK', { muted: sink.muted });
    },
    async invalidateJobs(request) { return ack(request, 'JOBS_INVALIDATED_ACK', { jobGeneration: request.nextJobGeneration }); },
    async stopPlayer(request) {
      for (const handle of playerHandles.values()) {
        if (key(handle.scope) === key(request.scope) && handle.jobGeneration < request.nextJobGeneration) handle.stopped = true;
      }
      return ack(request, 'PLAYER_STOP_ACK', { stopped: true, jobGeneration: request.nextJobGeneration });
    },
    async teardownVoice(request) {
      const sink = sinks.get(key(request.scope));
      if (sink) { sink.retired = true; sink.playing = false; }
      return ack(request, 'VOICE_TEARDOWN_ACK', { transportRetired: true, voiceOwnerReleased: true });
    },
    async reconcilePresentation(request) { return ack(request, 'PRESENTATION_ACK', { presentationDetached: true, threadPreserved: true, backendSubscriptionsPreserved: true, composerUsable: true, independentBlockers: [] }); },
  };
  for (const method of ['setOutputMuted', 'playNativeOutput', 'invalidateJobs', 'stopPlayer', 'teardownVoice', 'reconcilePresentation']) {
    const implementation = overrides[method] ?? adapter[method];
    adapter[method] = async request => { calls.push({ method, request }); return implementation(request, adapter, ack); };
  }
  return adapter;
}

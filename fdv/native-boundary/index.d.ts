export declare const CANDIDATE_VERSION: 'FDV_NATIVE_VOICE_BOUNDARY_V1';
export interface VoiceScope {
  readonly threadId: string;
  readonly nativeSessionId: string;
  readonly voiceGeneration: number;
  readonly ownerId: string;
}
export interface ProducerScope {
  readonly thread_id: string;
  readonly native_session_id: string;
  readonly voice_session_generation: number;
  readonly owner_id: string;
}
export type AdapterKind = 'FAKE' | 'NATIVE_PRIVATE_SEAM';
export type OnsetOrigin = 'FAKE_FIXTURE' | 'NATIVE_VAD_V2' | 'NATIVE_VAD_V3';
export type BoundaryState = 'MUTING' | 'READY_MUTED' | 'CLOSING' | 'CLOSED' | 'HOLD';
export type IndependentComposerBlocker = 'backendBusy' | 'permissionPrompt' | 'threadReadOnly';
export interface ComposerObservation {
  readonly composerUsable: boolean;
  readonly independentBlockers: readonly IndependentComposerBlocker[];
  readonly composerReady: boolean;
}
export interface AdapterRequest {
  readonly scope: VoiceScope;
  readonly operationId: string;
}
export interface Acknowledgement extends AdapterRequest { readonly type: string; }
export interface OutputMutedAck extends Acknowledgement {
  readonly type: 'OUTPUT_MUTED_ACK';
  readonly muted: true;
  readonly observation: 'SINK_READBACK';
}
export interface JobFenceRequest extends AdapterRequest {
  readonly nextJobGeneration: number;
  readonly reason: 'SPEECH_ONSET' | 'VOICE_CLOSE';
}
/** Native implementation must verify scope/owner again immediately before each effect.
 * No discovery, external IPC or installed Desktop adapter is supplied in this candidate.
 */
export interface NativeBoundaryAdapter {
  readonly kind: AdapterKind;
  setOutputMuted(request: AdapterRequest & { readonly muted: true }): Promise<OutputMutedAck>;
  playNativeOutput(request: AdapterRequest & { readonly requiredMuted: true; readonly mutedOperationId: string }): Promise<Acknowledgement & { readonly type: 'NATIVE_OUTPUT_PLAY_ACK'; readonly muted: true }>;
  invalidateJobs(request: JobFenceRequest): Promise<Acknowledgement & { readonly type: 'JOBS_INVALIDATED_ACK'; readonly jobGeneration: number }>;
  /** Stop only handles of this scope with jobGeneration < nextJobGeneration. */
  stopPlayer(request: JobFenceRequest): Promise<Acknowledgement & { readonly type: 'PLAYER_STOP_ACK'; readonly stopped: true; readonly jobGeneration: number }>;
  teardownVoice(request: AdapterRequest): Promise<Acknowledgement & { readonly type: 'VOICE_TEARDOWN_ACK'; readonly transportRetired: true; readonly voiceOwnerReleased: true }>;
  reconcilePresentation(request: AdapterRequest): Promise<Acknowledgement & { readonly type: 'PRESENTATION_ACK'; readonly presentationDetached: true; readonly threadPreserved: true; readonly backendSubscriptionsPreserved: true; readonly composerUsable: boolean; readonly independentBlockers: readonly IndependentComposerBlocker[] }>;
}
export interface BoundaryResult {
  readonly status: string;
  readonly scope: VoiceScope;
  readonly jobGeneration?: number;
  readonly acknowledgement?: OutputMutedAck;
  readonly eventId?: string;
  readonly origin?: OnsetOrigin;
  readonly composerUsable?: boolean;
  readonly independentBlockers?: readonly IndependentComposerBlocker[];
  readonly composerReady?: boolean;
}
export interface BoundarySnapshot {
  readonly scope: VoiceScope;
  readonly state: BoundaryState;
  readonly isCurrent: boolean;
  readonly jobGeneration: number;
  readonly outputMutedConfirmed: boolean;
  readonly pendingOnsets: number;
  readonly seenOnsets: number;
  readonly errors: readonly string[];
  readonly composer: ComposerObservation | null;
  readonly adapterKind: AdapterKind;
}
export interface BoundaryEvent {
  readonly sequence: number;
  readonly type: string;
  readonly scope: VoiceScope;
  readonly jobGeneration: number;
  readonly adapterKind: AdapterKind;
  readonly [key: string]: unknown;
}
export interface NativeVoiceBoundary {
  beginSession(scope: VoiceScope): Promise<BoundaryResult>;
  playNativeOutput(scope: VoiceScope): Promise<BoundaryResult>;
  speechOnset(event: { scope: VoiceScope; eventId: string; origin: OnsetOrigin }): Promise<BoundaryResult>;
  closeVoice(scope: VoiceScope): Promise<BoundaryResult>;
  /** Necessary fence only. Does NOT authorize final provenance or egress. Recheck at actual player start. */
  gateVaiJob(job: { scope: VoiceScope; jobGeneration: number }): {
    allowed: boolean;
    reason: string;
    finalProvenanceAuthorized?: false;
    scope?: VoiceScope;
    jobGeneration?: number;
  };
  snapshot(scope: VoiceScope): BoundarySnapshot | null;
  events(): BoundaryEvent[];
}
export declare function toProducerScope(scope: VoiceScope): ProducerScope;
export declare function createNativeVoiceBoundary(options: { adapter: NativeBoundaryAdapter; timeoutMs?: number }): NativeVoiceBoundary;

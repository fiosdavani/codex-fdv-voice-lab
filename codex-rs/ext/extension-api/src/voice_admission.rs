//! Opt-in host seam for durable Voice admission; never owns a Core turn itself.

use std::future::Future;
use std::pin::Pin;

use codex_protocol::ThreadId;

/// Immutable identity sealed by Core from the provider's session lifecycle.
/// A handoff retains this snapshot; it never looks up the current generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoiceAdmissionScope {
    pub voice_session_generation: u64,
    pub native_session_id: String,
}

/// Origin retained before a realtime handoff is rendered into model-visible text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoiceAdmissionInput {
    pub thread_id: ThreadId,
    pub scope: VoiceAdmissionScope,
    pub origin_id: String,
    pub handoff_id: Option<String>,
    pub item_id: Option<String>,
    pub text: String,
}

/// Durable queue acceptance, not evidence that Core has started or persisted a turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoiceAdmissionAck {
    pub queued_item_id: String,
}

pub type VoiceAdmissionFuture<'a> =
    Pin<Box<dyn Future<Output = Result<VoiceAdmissionAck, String>> + Send + 'a>>;

/// Implemented by the host's existing durable queue. Implementations deduplicate
/// by origin, preserve client_id, and use StartIfIdle. An ambiguous dispatch must
/// never be retried without positive reconciliation. Core has no queue dependency.
pub trait VoiceAdmission: Send + Sync {
    fn admit(&self, input: VoiceAdmissionInput) -> VoiceAdmissionFuture<'_>;
}

/// Explicit thread attachment enabling the native lifecycle candidate. Installing
/// the durable queue alone does not enable it. No default host installs this hook.
#[derive(Clone)]
pub struct VoiceNativeSessionHooks(pub std::sync::Arc<dyn VoiceNativeSessionObserver>);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VoiceNativeSessionEvent {
    Starting,
    Ready { native_session_id: String },
    Closed { native_session_id: Option<String> },
}

/// The generation is Core-owned and monotonic within the Core process. Hosts must
/// reject stale subscriptions across process restarts; it is not a durable lease.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoiceNativeSessionSignal {
    pub thread_id: ThreadId,
    pub start_id: String,
    pub voice_generation: u64,
    pub event: VoiceNativeSessionEvent,
}

impl VoiceNativeSessionSignal {
    /// Export the lifecycle envelope without playback ownership or payload text.
    pub fn to_json(&self) -> serde_json::Value {
        let mut value = serde_json::json!({
            "threadId": self.thread_id.to_string(),
            "startId": self.start_id,
            "voiceGeneration": self.voice_generation,
        });
        match &self.event {
            VoiceNativeSessionEvent::Starting => value["type"] = "nativeSessionStarting".into(),
            VoiceNativeSessionEvent::Ready { native_session_id } => {
                value["type"] = "nativeSessionReady".into();
                value["nativeSessionId"] = native_session_id.clone().into();
            }
            VoiceNativeSessionEvent::Closed { native_session_id } => {
                value["type"] = "nativeSessionClosed".into();
                value["nativeSessionId"] = serde_json::json!(native_session_id);
            }
        }
        value
    }
}

pub type VoiceNativeSessionFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;

/// Host-owned, ordered lifecycle delivery. A successful future acknowledges
/// delivery to the bound observer, not permission to play audio. Implementations
/// must bound delivery time and report failures; Core admits no handoff until
/// Ready is acknowledged. Captured thread/start/generation must never be replaced
/// with a current-session lookup, including on Closed.
/// Delivery runs under Core's lifecycle permit: do not reenter or await Core
/// start/stop from this callback. Closed may confirm host presentation teardown,
/// but must not resubmit Core stop. Runtime timeout enforcement belongs to the
/// host; this candidate supplies no production observer.
pub trait VoiceNativeSessionObserver: Send + Sync {
    fn emit(&self, signal: VoiceNativeSessionSignal) -> VoiceNativeSessionFuture<'_>;
}

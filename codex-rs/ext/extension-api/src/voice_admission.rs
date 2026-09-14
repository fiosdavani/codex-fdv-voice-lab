//! Opt-in host seam for durable Voice admission; never owns a Core turn itself.

use std::future::Future;
use std::pin::Pin;

use codex_protocol::ThreadId;

/// Explicit host attachment for one native session. The host must supply a fresh,
/// persisted generation and native session identity; neither is inferred from text.
/// No config default installs this attachment. Retain its snapshot for the session.
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

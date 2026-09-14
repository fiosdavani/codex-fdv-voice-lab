use anyhow::Result;
use codex_protocol::ThreadId;
use serde::Deserialize;
use serde::Serialize;
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

/// An opaque provider utterance key, already scoped by the caller to its origin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceQueueOrigin {
    pub native_session_id: String,
    pub voice_session_generation: u64,
    pub origin_id: String,
    pub handoff_id: Option<String>,
    pub item_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VoiceAdmissionResult {
    Queued,
    Claimed,
    Started,
    Ambiguous,
    Rejected,
    Cancelled,
}

/// Durable admission evidence; Started is acceptance, not a terminal result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceAdmissionReceipt {
    pub schema: String,
    pub thread_id: ThreadId,
    pub native_session_id: String,
    pub voice_session_generation: u64,
    pub origin_id: String,
    pub handoff_id: Option<String>,
    pub item_id: Option<String>,
    pub queued_item_id: String,
    pub receipt_id: String,
    pub client_id: String,
    /// None means conflict detection compares the private canonical payload.
    pub input_digest: Option<String>,
    pub admission_result: VoiceAdmissionResult,
    pub attempt_id: Option<String>,
    pub turn_id: Option<String>,
    pub reason: Option<String>,
}

/// Only a positively observed Core rejection may release a durable claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceClaimOutcome {
    Started { turn_id: String },
    RetryableRejection { reason: String },
    Rejected { reason: String },
    Ambiguous { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceEnqueueOutcome {
    Inserted(VoiceAdmissionReceipt),
    Existing(VoiceAdmissionReceipt),
}

impl VoiceAdmissionReceipt {
    pub(crate) fn try_from_row(row: &SqliteRow) -> Result<Self> {
        let result: String = row.try_get("admission_result")?;
        let admission_result = match result.as_str() {
            "Queued" => VoiceAdmissionResult::Queued,
            "Claimed" => VoiceAdmissionResult::Claimed,
            "Started" => VoiceAdmissionResult::Started,
            "Ambiguous" => VoiceAdmissionResult::Ambiguous,
            "Rejected" => VoiceAdmissionResult::Rejected,
            "Cancelled" => VoiceAdmissionResult::Cancelled,
            _ => anyhow::bail!("invalid voice admission state"),
        };
        let native_session_id: Option<String> = row.try_get("native_session_id")?;
        let native_session_id = native_session_id
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("voice receipt has no native session binding"))?;
        let queued_item_id: String = row.try_get("queued_item_id")?;
        Ok(Self {
            schema: "fdv.voice.admission.v1".to_string(),
            thread_id: ThreadId::try_from(row.try_get::<String, _>("thread_id")?)?,
            native_session_id,
            voice_session_generation: u64::try_from(row.try_get::<i64, _>("voice_session_generation")?)?,
            origin_id: row.try_get("origin_id")?,
            handoff_id: row.try_get("handoff_id")?,
            item_id: row.try_get("item_id")?,
            receipt_id: queued_item_id.clone(),
            queued_item_id,
            client_id: row.try_get("client_id")?,
            input_digest: row.try_get("input_digest")?,
            admission_result,
            attempt_id: row.try_get("attempt_id")?,
            turn_id: row.try_get("turn_id")?,
            reason: row.try_get("reason")?,
        })
    }
}

/// One durable, ordered user submission for a thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedUserSubmissionRecord {
    pub id: String,
    pub thread_id: ThreadId,
    pub payload: String,
}

impl QueuedUserSubmissionRecord {
    pub(crate) fn try_from_row(row: &SqliteRow) -> Result<Self> {
        Ok(Self {
            id: row.try_get("id")?,
            thread_id: ThreadId::try_from(row.try_get::<String, _>("thread_id")?)?,
            payload: row.try_get("payload_json")?,
        })
    }
}

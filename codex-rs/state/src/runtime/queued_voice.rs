use super::SqliteQueueStore;
use crate::MAX_QUEUE_ITEMS;
use crate::VoiceAdmissionReceipt;
use crate::VoiceAdmissionResult;
use crate::VoiceClaimOutcome;
use crate::VoiceEnqueueOutcome;
use crate::VoiceQueueOrigin;
use codex_protocol::ThreadId;
use sqlx::Row;
use uuid::Uuid;

// Keep these statements literal so offline SQLite validation exercises the
// statements used by Rust, rather than a second implementation of the queue.
const ENQUEUE_VOICE_RECEIPT_SQL: &str = r#"
INSERT INTO voice_admission_receipts (
    thread_id, origin_id, native_session_id, voice_session_generation, handoff_id, item_id,
    queued_item_id, client_id, payload_json, admission_result
) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'Queued')
ON CONFLICT(thread_id, origin_id) DO NOTHING
"#;
const ENQUEUE_VOICE_ITEM_SQL: &str = r#"
INSERT INTO queued_items (id, thread_id, payload_json, queue_order, created_at_ms, updated_at_ms)
SELECT ?, ?, ?, COALESCE((SELECT MAX(queue_order) FROM queued_items WHERE thread_id = ?), -1) + 1, ?, ?
WHERE (SELECT COUNT(*) FROM queued_items WHERE thread_id = ?) < ?
"#;
const GET_VOICE_RECEIPT_SQL: &str =
    "SELECT * FROM voice_admission_receipts WHERE thread_id = ? AND origin_id = ?";
const GET_VOICE_ITEM_RECEIPT_SQL: &str =
    "SELECT * FROM voice_admission_receipts WHERE thread_id = ? AND queued_item_id = ?";
const CLAIM_VOICE_SQL: &str = r#"
UPDATE voice_admission_receipts SET admission_result = 'Claimed', attempt_id = ?, reason = NULL
WHERE thread_id = ? AND queued_item_id = ? AND admission_result = 'Queued'
AND native_session_id IS NOT NULL AND length(trim(native_session_id)) > 0
AND EXISTS (
    SELECT 1 FROM queued_items WHERE id = queued_item_id
      AND thread_id = voice_admission_receipts.thread_id
      AND payload_json = voice_admission_receipts.payload_json
)
AND NOT EXISTS (
    SELECT 1 FROM voice_admission_receipts AS blocked
    WHERE blocked.thread_id = voice_admission_receipts.thread_id
      AND blocked.admission_result IN ('Claimed', 'Ambiguous')
)
RETURNING *
"#;
const FINISH_VOICE_CLAIM_SQL: &str = r#"
UPDATE voice_admission_receipts
SET admission_result = ?, turn_id = ?, reason = ?, attempt_id = ?
WHERE thread_id = ? AND queued_item_id = ? AND admission_result = 'Claimed' AND attempt_id = ?
AND native_session_id IS NOT NULL AND length(trim(native_session_id)) > 0
RETURNING *
"#;
const REMOVE_VOICE_ITEM_SQL: &str = "DELETE FROM queued_items WHERE thread_id = ? AND id = ?";
const RECONCILE_VOICE_STARTED_SQL: &str = r#"
UPDATE voice_admission_receipts SET admission_result = 'Started', turn_id = ?, reason = NULL
WHERE thread_id = ? AND native_session_id = ? AND origin_id = ? AND client_id = ?
AND length(trim(native_session_id)) > 0
AND (admission_result IN ('Claimed', 'Ambiguous') OR (admission_result = 'Started' AND turn_id = ?))
RETURNING *
"#;

impl SqliteQueueStore {
    /// Atomically enqueue once per opaque origin. Receipts survive queue removal.
    pub async fn enqueue_voice(
        &self,
        thread_id: ThreadId,
        payload_json: &str,
        origin: &VoiceQueueOrigin,
    ) -> anyhow::Result<VoiceEnqueueOutcome> {
        anyhow::ensure!(!origin.origin_id.is_empty(), "voice origin id is empty");
        anyhow::ensure!(
            !origin.native_session_id.trim().is_empty(),
            "native session id is empty"
        );
        anyhow::ensure!(
            origin.voice_session_generation > 0,
            "invalid voice generation"
        );
        let input: serde_json::Value = serde_json::from_str(payload_json)?;
        anyhow::ensure!(
            input
                .pointer("/UserInput/client_id")
                .and_then(serde_json::Value::as_str)
                == Some(origin.origin_id.as_str()),
            "voice input client id does not match origin"
        );
        let generation = i64::try_from(origin.voice_session_generation)?;
        let queued_item_id = Uuid::now_v7().to_string();
        let mut transaction = self.pool.begin().await?;
        // First acquire the SQLite write lock, including on the duplicate path.
        let inserted = sqlx::query(ENQUEUE_VOICE_RECEIPT_SQL)
            .bind(thread_id.to_string())
            .bind(&origin.origin_id)
            .bind(&origin.native_session_id)
            .bind(generation)
            .bind(&origin.handoff_id)
            .bind(&origin.item_id)
            .bind(&queued_item_id)
            .bind(&origin.origin_id)
            .bind(payload_json)
            .execute(transaction.as_mut())
            .await?
            .rows_affected();
        let row = sqlx::query(GET_VOICE_RECEIPT_SQL)
            .bind(thread_id.to_string())
            .bind(&origin.origin_id)
            .fetch_one(transaction.as_mut())
            .await?;
        let receipt = VoiceAdmissionReceipt::try_from_row(&row)?;
        anyhow::ensure!(
            row.try_get::<String, _>("payload_json")? == payload_json
                && receipt.native_session_id == origin.native_session_id
                && receipt.voice_session_generation == origin.voice_session_generation
                && receipt.handoff_id == origin.handoff_id
                && receipt.item_id == origin.item_id,
            "voice origin conflict"
        );
        if inserted == 1 {
            let now_ms = chrono::Utc::now().timestamp_millis();
            let inserted = sqlx::query(ENQUEUE_VOICE_ITEM_SQL)
                .bind(&queued_item_id)
                .bind(thread_id.to_string())
                .bind(payload_json)
                .bind(thread_id.to_string())
                .bind(now_ms)
                .bind(now_ms)
                .bind(thread_id.to_string())
                .bind(i64::try_from(MAX_QUEUE_ITEMS)?)
                .execute(transaction.as_mut())
                .await?
                .rows_affected();
            anyhow::ensure!(inserted == 1, "voice queue is full");
        }
        transaction.commit().await?;
        Ok(if inserted == 1 {
            VoiceEnqueueOutcome::Inserted(receipt)
        } else {
            VoiceEnqueueOutcome::Existing(receipt)
        })
    }

    pub async fn get_voice_receipt(
        &self,
        thread_id: ThreadId,
        origin_id: &str,
    ) -> anyhow::Result<Option<VoiceAdmissionReceipt>> {
        sqlx::query(GET_VOICE_RECEIPT_SQL)
            .bind(thread_id.to_string())
            .bind(origin_id)
            .fetch_optional(self.pool.as_ref())
            .await?
            .as_ref()
            .map(VoiceAdmissionReceipt::try_from_row)
            .transpose()
    }

    pub async fn voice_receipt_for_item(
        &self,
        thread_id: ThreadId,
        queued_item_id: &str,
    ) -> anyhow::Result<Option<VoiceAdmissionReceipt>> {
        sqlx::query(GET_VOICE_ITEM_RECEIPT_SQL)
            .bind(thread_id.to_string())
            .bind(queued_item_id)
            .fetch_optional(self.pool.as_ref())
            .await?
            .as_ref()
            .map(VoiceAdmissionReceipt::try_from_row)
            .transpose()
    }

    /// Claims never expire into eligibility. A crashed claimant must reconcile.
    pub async fn claim_voice(
        &self,
        thread_id: ThreadId,
        queued_item_id: &str,
        attempt_id: &str,
    ) -> anyhow::Result<Option<VoiceAdmissionReceipt>> {
        anyhow::ensure!(!attempt_id.is_empty(), "voice attempt id is empty");
        sqlx::query(CLAIM_VOICE_SQL)
            .bind(attempt_id)
            .bind(thread_id.to_string())
            .bind(queued_item_id)
            .fetch_optional(self.pool.as_ref())
            .await?
            .as_ref()
            .map(VoiceAdmissionReceipt::try_from_row)
            .transpose()
    }

    /// Record the observed outcome and dequeue terminal admissions atomically.
    pub async fn finish_voice_claim(
        &self,
        thread_id: ThreadId,
        queued_item_id: &str,
        attempt_id: &str,
        outcome: VoiceClaimOutcome,
    ) -> anyhow::Result<VoiceAdmissionReceipt> {
        let (state, turn_id, reason) = match outcome {
            VoiceClaimOutcome::Started { turn_id } => {
                anyhow::ensure!(!turn_id.is_empty(), "voice turn id is empty");
                ("Started", Some(turn_id), None)
            }
            VoiceClaimOutcome::RetryableRejection { reason } => ("Queued", None, Some(reason)),
            VoiceClaimOutcome::Rejected { reason } => ("Rejected", None, Some(reason)),
            VoiceClaimOutcome::Ambiguous { reason } => ("Ambiguous", None, Some(reason)),
        };
        let next_attempt = if state == "Queued" {
            None
        } else {
            Some(attempt_id)
        };
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query(FINISH_VOICE_CLAIM_SQL)
            .bind(state)
            .bind(turn_id)
            .bind(reason)
            .bind(next_attempt)
            .bind(thread_id.to_string())
            .bind(queued_item_id)
            .bind(attempt_id)
            .fetch_optional(transaction.as_mut())
            .await?
            .ok_or_else(|| anyhow::anyhow!("voice claim no longer matches"))?;
        let receipt = VoiceAdmissionReceipt::try_from_row(&row)?;
        if matches!(
            receipt.admission_result,
            VoiceAdmissionResult::Started | VoiceAdmissionResult::Rejected
        ) {
            sqlx::query(REMOVE_VOICE_ITEM_SQL)
                .bind(thread_id.to_string())
                .bind(queued_item_id)
                .execute(transaction.as_mut())
                .await?;
        }
        transaction.commit().await?;
        Ok(receipt)
    }

    /// Caller must have positively matched persisted thread, turn and client IDs.
    /// Absence of history is never evidence permitting a replay.
    pub async fn reconcile_voice_started(
        &self,
        thread_id: ThreadId,
        native_session_id: &str,
        origin_id: &str,
        client_id: &str,
        turn_id: &str,
    ) -> anyhow::Result<VoiceAdmissionReceipt> {
        anyhow::ensure!(!turn_id.is_empty(), "voice turn id is empty");
        anyhow::ensure!(
            client_id == origin_id,
            "voice reconciliation client id mismatch"
        );
        anyhow::ensure!(
            !native_session_id.trim().is_empty(),
            "native session id is empty"
        );
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query(RECONCILE_VOICE_STARTED_SQL)
            .bind(turn_id)
            .bind(thread_id.to_string())
            .bind(native_session_id)
            .bind(origin_id)
            .bind(client_id)
            .bind(turn_id)
            .fetch_optional(transaction.as_mut())
            .await?
            .ok_or_else(|| anyhow::anyhow!("voice receipt cannot be reconciled"))?;
        let receipt = VoiceAdmissionReceipt::try_from_row(&row)?;
        sqlx::query(REMOVE_VOICE_ITEM_SQL)
            .bind(thread_id.to_string())
            .bind(&receipt.queued_item_id)
            .execute(transaction.as_mut())
            .await?;
        transaction.commit().await?;
        Ok(receipt)
    }
}

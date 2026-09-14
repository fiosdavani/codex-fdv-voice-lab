use std::fmt::Display;
use std::future::Future;

use codex_protocol::ThreadId;
use codex_rollout::StateDbHandle;
use codex_state::QueuedUserSubmissionRecord;
use codex_state::SqliteQueueStore;
use codex_state::VoiceAdmissionReceipt;
use codex_state::VoiceClaimOutcome;
use codex_state::VoiceEnqueueOutcome;
use codex_state::VoiceQueueOrigin;

use crate::MAX_QUEUE_ITEMS;
use crate::ThreadStoreError;
use crate::ThreadStoreFuture;

/// Storage-neutral persistence for ordered, thread-scoped user messages.
pub trait QueueStore: Send + Sync {
    /// Opt in only when every receipt, claim and protected mutation is implemented.
    fn supports_voice_admission(&self) -> bool {
        false
    }
    /// Implement all voice methods together. The default cannot admit voice.
    fn enqueue_voice(
        &self,
        _thread_id: ThreadId,
        _payload: String,
        _origin: VoiceQueueOrigin,
    ) -> ThreadStoreFuture<'_, VoiceEnqueueOutcome> {
        Box::pin(async { Err(voice_unavailable()) })
    }

    fn get_voice_receipt(
        &self,
        _thread_id: ThreadId,
        _origin_id: String,
    ) -> ThreadStoreFuture<'_, Option<VoiceAdmissionReceipt>> {
        Box::pin(async { Err(voice_unavailable()) })
    }

    fn voice_receipt_for_item(
        &self,
        _thread_id: ThreadId,
        _item_id: String,
    ) -> ThreadStoreFuture<'_, Option<VoiceAdmissionReceipt>> {
        Box::pin(async { Err(voice_unavailable()) })
    }

    fn claim_voice(
        &self,
        _thread_id: ThreadId,
        _item_id: String,
        _attempt_id: String,
    ) -> ThreadStoreFuture<'_, Option<VoiceAdmissionReceipt>> {
        Box::pin(async { Err(voice_unavailable()) })
    }

    fn finish_voice_claim(
        &self,
        _thread_id: ThreadId,
        _item_id: String,
        _attempt_id: String,
        _outcome: VoiceClaimOutcome,
    ) -> ThreadStoreFuture<'_, VoiceAdmissionReceipt> {
        Box::pin(async { Err(voice_unavailable()) })
    }

    /// Accept only positive evidence from the host's persisted user-item reader.
    fn reconcile_voice_started(
        &self,
        _thread_id: ThreadId,
        _origin_id: String,
        _client_id: String,
        _turn_id: String,
    ) -> ThreadStoreFuture<'_, VoiceAdmissionReceipt> {
        Box::pin(async { Err(voice_unavailable()) })
    }

    /// Return a stable revision that changes when another connection updates the queue.
    fn change_version(&self) -> ThreadStoreFuture<'_, i64>;

    /// Return changed, loaded thread IDs and their durable revisions after `revision`.
    fn changes_since<'a>(
        &'a self,
        revision: i64,
        thread_ids: &'a [ThreadId],
    ) -> ThreadStoreFuture<'a, Vec<(ThreadId, i64)>>;

    fn enqueue(
        &self,
        thread_id: ThreadId,
        payload: String,
    ) -> ThreadStoreFuture<'_, QueuedUserSubmissionRecord>;

    fn list_page(
        &self,
        thread_id: ThreadId,
        offset: usize,
        limit: usize,
    ) -> ThreadStoreFuture<'_, Vec<QueuedUserSubmissionRecord>>;

    fn update(
        &self,
        thread_id: ThreadId,
        item_id: String,
        payload: String,
    ) -> ThreadStoreFuture<'_, Option<QueuedUserSubmissionRecord>>;

    fn delete(&self, thread_id: ThreadId, item_id: String) -> ThreadStoreFuture<'_, bool>;

    /// Atomically replace queue order with every current item ID exactly once.
    ///
    /// Returns [`ThreadStoreError::InvalidRequest`] when `item_ids` is not a
    /// permutation of the complete queue.
    fn reorder(&self, thread_id: ThreadId, item_ids: Vec<String>) -> ThreadStoreFuture<'_, ()>;
}

/// Adapts the local state runtime to the shared queue-storage interface.
#[derive(Clone)]
pub struct LocalQueueStore {
    state_db: StateDbHandle,
}

impl LocalQueueStore {
    pub fn new(state_db: StateDbHandle) -> Self {
        Self { state_db }
    }

    fn queue(&self) -> &SqliteQueueStore {
        self.state_db.thread_queue()
    }
}

fn queue_future<'a, T, E>(
    future: impl Future<Output = Result<T, E>> + Send + 'a,
) -> ThreadStoreFuture<'a, T>
where
    T: Send + 'a,
    E: Display + Send + 'a,
{
    Box::pin(async move {
        future.await.map_err(|error| ThreadStoreError::Internal {
            message: format!("queue storage failed: {error}"),
        })
    })
}

fn voice_unavailable() -> ThreadStoreError {
    ThreadStoreError::InvalidRequest {
        message: "durable voice admission is unavailable".to_string(),
    }
}

impl QueueStore for LocalQueueStore {
    fn supports_voice_admission(&self) -> bool {
        true
    }
    fn enqueue_voice(
        &self,
        thread_id: ThreadId,
        payload: String,
        origin: VoiceQueueOrigin,
    ) -> ThreadStoreFuture<'_, VoiceEnqueueOutcome> {
        queue_future(async move { self.queue().enqueue_voice(thread_id, &payload, &origin).await })
    }

    fn get_voice_receipt(
        &self,
        thread_id: ThreadId,
        origin_id: String,
    ) -> ThreadStoreFuture<'_, Option<VoiceAdmissionReceipt>> {
        queue_future(async move { self.queue().get_voice_receipt(thread_id, &origin_id).await })
    }

    fn voice_receipt_for_item(
        &self,
        thread_id: ThreadId,
        item_id: String,
    ) -> ThreadStoreFuture<'_, Option<VoiceAdmissionReceipt>> {
        queue_future(async move { self.queue().voice_receipt_for_item(thread_id, &item_id).await })
    }

    fn claim_voice(
        &self,
        thread_id: ThreadId,
        item_id: String,
        attempt_id: String,
    ) -> ThreadStoreFuture<'_, Option<VoiceAdmissionReceipt>> {
        queue_future(async move { self.queue().claim_voice(thread_id, &item_id, &attempt_id).await })
    }

    fn finish_voice_claim(
        &self,
        thread_id: ThreadId,
        item_id: String,
        attempt_id: String,
        outcome: VoiceClaimOutcome,
    ) -> ThreadStoreFuture<'_, VoiceAdmissionReceipt> {
        queue_future(async move {
            self.queue().finish_voice_claim(thread_id, &item_id, &attempt_id, outcome).await
        })
    }

    fn reconcile_voice_started(
        &self,
        thread_id: ThreadId,
        origin_id: String,
        client_id: String,
        turn_id: String,
    ) -> ThreadStoreFuture<'_, VoiceAdmissionReceipt> {
        queue_future(async move {
            self.queue().reconcile_voice_started(thread_id, &origin_id, &client_id, &turn_id).await
        })
    }

    fn change_version(&self) -> ThreadStoreFuture<'_, i64> {
        queue_future(self.queue().change_version())
    }

    fn changes_since<'a>(
        &'a self,
        revision: i64,
        thread_ids: &'a [ThreadId],
    ) -> ThreadStoreFuture<'a, Vec<(ThreadId, i64)>> {
        queue_future(self.queue().changes_since(revision, thread_ids))
    }

    fn enqueue(
        &self,
        thread_id: ThreadId,
        payload: String,
    ) -> ThreadStoreFuture<'_, QueuedUserSubmissionRecord> {
        Box::pin(async move {
            self.queue()
                .enqueue(thread_id, &payload)
                .await
                .map_err(|error| match error.downcast_ref::<sqlx::Error>() {
                    Some(sqlx::Error::RowNotFound) => ThreadStoreError::InvalidRequest {
                        message: format!(
                            "queue cannot contain more than {MAX_QUEUE_ITEMS} submissions"
                        ),
                    },
                    _ => ThreadStoreError::Internal {
                        message: format!("queue storage failed: {error}"),
                    },
                })
        })
    }

    fn list_page(
        &self,
        thread_id: ThreadId,
        offset: usize,
        limit: usize,
    ) -> ThreadStoreFuture<'_, Vec<QueuedUserSubmissionRecord>> {
        queue_future(self.queue().list_page(thread_id, offset, limit))
    }

    fn update(
        &self,
        thread_id: ThreadId,
        item_id: String,
        payload: String,
    ) -> ThreadStoreFuture<'_, Option<QueuedUserSubmissionRecord>> {
        queue_future(async move { self.queue().update(thread_id, &item_id, &payload).await })
    }

    fn delete(&self, thread_id: ThreadId, item_id: String) -> ThreadStoreFuture<'_, bool> {
        queue_future(async move { self.queue().delete(thread_id, &item_id).await })
    }

    fn reorder(&self, thread_id: ThreadId, item_ids: Vec<String>) -> ThreadStoreFuture<'_, ()> {
        Box::pin(async move {
            self.queue()
                .reorder(thread_id, &item_ids)
                .await
                .map_err(|error| match error.downcast_ref::<std::io::Error>() {
                    Some(error) if error.kind() == std::io::ErrorKind::InvalidInput => {
                        ThreadStoreError::InvalidRequest {
                            message: error.to_string(),
                        }
                    }
                    _ => ThreadStoreError::Internal {
                        message: format!("queue storage failed: {error}"),
                    },
                })
        })
    }
}

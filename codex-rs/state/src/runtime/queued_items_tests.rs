use super::*;
use crate::migrations::QUEUE_MIGRATOR;
use crate::runtime::test_support::test_thread_metadata;
use crate::runtime::test_support::unique_temp_dir;
use codex_utils_absolute_path::test_support::PathExt;
use pretty_assertions::assert_eq;
use sqlx::migrate::Migrator;
use std::borrow::Cow;

fn voice_origin(id: &str) -> crate::VoiceQueueOrigin {
    crate::VoiceQueueOrigin {
        native_session_id: "native-session-A".to_string(),
        voice_session_generation: 1,
        origin_id: id.to_string(),
        handoff_id: Some("handoff".to_string()),
        item_id: Some("item".to_string()),
    }
}

fn voice_payload(origin: &crate::VoiceQueueOrigin, text: &str) -> String {
    serde_json::to_string(&codex_protocol::turn_input::TurnInput::UserInput {
        content: vec![codex_protocol::user_input::UserInput::Text {
            text: text.to_string(),
            text_elements: Vec::new(),
        }],
        client_id: Some(origin.origin_id.clone()),
    })
    .unwrap()
}

#[tokio::test]
async fn voice_origin_replay_cannot_replace_input_or_recreate_a_started_item() {
    use crate::VoiceAdmissionResult;
    use crate::VoiceClaimOutcome;
    use crate::VoiceEnqueueOutcome;
    let (runtime, thread_id) = runtime_with_thread().await;
    let queue = runtime.thread_queue();
    let origin = voice_origin("generation-1/utterance-1");
    let payload = voice_payload(&origin, "first");
    let VoiceEnqueueOutcome::Inserted(receipt) = queue
        .enqueue_voice(thread_id, &payload, &origin)
        .await
        .unwrap()
    else {
        panic!("first origin must insert");
    };
    assert_eq!(
        VoiceEnqueueOutcome::Existing(receipt.clone()),
        queue
            .enqueue_voice(thread_id, &payload, &origin)
            .await
            .unwrap(),
    );
    assert!(
        queue
            .enqueue_voice(thread_id, &voice_payload(&origin, "changed"), &origin)
            .await
            .is_err()
    );
    assert_eq!(origin.native_session_id, receipt.native_session_id);
    let mut wrong_session = origin.clone();
    wrong_session.native_session_id = "native-session-B".to_string();
    assert!(
        queue
            .enqueue_voice(thread_id, &payload, &wrong_session)
            .await
            .is_err()
    );
    let mut blank_session = origin.clone();
    blank_session.native_session_id = " ".to_string();
    assert!(
        queue
            .enqueue_voice(thread_id, &payload, &blank_session)
            .await
            .is_err()
    );
    assert_eq!(
        Some(receipt.clone()),
        queue
            .get_voice_receipt(thread_id, &origin.origin_id)
            .await
            .unwrap()
    );
    queue
        .claim_voice(thread_id, &receipt.queued_item_id, "attempt-1")
        .await
        .unwrap()
        .unwrap();
    let started = queue
        .finish_voice_claim(
            thread_id,
            &receipt.queued_item_id,
            "attempt-1",
            VoiceClaimOutcome::Started {
                turn_id: "core-turn-1".to_string(),
            },
        )
        .await
        .unwrap();
    let mut expected = receipt;
    expected.admission_result = VoiceAdmissionResult::Started;
    expected.attempt_id = Some("attempt-1".to_string());
    expected.turn_id = Some("core-turn-1".to_string());
    assert_eq!(expected, started);
    assert!(
        queue
            .list_page(thread_id, /*offset*/ 0, /*limit*/ 1)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        VoiceEnqueueOutcome::Existing(started),
        queue
            .enqueue_voice(thread_id, &payload, &origin)
            .await
            .unwrap()
    );
    assert!(
        queue
            .list_page(thread_id, /*offset*/ 0, /*limit*/ 1)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn voice_claim_survives_restart_and_only_positive_reconciliation_dequeues() {
    use crate::VoiceAdmissionResult;
    use crate::VoiceClaimOutcome;
    use crate::VoiceEnqueueOutcome;
    let (runtime, thread_id) = runtime_with_thread().await;
    let origin = voice_origin("generation-1/utterance-2");
    let payload = voice_payload(&origin, "once");
    let VoiceEnqueueOutcome::Inserted(receipt) = runtime
        .thread_queue()
        .enqueue_voice(thread_id, &payload, &origin)
        .await
        .unwrap()
    else {
        panic!("first origin must insert");
    };
    let other = StateRuntime::init(runtime.sqlite().clone(), "test-provider".to_string())
        .await
        .unwrap();
    let (first, second) = tokio::join!(
        runtime
            .thread_queue()
            .claim_voice(thread_id, &receipt.queued_item_id, "attempt-a"),
        other
            .thread_queue()
            .claim_voice(thread_id, &receipt.queued_item_id, "attempt-b"),
    );
    let claims = [first.unwrap(), second.unwrap()];
    assert_eq!(1, claims.iter().filter(|claim| claim.is_some()).count());
    let claimed = claims.into_iter().flatten().next().unwrap();
    let reopened = StateRuntime::init(runtime.sqlite().clone(), "test-provider".to_string())
        .await
        .unwrap();
    let queue = reopened.thread_queue();
    assert_eq!(
        Some(claimed.clone()),
        queue
            .get_voice_receipt(thread_id, &origin.origin_id)
            .await
            .unwrap()
    );
    assert_eq!(
        None,
        queue
            .claim_voice(thread_id, &receipt.queued_item_id, "retry")
            .await
            .unwrap()
    );
    assert!(
        queue
            .delete(thread_id, &receipt.queued_item_id)
            .await
            .is_err()
    );
    assert_eq!(
        None,
        queue
            .update(
                thread_id,
                &receipt.queued_item_id,
                &voice_payload(&origin, "replace")
            )
            .await
            .unwrap()
    );
    let attempt = claimed.attempt_id.as_deref().unwrap();
    let ambiguous = queue
        .finish_voice_claim(
            thread_id,
            &receipt.queued_item_id,
            attempt,
            VoiceClaimOutcome::Ambiguous {
                reason: "lost receipt".to_string(),
            },
        )
        .await
        .unwrap();
    let mut expected = claimed;
    expected.admission_result = VoiceAdmissionResult::Ambiguous;
    expected.reason = Some("lost receipt".to_string());
    assert_eq!(expected, ambiguous);
    assert_eq!(
        None,
        queue
            .claim_voice(thread_id, &receipt.queued_item_id, "retry")
            .await
            .unwrap()
    );
    assert!(
        queue
            .reconcile_voice_started(
                thread_id,
                &origin.native_session_id,
                &origin.origin_id,
                "wrong-client",
                "turn"
            )
            .await
            .is_err()
    );
    assert!(
        queue
            .reconcile_voice_started(
                thread_id,
                "native-session-B",
                &origin.origin_id,
                &origin.origin_id,
                "turn"
            )
            .await
            .is_err()
    );
    let started = queue
        .reconcile_voice_started(
            thread_id,
            &origin.native_session_id,
            &origin.origin_id,
            &origin.origin_id,
            "turn",
        )
        .await
        .unwrap();
    expected.admission_result = VoiceAdmissionResult::Started;
    expected.reason = None;
    expected.turn_id = Some("turn".to_string());
    assert_eq!(expected, started);
    assert_eq!(
        started,
        queue
            .reconcile_voice_started(
                thread_id,
                &origin.native_session_id,
                &origin.origin_id,
                &origin.origin_id,
                "turn"
            )
            .await
            .unwrap()
    );
    assert!(
        queue
            .reconcile_voice_started(
                thread_id,
                &origin.native_session_id,
                &origin.origin_id,
                &origin.origin_id,
                "different-turn"
            )
            .await
            .is_err()
    );
    assert!(
        queue
            .list_page(thread_id, /*offset*/ 0, /*limit*/ 1)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn voice_busy_release_is_attempt_scoped_and_cancellation_keeps_tombstone() {
    use crate::VoiceAdmissionResult;
    use crate::VoiceClaimOutcome;
    use crate::VoiceEnqueueOutcome;
    let (runtime, thread_id) = runtime_with_thread().await;
    let queue = runtime.thread_queue();
    let origin = voice_origin("generation-1/utterance-3");
    let payload = voice_payload(&origin, "later");
    let VoiceEnqueueOutcome::Inserted(receipt) = queue
        .enqueue_voice(thread_id, &payload, &origin)
        .await
        .unwrap()
    else {
        panic!("first origin must insert");
    };
    queue
        .claim_voice(thread_id, &receipt.queued_item_id, "busy-attempt")
        .await
        .unwrap()
        .unwrap();
    let released = queue
        .finish_voice_claim(
            thread_id,
            &receipt.queued_item_id,
            "busy-attempt",
            VoiceClaimOutcome::RetryableRejection {
                reason: "NotIdle".to_string(),
            },
        )
        .await
        .unwrap();
    let mut expected = receipt.clone();
    expected.reason = Some("NotIdle".to_string());
    assert_eq!(expected, released);
    queue
        .claim_voice(thread_id, &receipt.queued_item_id, "new-attempt")
        .await
        .unwrap()
        .unwrap();
    assert!(
        queue
            .finish_voice_claim(
                thread_id,
                &receipt.queued_item_id,
                "busy-attempt",
                VoiceClaimOutcome::Started {
                    turn_id: "wrong".to_string()
                },
            )
            .await
            .is_err()
    );
    queue
        .finish_voice_claim(
            thread_id,
            &receipt.queued_item_id,
            "new-attempt",
            VoiceClaimOutcome::RetryableRejection {
                reason: "ServerDraining".to_string(),
            },
        )
        .await
        .unwrap();
    assert!(
        queue
            .delete(thread_id, &receipt.queued_item_id)
            .await
            .unwrap()
    );
    expected.admission_result = VoiceAdmissionResult::Cancelled;
    expected.reason = Some("ServerDraining".to_string());
    assert_eq!(
        Some(expected.clone()),
        queue
            .get_voice_receipt(thread_id, &origin.origin_id)
            .await
            .unwrap()
    );
    assert_eq!(
        VoiceEnqueueOutcome::Existing(expected),
        queue
            .enqueue_voice(thread_id, &payload, &origin)
            .await
            .unwrap()
    );
    assert!(
        queue
            .list_page(thread_id, /*offset*/ 0, /*limit*/ 1)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn migrating_receipts_cannot_invent_a_native_session() {
    let home = unique_temp_dir();
    tokio::fs::create_dir_all(&home).await.unwrap();
    let sqlite = crate::SqliteConfig::new_for_testing(home.as_path().abs());
    let old_queue_migrator = Migrator {
        migrations: Cow::Owned(QUEUE_MIGRATOR.migrations.iter().take(3).cloned().collect()),
        ignore_missing: false,
        locking: true,
        no_tx: false,
        table_name: QUEUE_MIGRATOR.table_name.clone(),
        create_schemas: QUEUE_MIGRATOR.create_schemas.clone(),
    };
    let pool = sqlite
        .open_read_write_pool(&sqlite.queue_db_path())
        .await
        .unwrap();
    old_queue_migrator.run(&pool).await.unwrap();
    let thread_id = ThreadId::new();
    sqlx::query(
        "INSERT INTO voice_admission_receipts
         (thread_id, origin_id, voice_session_generation, queued_item_id, client_id, payload_json, admission_result)
         VALUES (?, 'old-origin', 1, 'old-queue', 'old-origin', '{}', 'Queued')",
    ).bind(thread_id.to_string()).execute(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO queued_items
         (id, thread_id, payload_json, queue_order, created_at_ms, updated_at_ms)
         VALUES ('old-queue', ?, '{}', 0, 0, 0)",
    )
    .bind(thread_id.to_string())
    .execute(&pool)
    .await
    .unwrap();
    QUEUE_MIGRATOR.run(&pool).await.unwrap();
    let native_session: Option<String> = sqlx::query_scalar(
        "SELECT native_session_id FROM voice_admission_receipts WHERE origin_id = 'old-origin'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(None, native_session);
    assert!(sqlx::query(
        "UPDATE voice_admission_receipts SET native_session_id = 'invented' WHERE origin_id = 'old-origin'",
    ).execute(&pool).await.is_err());
    pool.close().await;
    let runtime = StateRuntime::init(sqlite, "test-provider".to_string())
        .await
        .unwrap();
    let queue = runtime.thread_queue();
    assert!(
        queue
            .get_voice_receipt(thread_id, "old-origin")
            .await
            .is_err()
    );
    assert_eq!(
        None,
        queue
            .claim_voice(thread_id, "old-queue", "attempt")
            .await
            .unwrap()
    );
    assert!(
        queue
            .reconcile_voice_started(thread_id, "invented", "old-origin", "old-origin", "turn")
            .await
            .is_err()
    );
    assert_eq!(1, queue.list_page(thread_id, 0, 10).await.unwrap().len());
}

async fn runtime_with_thread() -> (Arc<StateRuntime>, ThreadId) {
    let home = unique_temp_dir();
    let runtime = StateRuntime::init(
        crate::SqliteConfig::new_for_testing(home.as_path().abs()),
        "test-provider".to_string(),
    )
    .await
    .expect("state runtime");
    let thread_id = ThreadId::new();
    let metadata = test_thread_metadata(home.as_path(), thread_id, home.clone());
    runtime.upsert_thread(&metadata).await.unwrap();
    (runtime, thread_id)
}

#[tokio::test]
async fn competing_runtimes_preserve_fifo_queue_order() {
    let (runtime, thread_id) = runtime_with_thread().await;
    let other = StateRuntime::init(runtime.sqlite().clone(), "test-provider".to_string())
        .await
        .unwrap();
    let queue = runtime.thread_queue();
    let other_queue = other.thread_queue();
    let (first, second) = tokio::join!(
        queue.enqueue(thread_id, r#"{"first":true}"#),
        other_queue.enqueue(thread_id, r#"{"second":true}"#),
    );
    let mut expected = vec![first.unwrap(), second.unwrap()];
    expected.sort_by(|first, second| first.id.cmp(&second.id));
    let mut actual = queue
        .list_page(thread_id, /*offset*/ 0, /*limit*/ 2)
        .await
        .unwrap();
    actual.sort_by(|first, second| first.id.cmp(&second.id));
    assert_eq!(expected, actual);
}

#[tokio::test]
async fn migrating_existing_queue_backfills_thread_revisions() {
    let home = unique_temp_dir();
    tokio::fs::create_dir_all(&home).await.unwrap();
    let sqlite = crate::SqliteConfig::new_for_testing(home.as_path().abs());
    let queue_path = sqlite.queue_db_path();
    let old_queue_migrator = Migrator {
        migrations: Cow::Owned(vec![QUEUE_MIGRATOR.migrations[0].clone()]),
        ignore_missing: false,
        locking: true,
        no_tx: false,
        table_name: QUEUE_MIGRATOR.table_name.clone(),
        create_schemas: QUEUE_MIGRATOR.create_schemas.clone(),
    };
    let pool = sqlite.open_read_write_pool(&queue_path).await.unwrap();
    old_queue_migrator.run(&pool).await.unwrap();

    let thread_id = ThreadId::new();
    let queued = QueuedUserSubmissionRecord {
        id: Uuid::now_v7().to_string(),
        thread_id,
        payload: r#"{"existing":true}"#.to_string(),
    };
    sqlx::query(
        "INSERT INTO queued_items
         (id, thread_id, payload_json, queue_order, created_at_ms, updated_at_ms)
         VALUES (?, ?, ?, 0, 0, 0)",
    )
    .bind(&queued.id)
    .bind(thread_id.to_string())
    .bind(&queued.payload)
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;

    let runtime = StateRuntime::init(sqlite, "test-provider".to_string())
        .await
        .unwrap();
    let queue = runtime.thread_queue();
    assert_eq!(
        vec![(thread_id, 1)],
        queue
            .changes_since(/*revision*/ 0, &[thread_id])
            .await
            .unwrap()
    );
    assert_eq!(
        vec![queued],
        queue
            .list_page(thread_id, /*offset*/ 0, /*limit*/ 1)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn queue_revisions_identify_changed_threads_after_updates_and_deletions() {
    let (runtime, thread_id) = runtime_with_thread().await;
    let queue = runtime.thread_queue();
    let first = queue.enqueue(thread_id, r#"{"first":true}"#).await.unwrap();
    let first_revision = queue
        .changes_since(/*revision*/ 0, &[thread_id])
        .await
        .unwrap()[0]
        .1;
    queue
        .update(thread_id, &first.id, r#"{"updated":true}"#)
        .await
        .unwrap();
    let updated_revision = queue
        .changes_since(first_revision, &[thread_id])
        .await
        .unwrap()[0]
        .1;
    let other_thread_id = ThreadId::new();
    queue
        .enqueue(other_thread_id, r#"{"other":true}"#)
        .await
        .unwrap();
    let newly_loaded_changes = queue
        .changes_since(/*revision*/ 0, &[other_thread_id])
        .await
        .unwrap();
    assert_eq!(
        vec![(thread_id, updated_revision), newly_loaded_changes[0]],
        queue
            .changes_since(first_revision, &[thread_id, other_thread_id])
            .await
            .unwrap()
    );
    assert!(queue.delete(thread_id, &first.id).await.unwrap());
    assert!(
        queue
            .changes_since(updated_revision, &[thread_id])
            .await
            .unwrap()
            .iter()
            .any(|(changed_thread, _)| *changed_thread == thread_id)
    );
}

#[tokio::test]
async fn fifo_dispatch_preserves_edits_reordering_and_pagination() {
    let (runtime, thread_id) = runtime_with_thread().await;
    let queue = runtime.thread_queue();
    let first = queue.enqueue(thread_id, r#"{"n":1}"#).await.unwrap();
    let second = queue.enqueue(thread_id, r#"{"n":2}"#).await.unwrap();
    let third = queue.enqueue(thread_id, r#"{"n":3}"#).await.unwrap();

    let updated = queue
        .update(thread_id, &first.id, r#"{"n":"edited"}"#)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.id, updated.id);
    let error = queue
        .reorder(thread_id, std::slice::from_ref(&first.id))
        .await
        .unwrap_err();
    assert_eq!(
        std::io::ErrorKind::InvalidInput,
        error.downcast_ref::<std::io::Error>().unwrap().kind()
    );

    let ordered_ids = vec![third.id, first.id, second.id];
    queue.reorder(thread_id, &ordered_ids).await.unwrap();

    let items = queue
        .list_page(thread_id, /*offset*/ 0, /*limit*/ 3)
        .await
        .unwrap();
    let page = queue
        .list_page(thread_id, /*offset*/ 1, /*limit*/ 1)
        .await
        .unwrap();
    assert_eq!(vec![items[1].clone()], page);
    assert_eq!(r#"{"n":"edited"}"#, items[1].payload);

    for item in items {
        assert!(queue.delete(thread_id, &item.id).await.unwrap());
    }
    assert!(
        queue
            .list_page(thread_id, /*offset*/ 0, /*limit*/ 1)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn queue_operations_cannot_mutate_another_threads_messages() {
    let (runtime, thread_id) = runtime_with_thread().await;
    let queue = runtime.thread_queue();
    let first = queue.enqueue(thread_id, r#"{"n":1}"#).await.unwrap();
    let other_thread_id = ThreadId::new();
    let other = queue.enqueue(other_thread_id, r#"{"n":2}"#).await.unwrap();
    let other_id = &other.id;

    assert_eq!(
        None,
        queue
            .update(thread_id, other_id, r#"{"n":3}"#)
            .await
            .unwrap()
    );
    assert!(!queue.delete(thread_id, other_id).await.unwrap());
    assert!(
        queue
            .reorder(thread_id, std::slice::from_ref(other_id))
            .await
            .is_err()
    );
    let (items, other_items) = tokio::join!(
        queue.list_page(thread_id, /*offset*/ 0, /*limit*/ 1),
        queue.list_page(other_thread_id, /*offset*/ 0, /*limit*/ 1),
    );
    assert_eq!(
        (vec![first], vec![other]),
        (items.unwrap(), other_items.unwrap())
    );
}

#[tokio::test]
async fn deleting_a_thread_removes_its_queue() {
    let (runtime, thread_id) = runtime_with_thread().await;
    runtime
        .thread_queue()
        .enqueue(thread_id, r#"{"n":1}"#)
        .await
        .unwrap();

    assert_eq!(1, runtime.delete_thread(thread_id).await.unwrap());
    assert!(
        runtime
            .thread_queue()
            .list_page(thread_id, /*offset*/ 0, /*limit*/ 1)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn concurrent_inserts_enforce_the_queue_limit() {
    let (runtime, thread_id) = runtime_with_thread().await;
    let other = StateRuntime::init(runtime.sqlite().clone(), "test-provider".to_string())
        .await
        .unwrap();

    for _ in 0..MAX_QUEUE_ITEMS - 1 {
        runtime
            .thread_queue()
            .enqueue(thread_id, r#"{"n":1}"#)
            .await
            .unwrap();
    }
    let (first, second) = tokio::join!(
        runtime.thread_queue().enqueue(thread_id, r#"{"n":2}"#),
        other.thread_queue().enqueue(thread_id, r#"{"n":3}"#),
    );
    assert_ne!(first.is_ok(), second.is_ok());
    assert_eq!(
        MAX_QUEUE_ITEMS,
        runtime
            .thread_queue()
            .list_page(thread_id, /*offset*/ 0, /*limit*/ MAX_QUEUE_ITEMS)
            .await
            .unwrap()
            .len()
    );
}

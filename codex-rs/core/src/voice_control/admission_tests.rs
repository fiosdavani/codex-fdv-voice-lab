use super::*;
use pretty_assertions::assert_eq;
use tokio::sync::Mutex as AsyncMutex;

fn request(
    origin: InputOrigin,
    authority: SubmissionAuthority,
    voice_lease_id: Option<VoiceLeaseId>,
) -> AdmissionRequest {
    AdmissionRequest {
        provenance: InputProvenance {
            origin,
            effect: InputEffect::StartTurn,
        },
        authority,
        voice_lease_id,
    }
}

#[tokio::test]
async fn voice_disabled_preserves_upstream_admission_baseline() {
    let active_turn = AsyncMutex::new(None);
    let guard = active_turn.lock().await;
    let controller = AdmissionController::default();

    let ticket = controller
        .issue_ticket(
            &guard,
            request(
                InputOrigin::HumanExternalClient,
                SubmissionAuthority::ExternalClient,
                None,
            ),
        )
        .expect("baseline ticket");

    assert_eq!(ticket.commitment(), CommitmentState::NotCommitted);
    assert_eq!(
        controller.commit(&guard, ticket).expect("commit"),
        CommittedAdmission {
            commitment: CommitmentState::Committed
        }
    );
}

#[tokio::test]
async fn active_lease_fails_closed_for_unknown_and_external_human_input() {
    let active_turn = AsyncMutex::new(None);
    let guard = active_turn.lock().await;
    let controller = AdmissionController::default();
    let lease_id = VoiceLeaseId::new("voice-session");
    controller
        .begin_lease_acquire(&guard, lease_id.clone())
        .expect("acquire");
    controller
        .activate_lease(&guard, &lease_id)
        .expect("activate");

    let results = [
        controller.issue_ticket(
            &guard,
            request(InputOrigin::Unknown, SubmissionAuthority::Unknown, None),
        ),
        controller.issue_ticket(
            &guard,
            request(
                InputOrigin::HumanExternalClient,
                SubmissionAuthority::ExternalClient,
                None,
            ),
        ),
    ];

    assert!(
        results
            .into_iter()
            .all(|result| result.unwrap_err() == NotSubmittedReason::PermissionDenied)
    );
}

#[tokio::test]
async fn administrative_recovery_cannot_submit_human_input() {
    let active_turn = AsyncMutex::new(None);
    let guard = active_turn.lock().await;
    let controller = AdmissionController::default();

    assert_eq!(
        controller
            .issue_ticket(
                &guard,
                request(
                    InputOrigin::HumanExternalClient,
                    SubmissionAuthority::AdministrativeRecovery,
                    None,
                ),
            )
            .unwrap_err(),
        NotSubmittedReason::PermissionDenied
    );
}

#[tokio::test]
async fn changed_lease_generation_rejects_a_stale_ticket() {
    let active_turn = AsyncMutex::new(None);
    let guard = active_turn.lock().await;
    let controller = AdmissionController::default();
    let ticket = controller
        .issue_ticket(
            &guard,
            request(
                InputOrigin::HumanExternalClient,
                SubmissionAuthority::ExternalClient,
                None,
            ),
        )
        .expect("ticket");
    let lease_id = VoiceLeaseId::new("voice-session");
    controller
        .begin_lease_acquire(&guard, lease_id)
        .expect("acquire");

    assert_eq!(
        controller.commit(&guard, ticket).unwrap_err(),
        NotSubmittedReason::StaleGeneration
    );
}

#[tokio::test]
async fn changed_idle_epoch_rejects_a_stale_ticket() {
    let active_turn = AsyncMutex::new(None);
    let guard = active_turn.lock().await;
    let controller = AdmissionController::default();
    let ticket = controller
        .issue_ticket(
            &guard,
            request(
                InputOrigin::HumanExternalClient,
                SubmissionAuthority::ExternalClient,
                None,
            ),
        )
        .expect("ticket");
    controller.record_idle_transition(&guard);

    assert_eq!(
        controller.commit(&guard, ticket).unwrap_err(),
        NotSubmittedReason::StaleIdleEpoch
    );
}

#[tokio::test]
async fn expiration_before_commit_is_definitively_not_submitted() {
    let active_turn = AsyncMutex::new(None);
    let guard = active_turn.lock().await;
    let controller = AdmissionController::default();
    let ticket = controller
        .issue_ticket(
            &guard,
            request(
                InputOrigin::HumanExternalClient,
                SubmissionAuthority::ExternalClient,
                None,
            ),
        )
        .expect("ticket");

    assert_eq!(
        ticket.expire(),
        SubmissionResolution::NotSubmitted(NotSubmittedReason::TicketExpired)
    );
}

#[tokio::test]
async fn waiter_loss_after_commit_is_an_unknown_result_not_busy() {
    let active_turn = AsyncMutex::new(None);
    let guard = active_turn.lock().await;
    let controller = AdmissionController::default();
    let ticket = controller
        .issue_ticket(
            &guard,
            request(
                InputOrigin::HumanExternalClient,
                SubmissionAuthority::ExternalClient,
                None,
            ),
        )
        .expect("ticket");
    let committed = controller.commit(&guard, ticket).expect("commit");

    assert_eq!(
        committed.result_unknown(),
        SubmissionResolution::UnknownResult
    );
}

#[tokio::test]
async fn stopping_fence_requires_the_exact_finalizer() {
    let active_turn = AsyncMutex::new(None);
    let guard = active_turn.lock().await;
    let controller = AdmissionController::default();
    let generation = controller.begin_stopping(&guard, "turn-a".to_string());

    assert!(!controller.finish_stopping(&guard, "turn-b", generation));
    assert_eq!(
        controller
            .issue_ticket(
                &guard,
                request(
                    InputOrigin::HumanExternalClient,
                    SubmissionAuthority::ExternalClient,
                    None,
                ),
            )
            .unwrap_err(),
        NotSubmittedReason::Stopping
    );
    assert!(controller.finish_stopping(&guard, "turn-a", generation));
}

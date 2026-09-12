use super::*;
use crate::voice_control::InputEffect;
use crate::voice_control::InputOrigin;
use pretty_assertions::assert_eq;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::Mutex as AsyncMutex;

fn request(
    origin: InputOrigin,
    effect: InputEffect,
    authority: SubmissionAuthority,
    voice_lease_id: Option<VoiceLeaseId>,
) -> AdmissionRequest {
    AdmissionRequest {
        provenance: InputProvenance { origin, effect },
        authority,
        voice_lease_id,
    }
}

fn issue(
    controller: &AdmissionController,
    guard: &MutexGuard<'_, Option<ActiveTurn>>,
    request: AdmissionRequest,
    now: Instant,
) -> Result<AdmissionTicket, NotSubmittedReason> {
    controller.issue_ticket(guard, request, now, Duration::from_secs(/* secs */ 10))
}

#[tokio::test]
async fn voice_disabled_preserves_external_upstream_baseline() {
    let active_turn = AsyncMutex::new(None);
    let guard = active_turn.lock().await;
    let controller = AdmissionController::default();
    let now = Instant::now();
    let ticket = issue(
        &controller,
        &guard,
        request(
            InputOrigin::HumanExternalClient,
            InputEffect::StartTurn,
            SubmissionAuthority::ExternalClient,
            None,
        ),
        now,
    )
    .expect("baseline ticket");

    assert_eq!(ticket.commitment(), CommitmentState::Prepared);
    let boundary = controller
        .enter_effect_boundary(&guard, ticket, now)
        .expect("effect boundary");
    assert_eq!(boundary.commitment, CommitmentState::Committing);
    assert_eq!(boundary.result_known(), SubmissionResolution::KnownResult);
}

#[tokio::test]
async fn active_lease_governs_external_unknown_and_voice_effects() {
    let active_turn = AsyncMutex::new(None);
    let guard = active_turn.lock().await;
    let controller = AdmissionController::default();
    let owner = VoiceLeaseId::new("owner");
    controller
        .begin_lease_acquire(&guard, owner.clone())
        .expect("acquire");
    controller.activate_lease(&guard, &owner).expect("activate");
    let now = Instant::now();
    let denied = [
        request(
            InputOrigin::HumanExternalClient,
            InputEffect::StartTurn,
            SubmissionAuthority::ExternalClient,
            None,
        ),
        request(
            InputOrigin::HumanExternalClient,
            InputEffect::Steer,
            SubmissionAuthority::ExternalClient,
            None,
        ),
        request(
            InputOrigin::HumanVoice,
            InputEffect::Say,
            SubmissionAuthority::Voice,
            Some(VoiceLeaseId::new("wrong")),
        ),
        request(
            InputOrigin::Unknown,
            InputEffect::StartTurn,
            SubmissionAuthority::Unknown,
            None,
        ),
        request(
            InputOrigin::Unknown,
            InputEffect::Continue,
            SubmissionAuthority::Unknown,
            None,
        ),
    ];

    assert!(denied.into_iter().all(|request| {
        issue(&controller, &guard, request, now).unwrap_err()
            == NotSubmittedReason::PermissionDenied
    }));
    assert!(
        issue(
            &controller,
            &guard,
            request(
                InputOrigin::HumanVoice,
                InputEffect::Say,
                SubmissionAuthority::Voice,
                Some(owner),
            ),
            now,
        )
        .is_ok()
    );
}

#[tokio::test]
async fn voice_input_without_a_lease_is_denied() {
    let active_turn = AsyncMutex::new(None);
    let guard = active_turn.lock().await;
    let controller = AdmissionController::default();

    assert_eq!(
        issue(
            &controller,
            &guard,
            request(
                InputOrigin::HumanVoice,
                InputEffect::Say,
                SubmissionAuthority::Voice,
                None,
            ),
            Instant::now(),
        )
        .unwrap_err(),
        NotSubmittedReason::PermissionDenied
    );
}

#[tokio::test]
async fn administrative_recovery_never_receives_an_input_permit() {
    let active_turn = AsyncMutex::new(None);
    let guard = active_turn.lock().await;
    let controller = AdmissionController::default();
    let now = Instant::now();
    let attempts = [
        (InputOrigin::HumanExternalClient, InputEffect::StartTurn),
        (InputOrigin::HumanVoice, InputEffect::Say),
        (InputOrigin::HumanExternalClient, InputEffect::Steer),
        (InputOrigin::Unknown, InputEffect::Continue),
        (InputOrigin::InternalAgent, InputEffect::Continue),
        (InputOrigin::CorrelatedResponse, InputEffect::Continue),
    ];

    assert!(attempts.into_iter().all(|(origin, effect)| {
        issue(
            &controller,
            &guard,
            request(
                origin,
                effect,
                SubmissionAuthority::AdministrativeRecovery,
                None,
            ),
            now,
        )
        .unwrap_err()
            == NotSubmittedReason::PermissionDenied
    }));
}

#[tokio::test]
async fn stale_generation_and_idle_epoch_are_rejected() {
    let active_turn = AsyncMutex::new(None);
    let guard = active_turn.lock().await;
    let controller = AdmissionController::default();
    let now = Instant::now();
    let external = || {
        request(
            InputOrigin::HumanExternalClient,
            InputEffect::StartTurn,
            SubmissionAuthority::ExternalClient,
            None,
        )
    };
    let generation_ticket = issue(&controller, &guard, external(), now).expect("ticket");
    controller
        .begin_lease_acquire(&guard, VoiceLeaseId::new("voice-session"))
        .expect("acquire");
    assert_eq!(
        controller
            .enter_effect_boundary(&guard, generation_ticket, now)
            .unwrap_err(),
        NotSubmittedReason::StaleGeneration
    );

    let controller = AdmissionController::default();
    let epoch_ticket = issue(&controller, &guard, external(), now).expect("ticket");
    controller.record_idle_transition(&guard);
    assert_eq!(
        controller
            .enter_effect_boundary(&guard, epoch_ticket, now)
            .unwrap_err(),
        NotSubmittedReason::StaleIdleEpoch
    );
}

#[tokio::test]
async fn deadline_is_checked_deterministically_before_the_effect_boundary() {
    let active_turn = AsyncMutex::new(None);
    let guard = active_turn.lock().await;
    let controller = AdmissionController::default();
    let now = Instant::now();
    let ticket = controller
        .issue_ticket(
            &guard,
            request(
                InputOrigin::HumanExternalClient,
                InputEffect::StartTurn,
                SubmissionAuthority::ExternalClient,
                None,
            ),
            now,
            Duration::from_secs(/* secs */ 5),
        )
        .expect("ticket");

    assert_eq!(
        controller
            .enter_effect_boundary(&guard, ticket, now + Duration::from_secs(/* secs */ 5),)
            .unwrap_err(),
        NotSubmittedReason::TicketExpired
    );
}

#[tokio::test]
async fn waiter_loss_after_effect_boundary_is_unknown_not_busy() {
    let active_turn = AsyncMutex::new(None);
    let guard = active_turn.lock().await;
    let controller = AdmissionController::default();
    let now = Instant::now();
    let ticket = issue(
        &controller,
        &guard,
        request(
            InputOrigin::HumanExternalClient,
            InputEffect::StartTurn,
            SubmissionAuthority::ExternalClient,
            None,
        ),
        now,
    )
    .expect("ticket");
    let boundary = controller
        .enter_effect_boundary(&guard, ticket, now)
        .expect("effect boundary");

    assert_eq!(
        boundary.result_unknown(),
        SubmissionResolution::UnknownResult
    );
}

#[tokio::test]
async fn stopping_fence_rejects_replacement_and_stale_finalizers() {
    let active_turn = AsyncMutex::new(None);
    let mut guard = active_turn.lock().await;
    let controller = AdmissionController::default();
    let generation = controller
        .begin_stopping(&guard, "turn-a".to_string())
        .expect("stopping fence");

    assert_eq!(
        controller.begin_stopping(&guard, "turn-b".to_string()),
        Err(BeginStoppingError::AlreadyStopping)
    );
    assert!(!controller.finish_stopping(&guard, "turn-b", generation));
    assert!(!controller.finish_stopping(&guard, "turn-a", generation + 1));
    *guard = Some(ActiveTurn::default());
    assert!(!controller.finish_stopping(&guard, "turn-a", generation));
    *guard = None;
    assert!(controller.finish_stopping(&guard, "turn-a", generation));
    assert!(!controller.finish_stopping(&guard, "turn-a", generation));
    let successor_generation = controller
        .begin_stopping(&guard, "turn-b".to_string())
        .expect("successor fence");
    assert!(!controller.finish_stopping(&guard, "turn-a", generation));
    assert!(controller.finish_stopping(&guard, "turn-b", successor_generation));
}

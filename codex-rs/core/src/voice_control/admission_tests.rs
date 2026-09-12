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
    assert_eq!(boundary.commitment(), CommitmentState::Committing);
    assert_eq!(
        controller.resolve_effect_known(&guard, &boundary),
        Ok(SubmissionResolution::KnownResult)
    );
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
    controller
        .record_idle_transition(&guard)
        .expect("idle transition");
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
        controller.resolve_effect_unknown(&guard, &boundary),
        Ok(SubmissionResolution::UnknownResult)
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

#[tokio::test]
async fn provenance_and_authority_must_match_explicitly() {
    let active_turn = AsyncMutex::new(None);
    let guard = active_turn.lock().await;
    let controller = AdmissionController::default();
    let now = Instant::now();
    let cases = [
        (
            InputOrigin::InternalAgent,
            SubmissionAuthority::InternalAgent,
            true,
        ),
        (
            InputOrigin::SystemContinuation,
            SubmissionAuthority::System,
            true,
        ),
        (
            InputOrigin::CorrelatedResponse,
            SubmissionAuthority::CorrelatedResponse,
            true,
        ),
        (
            InputOrigin::InternalAgent,
            SubmissionAuthority::ExternalClient,
            false,
        ),
        (
            InputOrigin::SystemContinuation,
            SubmissionAuthority::InternalAgent,
            false,
        ),
        (
            InputOrigin::CorrelatedResponse,
            SubmissionAuthority::System,
            false,
        ),
        (
            InputOrigin::HumanExternalClient,
            SubmissionAuthority::Unknown,
            false,
        ),
        (
            InputOrigin::Unknown,
            SubmissionAuthority::ExternalClient,
            false,
        ),
    ];

    assert!(cases.into_iter().all(|(origin, authority, expected)| {
        issue(
            &controller,
            &guard,
            request(origin, InputEffect::Continue, authority, None),
            now,
        )
        .is_ok()
            == expected
    }));
}

#[tokio::test]
async fn lease_acquisition_requires_idle_without_a_stopping_fence() {
    let active_turn = AsyncMutex::new(Some(ActiveTurn::default()));
    let mut guard = active_turn.lock().await;
    let controller = AdmissionController::default();

    assert_eq!(
        controller.begin_lease_acquire(&guard, VoiceLeaseId::new("busy")),
        Err(BeginLeaseAcquireError::Busy)
    );
    *guard = None;
    controller
        .begin_stopping(&guard, "stopping-turn".to_string())
        .expect("stopping fence");
    assert_eq!(
        controller.begin_lease_acquire(&guard, VoiceLeaseId::new("stopping")),
        Err(BeginLeaseAcquireError::Stopping)
    );

    let controller = AdmissionController::default();
    assert!(
        controller
            .begin_lease_acquire(&guard, VoiceLeaseId::new("idle"))
            .is_ok()
    );
}

#[tokio::test]
async fn epoch_and_stopping_generation_overflow_fail_closed() {
    let active_turn = AsyncMutex::new(None);
    let guard = active_turn.lock().await;
    let controller = AdmissionController::default();
    controller
        .authority
        .lock()
        .expect("voice authority mutex poisoned")
        .idle_epoch = IdleEpoch(u64::MAX);
    assert_eq!(
        controller.record_idle_transition(&guard),
        Err(FencingError::GenerationExhausted)
    );
    assert_eq!(
        issue(
            &controller,
            &guard,
            request(
                InputOrigin::HumanExternalClient,
                InputEffect::StartTurn,
                SubmissionAuthority::ExternalClient,
                None,
            ),
            Instant::now(),
        )
        .unwrap_err(),
        NotSubmittedReason::FencingExhausted
    );

    let controller = AdmissionController::default();
    controller
        .authority
        .lock()
        .expect("voice authority mutex poisoned")
        .stopping_generation = u64::MAX;
    assert_eq!(
        controller.begin_stopping(&guard, "turn".to_string()),
        Err(BeginStoppingError::GenerationExhausted)
    );
    assert_eq!(
        issue(
            &controller,
            &guard,
            request(
                InputOrigin::HumanExternalClient,
                InputEffect::StartTurn,
                SubmissionAuthority::ExternalClient,
                None,
            ),
            Instant::now(),
        )
        .unwrap_err(),
        NotSubmittedReason::FencingExhausted
    );

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
    controller
        .authority
        .lock()
        .expect("voice authority mutex poisoned")
        .next_effect_operation_id = u64::MAX;
    assert_eq!(
        controller
            .enter_effect_boundary(&guard, ticket, now)
            .unwrap_err(),
        NotSubmittedReason::FencingExhausted
    );
}

fn activate_voice_lease(
    controller: &AdmissionController,
    guard: &MutexGuard<'_, Option<ActiveTurn>>,
    lease_id: &VoiceLeaseId,
) {
    controller
        .begin_lease_acquire(guard, lease_id.clone())
        .expect("acquire");
    controller
        .activate_lease(guard, lease_id)
        .expect("activate");
}

fn open_voice_effect(
    controller: &AdmissionController,
    guard: &MutexGuard<'_, Option<ActiveTurn>>,
    lease_id: &VoiceLeaseId,
    now: Instant,
) -> EffectBoundary {
    let ticket = issue(
        controller,
        guard,
        request(
            InputOrigin::HumanVoice,
            InputEffect::Say,
            SubmissionAuthority::Voice,
            Some(lease_id.clone()),
        ),
        now,
    )
    .expect("voice ticket");
    controller
        .enter_effect_boundary(guard, ticket, now)
        .expect("effect boundary")
}

#[tokio::test]
async fn finish_close_requires_no_active_turn_or_stopping_fence() {
    let active_turn = AsyncMutex::new(None);
    let mut guard = active_turn.lock().await;
    let controller = AdmissionController::default();
    let lease_id = VoiceLeaseId::new("voice-session");
    activate_voice_lease(&controller, &guard, &lease_id);
    controller
        .begin_lease_close(&guard, &lease_id)
        .expect("begin close");

    *guard = Some(ActiveTurn::default());
    assert_eq!(
        controller.finish_lease_close(&guard, &lease_id),
        Err(FinishLeaseCloseError::ActiveTurn)
    );
    *guard = None;
    controller
        .begin_stopping(&guard, "turn".to_string())
        .expect("stopping fence");
    assert_eq!(
        controller.finish_lease_close(&guard, &lease_id),
        Err(FinishLeaseCloseError::Stopping)
    );
}

#[tokio::test]
async fn open_effect_boundary_prevents_close_until_exact_known_resolution() {
    let active_turn = AsyncMutex::new(None);
    let guard = active_turn.lock().await;
    let controller = AdmissionController::default();
    let lease_id = VoiceLeaseId::new("voice-session");
    activate_voice_lease(&controller, &guard, &lease_id);
    let now = Instant::now();
    let first = open_voice_effect(&controller, &guard, &lease_id, now);
    let second = open_voice_effect(&controller, &guard, &lease_id, now);
    controller
        .begin_lease_close(&guard, &lease_id)
        .expect("begin close");

    assert_eq!(
        controller.finish_lease_close(&guard, &lease_id),
        Err(FinishLeaseCloseError::EffectInFlight)
    );
    assert_eq!(
        controller.resolve_effect_known(&guard, &first),
        Ok(SubmissionResolution::KnownResult)
    );
    assert_eq!(
        controller.resolve_effect_known(&guard, &first),
        Err(ResolveEffectError::UnknownOperation)
    );
    assert_eq!(
        controller.finish_lease_close(&guard, &lease_id),
        Err(FinishLeaseCloseError::EffectInFlight)
    );
    assert_eq!(
        controller.resolve_effect_known(&guard, &second),
        Ok(SubmissionResolution::KnownResult)
    );
    assert_eq!(controller.finish_lease_close(&guard, &lease_id), Ok(()));
}

#[tokio::test]
async fn stale_handle_cannot_resolve_a_successor_operation() {
    let active_turn = AsyncMutex::new(None);
    let guard = active_turn.lock().await;
    let controller = AdmissionController::default();
    let lease_id = VoiceLeaseId::new("voice-session");
    activate_voice_lease(&controller, &guard, &lease_id);
    let now = Instant::now();
    let stale = open_voice_effect(&controller, &guard, &lease_id, now);
    controller
        .resolve_effect_known(&guard, &stale)
        .expect("resolve first effect");
    let successor = open_voice_effect(&controller, &guard, &lease_id, now);

    assert_eq!(
        controller.resolve_effect_known(&guard, &stale),
        Err(ResolveEffectError::UnknownOperation)
    );
    controller
        .begin_lease_close(&guard, &lease_id)
        .expect("begin close");
    assert_eq!(
        controller.finish_lease_close(&guard, &lease_id),
        Err(FinishLeaseCloseError::EffectInFlight)
    );
    assert_eq!(
        controller.resolve_effect_known(&guard, &successor),
        Ok(SubmissionResolution::KnownResult)
    );
}

#[tokio::test]
async fn unknown_effect_requires_recovery_and_never_releases_automatically() {
    let active_turn = AsyncMutex::new(None);
    let guard = active_turn.lock().await;
    let controller = AdmissionController::default();
    let lease_id = VoiceLeaseId::new("voice-session");
    activate_voice_lease(&controller, &guard, &lease_id);
    let boundary = open_voice_effect(&controller, &guard, &lease_id, Instant::now());
    controller
        .begin_lease_close(&guard, &lease_id)
        .expect("begin close");

    assert_eq!(
        controller.resolve_effect_unknown(&guard, &boundary),
        Ok(SubmissionResolution::UnknownResult)
    );
    assert_eq!(
        controller.resolve_effect_unknown(&guard, &boundary),
        Err(ResolveEffectError::AlreadyResolved)
    );
    assert_eq!(
        controller.finish_lease_close(&guard, &lease_id),
        Err(FinishLeaseCloseError::RecoveryRequired)
    );
    assert_eq!(
        controller.begin_lease_acquire(&guard, VoiceLeaseId::new("successor")),
        Err(BeginLeaseAcquireError::RecoveryRequired)
    );
    assert!(matches!(
        controller
            .authority
            .lock()
            .expect("voice authority mutex poisoned")
            .lease
            .state(),
        VoiceLeaseState::RecoveryRequired { .. }
    ));
}

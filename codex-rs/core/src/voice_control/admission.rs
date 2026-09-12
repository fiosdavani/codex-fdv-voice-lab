use crate::state::ActiveTurn;
use crate::voice_control::InputClass;
use crate::voice_control::InputProvenance;
use crate::voice_control::LeaseGeneration;
use crate::voice_control::SubmissionAuthority;
use crate::voice_control::VoiceLease;
use crate::voice_control::VoiceLeaseId;
use crate::voice_control::VoiceLeaseState;
use crate::voice_control::lease::VoiceLeaseTransitionError;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::MutexGuard;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct IdleEpoch(u64);

impl IdleEpoch {
    fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommitmentState {
    Prepared,
    Committing,
    Completed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AdmissionRequest {
    pub(crate) provenance: InputProvenance,
    pub(crate) authority: SubmissionAuthority,
    pub(crate) voice_lease_id: Option<VoiceLeaseId>,
}

#[derive(Debug)]
pub(crate) struct AdmissionTicket {
    lease_generation: LeaseGeneration,
    idle_epoch: IdleEpoch,
    permit: AdmissionPermit,
    commitment: CommitmentState,
    expires_at: Instant,
}

impl AdmissionTicket {
    pub(crate) fn commitment(&self) -> CommitmentState {
        self.commitment
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum AdmissionPermit {
    UpstreamBaseline,
    Voice { lease_id: VoiceLeaseId },
    NonHumanInput,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EffectBoundary {
    pub(crate) commitment: CommitmentState,
}

impl EffectBoundary {
    pub(crate) fn result_known(mut self) -> SubmissionResolution {
        self.commitment = CommitmentState::Completed;
        SubmissionResolution::KnownResult
    }

    /// Reports loss of the waiter after the effect boundary has been entered.
    ///
    /// This intentionally cannot become `Busy` or `NotSubmitted`; callers must not retry an
    /// ambiguous, potentially committed human intention.
    pub(crate) fn result_unknown(self) -> SubmissionResolution {
        SubmissionResolution::UnknownResult
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SubmissionResolution {
    NotSubmitted(NotSubmittedReason),
    KnownResult,
    UnknownResult,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NotSubmittedReason {
    Busy,
    PermissionDenied,
    StaleGeneration,
    StaleIdleEpoch,
    Stopping,
    TicketExpired,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StoppingFence {
    turn_id: String,
    generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BeginStoppingError {
    AlreadyStopping,
}

#[derive(Debug)]
struct AdmissionState {
    lease: VoiceLease,
    idle_epoch: IdleEpoch,
    stopping: Option<StoppingFence>,
    stopping_generation: u64,
}

/// Core-owned admission foundation for voice-authorized human input.
///
/// Every method that locks the short authority mutex requires the caller to supply the existing
/// `active_turn` guard. Consequently the only representable lock order is `active_turn` (L) before
/// authority (A). The synchronous critical sections perform no await, I/O, callback, or send.
/// Locks remain private to Core; future App Server integration must expose intent-level methods.
#[derive(Debug)]
pub(crate) struct AdmissionController {
    authority: Mutex<AdmissionState>,
}

impl Default for AdmissionController {
    fn default() -> Self {
        Self {
            authority: Mutex::new(AdmissionState {
                lease: VoiceLease::default(),
                idle_epoch: IdleEpoch(0),
                stopping: None,
                stopping_generation: 0,
            }),
        }
    }
}

impl AdmissionController {
    pub(crate) fn begin_lease_acquire(
        &self,
        _active_turn: &MutexGuard<'_, Option<ActiveTurn>>,
        lease_id: VoiceLeaseId,
    ) -> Result<LeaseGeneration, VoiceLeaseTransitionError> {
        self.authority
            .lock()
            .expect("voice authority mutex poisoned")
            .lease
            .begin_acquire(lease_id)
    }

    pub(crate) fn activate_lease(
        &self,
        _active_turn: &MutexGuard<'_, Option<ActiveTurn>>,
        lease_id: &VoiceLeaseId,
    ) -> Result<(), VoiceLeaseTransitionError> {
        self.authority
            .lock()
            .expect("voice authority mutex poisoned")
            .lease
            .activate(lease_id)
    }

    pub(crate) fn begin_lease_close(
        &self,
        _active_turn: &MutexGuard<'_, Option<ActiveTurn>>,
        lease_id: &VoiceLeaseId,
    ) -> Result<(), VoiceLeaseTransitionError> {
        self.authority
            .lock()
            .expect("voice authority mutex poisoned")
            .lease
            .begin_close(lease_id)
    }

    pub(crate) fn finish_lease_close(
        &self,
        _active_turn: &MutexGuard<'_, Option<ActiveTurn>>,
        lease_id: &VoiceLeaseId,
    ) -> Result<(), VoiceLeaseTransitionError> {
        self.authority
            .lock()
            .expect("voice authority mutex poisoned")
            .lease
            .finish_close(lease_id)
    }

    pub(crate) fn issue_ticket(
        &self,
        active_turn: &MutexGuard<'_, Option<ActiveTurn>>,
        request: AdmissionRequest,
        now: Instant,
        ticket_ttl: Duration,
    ) -> Result<AdmissionTicket, NotSubmittedReason> {
        if active_turn.is_some() {
            return Err(NotSubmittedReason::Busy);
        }
        let state = self
            .authority
            .lock()
            .expect("voice authority mutex poisoned");
        if state.stopping.is_some() {
            return Err(NotSubmittedReason::Stopping);
        }
        let permit = state.authorize(&request)?;
        Ok(AdmissionTicket {
            lease_generation: state.lease.generation(),
            idle_epoch: state.idle_epoch,
            permit,
            commitment: CommitmentState::Prepared,
            expires_at: now.checked_add(ticket_ttl).unwrap_or(now),
        })
    }

    /// Revalidates a prepared ticket at the point where delivery may begin.
    ///
    /// P03 does not deliver the effect. Future ingress wiring must call this exactly at its real
    /// effect boundary and then reuse the upstream start-if-idle primitive.
    pub(crate) fn enter_effect_boundary(
        &self,
        active_turn: &MutexGuard<'_, Option<ActiveTurn>>,
        mut ticket: AdmissionTicket,
        now: Instant,
    ) -> Result<EffectBoundary, NotSubmittedReason> {
        if now >= ticket.expires_at {
            return Err(NotSubmittedReason::TicketExpired);
        }
        if active_turn.is_some() {
            return Err(NotSubmittedReason::Busy);
        }
        let state = self
            .authority
            .lock()
            .expect("voice authority mutex poisoned");
        if state.stopping.is_some() {
            return Err(NotSubmittedReason::Stopping);
        }
        if ticket.lease_generation != state.lease.generation()
            || !state.permit_remains_valid(&ticket.permit)
        {
            return Err(NotSubmittedReason::StaleGeneration);
        }
        if ticket.idle_epoch != state.idle_epoch {
            return Err(NotSubmittedReason::StaleIdleEpoch);
        }
        ticket.commitment = CommitmentState::Committing;
        Ok(EffectBoundary {
            commitment: ticket.commitment,
        })
    }

    pub(crate) fn record_idle_transition(&self, _active_turn: &MutexGuard<'_, Option<ActiveTurn>>) {
        let mut state = self
            .authority
            .lock()
            .expect("voice authority mutex poisoned");
        state.idle_epoch = state.idle_epoch.next();
    }

    pub(crate) fn begin_stopping(
        &self,
        _active_turn: &MutexGuard<'_, Option<ActiveTurn>>,
        turn_id: String,
    ) -> Result<u64, BeginStoppingError> {
        let mut state = self
            .authority
            .lock()
            .expect("voice authority mutex poisoned");
        if state.stopping.is_some() {
            return Err(BeginStoppingError::AlreadyStopping);
        }
        state.stopping_generation = state.stopping_generation.saturating_add(1);
        let generation = state.stopping_generation;
        state.stopping = Some(StoppingFence {
            turn_id,
            generation,
        });
        Ok(generation)
    }

    pub(crate) fn finish_stopping(
        &self,
        active_turn: &MutexGuard<'_, Option<ActiveTurn>>,
        turn_id: &str,
        generation: u64,
    ) -> bool {
        let mut state = self
            .authority
            .lock()
            .expect("voice authority mutex poisoned");
        let matches_finalizer = active_turn.is_none()
            && state
                .stopping
                .as_ref()
                .is_some_and(|fence| fence.turn_id == turn_id && fence.generation == generation);
        if matches_finalizer {
            state.stopping = None;
            state.idle_epoch = state.idle_epoch.next();
        }
        matches_finalizer
    }
}

impl AdmissionState {
    fn authorize(&self, request: &AdmissionRequest) -> Result<AdmissionPermit, NotSubmittedReason> {
        if request.authority == SubmissionAuthority::AdministrativeRecovery {
            return Err(NotSubmittedReason::PermissionDenied);
        }
        match request.provenance.input_class() {
            InputClass::CorrelatedResponse
            | InputClass::InternalAgent
            | InputClass::SystemContinuation => Ok(AdmissionPermit::NonHumanInput),
            InputClass::ExternalHuman | InputClass::Unknown => match self.lease.state() {
                VoiceLeaseState::Free => Ok(AdmissionPermit::UpstreamBaseline),
                VoiceLeaseState::Acquiring { .. }
                | VoiceLeaseState::Active { .. }
                | VoiceLeaseState::Closing { .. }
                | VoiceLeaseState::RecoveryRequired { .. } => {
                    Err(NotSubmittedReason::PermissionDenied)
                }
            },
            InputClass::VoiceHuman => match self.lease.state() {
                VoiceLeaseState::Active { lease_id }
                    if request.authority == SubmissionAuthority::Voice
                        && request.voice_lease_id.as_ref() == Some(lease_id) =>
                {
                    Ok(AdmissionPermit::Voice {
                        lease_id: lease_id.clone(),
                    })
                }
                VoiceLeaseState::Free
                | VoiceLeaseState::Acquiring { .. }
                | VoiceLeaseState::Active { .. }
                | VoiceLeaseState::Closing { .. }
                | VoiceLeaseState::RecoveryRequired { .. } => {
                    Err(NotSubmittedReason::PermissionDenied)
                }
            },
        }
    }

    fn permit_remains_valid(&self, permit: &AdmissionPermit) -> bool {
        match (permit, self.lease.state()) {
            (AdmissionPermit::UpstreamBaseline, VoiceLeaseState::Free)
            | (AdmissionPermit::NonHumanInput, _) => true,
            (
                AdmissionPermit::Voice {
                    lease_id: permitted,
                },
                VoiceLeaseState::Active { lease_id: active },
            ) => permitted == active,
            (
                AdmissionPermit::UpstreamBaseline,
                VoiceLeaseState::Acquiring { .. }
                | VoiceLeaseState::Active { .. }
                | VoiceLeaseState::Closing { .. }
                | VoiceLeaseState::RecoveryRequired { .. },
            )
            | (
                AdmissionPermit::Voice { .. },
                VoiceLeaseState::Free
                | VoiceLeaseState::Acquiring { .. }
                | VoiceLeaseState::Closing { .. }
                | VoiceLeaseState::RecoveryRequired { .. },
            ) => false,
        }
    }
}

#[cfg(test)]
#[path = "admission_tests.rs"]
mod tests;

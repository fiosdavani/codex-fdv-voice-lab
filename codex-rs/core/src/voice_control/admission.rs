use crate::state::ActiveTurn;
use crate::voice_control::InputEffect;
use crate::voice_control::InputOrigin;
use crate::voice_control::InputProvenance;
use crate::voice_control::LeaseGeneration;
use crate::voice_control::SubmissionAuthority;
use crate::voice_control::VoiceLease;
use crate::voice_control::VoiceLeaseId;
use crate::voice_control::VoiceLeaseState;
use crate::voice_control::lease::VoiceLeaseTransitionError;
use std::sync::Mutex;
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
    NotCommitted,
    Committed,
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
}

impl AdmissionTicket {
    pub(crate) fn commitment(&self) -> CommitmentState {
        self.commitment
    }

    pub(crate) fn expire(self) -> SubmissionResolution {
        SubmissionResolution::NotSubmitted(NotSubmittedReason::TicketExpired)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum AdmissionPermit {
    UpstreamBaseline,
    Voice { lease_id: VoiceLeaseId },
    NonHumanInput,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CommittedAdmission {
    pub(crate) commitment: CommitmentState,
}

impl CommittedAdmission {
    /// Reports loss of the waiter after Core may have accepted the submission.
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
            commitment: CommitmentState::NotCommitted,
        })
    }

    pub(crate) fn commit(
        &self,
        active_turn: &MutexGuard<'_, Option<ActiveTurn>>,
        mut ticket: AdmissionTicket,
    ) -> Result<CommittedAdmission, NotSubmittedReason> {
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
        ticket.commitment = CommitmentState::Committed;
        Ok(CommittedAdmission {
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
    ) -> u64 {
        let mut state = self
            .authority
            .lock()
            .expect("voice authority mutex poisoned");
        state.stopping_generation = state.stopping_generation.saturating_add(1);
        let generation = state.stopping_generation;
        state.stopping = Some(StoppingFence {
            turn_id,
            generation,
        });
        generation
    }

    pub(crate) fn finish_stopping(
        &self,
        _active_turn: &MutexGuard<'_, Option<ActiveTurn>>,
        turn_id: &str,
        generation: u64,
    ) -> bool {
        let mut state = self
            .authority
            .lock()
            .expect("voice authority mutex poisoned");
        let matches_finalizer = state
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
        if request.authority == SubmissionAuthority::AdministrativeRecovery
            && matches!(
                request.provenance.effect,
                InputEffect::Say | InputEffect::Steer
            )
        {
            return Err(NotSubmittedReason::PermissionDenied);
        }
        if !request.provenance.is_new_human_input() {
            return Ok(AdmissionPermit::NonHumanInput);
        }
        match self.lease.state() {
            VoiceLeaseState::Free
                if request.authority != SubmissionAuthority::AdministrativeRecovery
                    && request.provenance.origin != InputOrigin::HumanVoice =>
            {
                Ok(AdmissionPermit::UpstreamBaseline)
            }
            VoiceLeaseState::Free => Err(NotSubmittedReason::PermissionDenied),
            VoiceLeaseState::Active { lease_id }
                if request.provenance.origin == InputOrigin::HumanVoice
                    && request.authority == SubmissionAuthority::Voice
                    && request.voice_lease_id.as_ref() == Some(lease_id) =>
            {
                Ok(AdmissionPermit::Voice {
                    lease_id: lease_id.clone(),
                })
            }
            VoiceLeaseState::Acquiring { .. }
            | VoiceLeaseState::Active { .. }
            | VoiceLeaseState::Closing { .. }
            | VoiceLeaseState::RecoveryRequired { .. } => Err(NotSubmittedReason::PermissionDenied),
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

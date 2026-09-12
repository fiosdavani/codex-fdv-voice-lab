use crate::state::ActiveTurn;
use crate::voice_control::InputClass;
use crate::voice_control::InputProvenance;
use crate::voice_control::LeaseGeneration;
use crate::voice_control::SubmissionAuthority;
use crate::voice_control::VoiceLease;
use crate::voice_control::VoiceLeaseId;
use crate::voice_control::VoiceLeaseState;
use crate::voice_control::lease::VoiceLeaseTransitionError;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::MutexGuard;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct IdleEpoch(u64);

impl IdleEpoch {
    fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommitmentState {
    Prepared,
    Committing,
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
    operation_id: EffectOperationId,
    lease_generation: LeaseGeneration,
}

impl EffectBoundary {
    pub(crate) fn commitment(&self) -> CommitmentState {
        CommitmentState::Committing
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct EffectOperationId(u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EffectOperationState {
    Committing,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct EffectOperation {
    lease_generation: LeaseGeneration,
    state: EffectOperationState,
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
    FencingExhausted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StoppingFence {
    turn_id: String,
    generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BeginStoppingError {
    AlreadyStopping,
    GenerationExhausted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum BeginLeaseAcquireError {
    Busy,
    Stopping,
    FencingExhausted,
    EffectInFlight,
    RecoveryRequired,
    LeaseTransition(VoiceLeaseTransitionError),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum FinishLeaseCloseError {
    ActiveTurn,
    Stopping,
    EffectInFlight,
    RecoveryRequired,
    FencingExhausted,
    LeaseTransition(VoiceLeaseTransitionError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResolveEffectError {
    UnknownOperation,
    StaleGeneration,
    AlreadyResolved,
    FencingExhausted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FencingError {
    GenerationExhausted,
}

#[derive(Debug)]
struct AdmissionState {
    lease: VoiceLease,
    idle_epoch: IdleEpoch,
    stopping: Option<StoppingFence>,
    stopping_generation: u64,
    fencing_exhausted: bool,
    next_effect_operation_id: u64,
    effect_operations: HashMap<EffectOperationId, EffectOperation>,
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
                fencing_exhausted: false,
                next_effect_operation_id: 0,
                effect_operations: HashMap::new(),
            }),
        }
    }
}

impl AdmissionController {
    pub(crate) fn begin_lease_acquire(
        &self,
        active_turn: &MutexGuard<'_, Option<ActiveTurn>>,
        lease_id: VoiceLeaseId,
    ) -> Result<LeaseGeneration, BeginLeaseAcquireError> {
        if active_turn.is_some() {
            return Err(BeginLeaseAcquireError::Busy);
        }
        let mut state = self
            .authority
            .lock()
            .expect("voice authority mutex poisoned");
        if state.stopping.is_some() {
            return Err(BeginLeaseAcquireError::Stopping);
        }
        if state.fencing_exhausted {
            return Err(BeginLeaseAcquireError::FencingExhausted);
        }
        if state.has_unknown_effect() {
            return Err(BeginLeaseAcquireError::RecoveryRequired);
        }
        if !state.effect_operations.is_empty() {
            return Err(BeginLeaseAcquireError::EffectInFlight);
        }
        state
            .lease
            .begin_acquire(lease_id)
            .map_err(BeginLeaseAcquireError::LeaseTransition)
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
        active_turn: &MutexGuard<'_, Option<ActiveTurn>>,
        lease_id: &VoiceLeaseId,
    ) -> Result<(), FinishLeaseCloseError> {
        if active_turn.is_some() {
            return Err(FinishLeaseCloseError::ActiveTurn);
        }
        let mut state = self
            .authority
            .lock()
            .expect("voice authority mutex poisoned");
        if state.stopping.is_some() {
            return Err(FinishLeaseCloseError::Stopping);
        }
        if state.fencing_exhausted {
            return Err(FinishLeaseCloseError::FencingExhausted);
        }
        if state.has_unknown_effect() {
            return Err(FinishLeaseCloseError::RecoveryRequired);
        }
        if !state.effect_operations.is_empty() {
            return Err(FinishLeaseCloseError::EffectInFlight);
        }
        state
            .lease
            .finish_close(lease_id)
            .map_err(FinishLeaseCloseError::LeaseTransition)
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
        if state.fencing_exhausted {
            return Err(NotSubmittedReason::FencingExhausted);
        }
        let permit = state.authorize(&request)?;
        let lease_generation = state
            .lease
            .generation()
            .map_err(|_| NotSubmittedReason::FencingExhausted)?;
        Ok(AdmissionTicket {
            lease_generation,
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
        let mut state = self
            .authority
            .lock()
            .expect("voice authority mutex poisoned");
        if state.stopping.is_some() {
            return Err(NotSubmittedReason::Stopping);
        }
        if state.fencing_exhausted {
            return Err(NotSubmittedReason::FencingExhausted);
        }
        let lease_generation = state
            .lease
            .generation()
            .map_err(|_| NotSubmittedReason::FencingExhausted)?;
        if ticket.lease_generation != lease_generation
            || !state.permit_remains_valid(&ticket.permit)
        {
            return Err(NotSubmittedReason::StaleGeneration);
        }
        if ticket.idle_epoch != state.idle_epoch {
            return Err(NotSubmittedReason::StaleIdleEpoch);
        }
        let Some(next_operation_id) = state.next_effect_operation_id.checked_add(1) else {
            state.fencing_exhausted = true;
            return Err(NotSubmittedReason::FencingExhausted);
        };
        state.next_effect_operation_id = next_operation_id;
        let operation_id = EffectOperationId(next_operation_id);
        state.effect_operations.insert(
            operation_id,
            EffectOperation {
                lease_generation,
                state: EffectOperationState::Committing,
            },
        );
        ticket.commitment = CommitmentState::Committing;
        Ok(EffectBoundary {
            operation_id,
            lease_generation,
        })
    }

    /// Resolves delivery at the effect boundary; it does not imply terminal completion of a turn.
    pub(crate) fn resolve_effect_known(
        &self,
        _active_turn: &MutexGuard<'_, Option<ActiveTurn>>,
        boundary: &EffectBoundary,
    ) -> Result<SubmissionResolution, ResolveEffectError> {
        let mut state = self
            .authority
            .lock()
            .expect("voice authority mutex poisoned");
        state.resolve_effect(boundary)?;
        state.effect_operations.remove(&boundary.operation_id);
        Ok(SubmissionResolution::KnownResult)
    }

    /// Retains ambiguous delivery evidence and forces the lease into recovery-required state.
    pub(crate) fn resolve_effect_unknown(
        &self,
        _active_turn: &MutexGuard<'_, Option<ActiveTurn>>,
        boundary: &EffectBoundary,
    ) -> Result<SubmissionResolution, ResolveEffectError> {
        let mut state = self
            .authority
            .lock()
            .expect("voice authority mutex poisoned");
        state.resolve_effect(boundary)?;
        let operation = state
            .effect_operations
            .get_mut(&boundary.operation_id)
            .expect("effect operation disappeared while authority was locked");
        operation.state = EffectOperationState::Unknown;
        if state.lease.mark_recovery_required().is_err() {
            state.fencing_exhausted = true;
            return Err(ResolveEffectError::FencingExhausted);
        }
        Ok(SubmissionResolution::UnknownResult)
    }

    pub(crate) fn record_idle_transition(
        &self,
        _active_turn: &MutexGuard<'_, Option<ActiveTurn>>,
    ) -> Result<(), FencingError> {
        let mut state = self
            .authority
            .lock()
            .expect("voice authority mutex poisoned");
        let Some(idle_epoch) = state.idle_epoch.next() else {
            state.fencing_exhausted = true;
            return Err(FencingError::GenerationExhausted);
        };
        state.idle_epoch = idle_epoch;
        Ok(())
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
        let Some(stopping_generation) = state.stopping_generation.checked_add(1) else {
            state.fencing_exhausted = true;
            return Err(BeginStoppingError::GenerationExhausted);
        };
        state.stopping_generation = stopping_generation;
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
            let Some(idle_epoch) = state.idle_epoch.next() else {
                state.fencing_exhausted = true;
                return false;
            };
            state.stopping = None;
            state.idle_epoch = idle_epoch;
        }
        matches_finalizer
    }
}

impl AdmissionState {
    fn has_unknown_effect(&self) -> bool {
        self.effect_operations
            .values()
            .any(|operation| operation.state == EffectOperationState::Unknown)
    }

    fn resolve_effect(&self, boundary: &EffectBoundary) -> Result<(), ResolveEffectError> {
        let Some(operation) = self.effect_operations.get(&boundary.operation_id) else {
            return Err(ResolveEffectError::UnknownOperation);
        };
        if operation.state == EffectOperationState::Unknown {
            return Err(ResolveEffectError::AlreadyResolved);
        }
        if operation.lease_generation != boundary.lease_generation {
            return Err(ResolveEffectError::StaleGeneration);
        }
        Ok(())
    }

    fn authorize(&self, request: &AdmissionRequest) -> Result<AdmissionPermit, NotSubmittedReason> {
        match (request.provenance.input_class(), request.authority) {
            (InputClass::CorrelatedResponse, SubmissionAuthority::CorrelatedResponse)
            | (InputClass::InternalAgent, SubmissionAuthority::InternalAgent)
            | (InputClass::SystemContinuation, SubmissionAuthority::System) => {
                Ok(AdmissionPermit::NonHumanInput)
            }
            (InputClass::ExternalHuman, SubmissionAuthority::ExternalClient)
            | (InputClass::Unknown, SubmissionAuthority::Unknown) => match self.lease.state() {
                VoiceLeaseState::Free => Ok(AdmissionPermit::UpstreamBaseline),
                VoiceLeaseState::Acquiring { .. }
                | VoiceLeaseState::Active { .. }
                | VoiceLeaseState::Closing { .. }
                | VoiceLeaseState::RecoveryRequired { .. } => {
                    Err(NotSubmittedReason::PermissionDenied)
                }
            },
            (InputClass::VoiceHuman, SubmissionAuthority::Voice) => match self.lease.state() {
                VoiceLeaseState::Active { lease_id }
                    if request.voice_lease_id.as_ref() == Some(lease_id) =>
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
            (
                _,
                SubmissionAuthority::ExternalClient
                | SubmissionAuthority::Voice
                | SubmissionAuthority::CorrelatedResponse
                | SubmissionAuthority::InternalAgent
                | SubmissionAuthority::System
                | SubmissionAuthority::AdministrativeRecovery
                | SubmissionAuthority::Unknown,
            ) => Err(NotSubmittedReason::PermissionDenied),
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

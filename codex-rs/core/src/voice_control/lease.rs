use thiserror::Error;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct VoiceLeaseId(String);

impl VoiceLeaseId {
    pub(crate) fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LeaseGeneration(u64);

impl LeaseGeneration {
    pub(crate) fn initial() -> Self {
        Self(0)
    }

    fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum VoiceLeaseState {
    Free,
    Acquiring { lease_id: VoiceLeaseId },
    Active { lease_id: VoiceLeaseId },
    Closing { lease_id: VoiceLeaseId },
    RecoveryRequired { lease_id: Option<VoiceLeaseId> },
}

/// In-memory authority over new human input for one Codex session.
///
/// This type does not own the runtime, rollout, subscribers, tools, approvals, or permissions. Its
/// state is intentionally conservative: ambiguous loss of ownership evidence transitions to
/// [`VoiceLeaseState::RecoveryRequired`], never to [`VoiceLeaseState::Free`]. Durable persistence
/// and recovery are deliberately deferred to a later patch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct VoiceLease {
    state: VoiceLeaseState,
    generation: LeaseGeneration,
    generation_exhausted: bool,
}

impl Default for VoiceLease {
    fn default() -> Self {
        Self {
            state: VoiceLeaseState::Free,
            generation: LeaseGeneration::initial(),
            generation_exhausted: false,
        }
    }
}

impl VoiceLease {
    pub(crate) fn state(&self) -> &VoiceLeaseState {
        &self.state
    }

    pub(crate) fn generation(&self) -> Result<LeaseGeneration, VoiceLeaseTransitionError> {
        if self.generation_exhausted {
            Err(VoiceLeaseTransitionError::GenerationExhausted)
        } else {
            Ok(self.generation)
        }
    }

    pub(crate) fn begin_acquire(
        &mut self,
        lease_id: VoiceLeaseId,
    ) -> Result<LeaseGeneration, VoiceLeaseTransitionError> {
        if self.state != VoiceLeaseState::Free {
            return Err(VoiceLeaseTransitionError::NotFree);
        }
        self.generation = self.next_generation(None)?;
        self.state = VoiceLeaseState::Acquiring { lease_id };
        Ok(self.generation)
    }

    pub(crate) fn activate(
        &mut self,
        lease_id: &VoiceLeaseId,
    ) -> Result<(), VoiceLeaseTransitionError> {
        self.require_owner(lease_id, LeasePhase::Acquiring)?;
        self.state = VoiceLeaseState::Active {
            lease_id: lease_id.clone(),
        };
        Ok(())
    }

    pub(crate) fn begin_close(
        &mut self,
        lease_id: &VoiceLeaseId,
    ) -> Result<(), VoiceLeaseTransitionError> {
        self.require_owner(lease_id, LeasePhase::Active)?;
        self.state = VoiceLeaseState::Closing {
            lease_id: lease_id.clone(),
        };
        Ok(())
    }

    pub(crate) fn finish_close(
        &mut self,
        lease_id: &VoiceLeaseId,
    ) -> Result<(), VoiceLeaseTransitionError> {
        self.require_owner(lease_id, LeasePhase::Closing)?;
        self.generation = self.next_generation(Some(lease_id.clone()))?;
        self.state = VoiceLeaseState::Free;
        Ok(())
    }

    pub(crate) fn mark_recovery_required(&mut self) -> Result<(), VoiceLeaseTransitionError> {
        let lease_id = match &self.state {
            VoiceLeaseState::Free => None,
            VoiceLeaseState::Acquiring { lease_id }
            | VoiceLeaseState::Active { lease_id }
            | VoiceLeaseState::Closing { lease_id } => Some(lease_id.clone()),
            VoiceLeaseState::RecoveryRequired { lease_id } => lease_id.clone(),
        };
        self.generation = self.next_generation(lease_id.clone())?;
        self.state = VoiceLeaseState::RecoveryRequired { lease_id };
        Ok(())
    }

    fn next_generation(
        &mut self,
        lease_id: Option<VoiceLeaseId>,
    ) -> Result<LeaseGeneration, VoiceLeaseTransitionError> {
        let Some(generation) = self.generation.next() else {
            self.generation_exhausted = true;
            self.state = VoiceLeaseState::RecoveryRequired { lease_id };
            return Err(VoiceLeaseTransitionError::GenerationExhausted);
        };
        Ok(generation)
    }

    fn require_owner(
        &self,
        lease_id: &VoiceLeaseId,
        phase: LeasePhase,
    ) -> Result<(), VoiceLeaseTransitionError> {
        let actual = match &self.state {
            VoiceLeaseState::Acquiring { lease_id } if phase == LeasePhase::Acquiring => lease_id,
            VoiceLeaseState::Active { lease_id } if phase == LeasePhase::Active => lease_id,
            VoiceLeaseState::Closing { lease_id } if phase == LeasePhase::Closing => lease_id,
            VoiceLeaseState::Free
            | VoiceLeaseState::Acquiring { .. }
            | VoiceLeaseState::Active { .. }
            | VoiceLeaseState::Closing { .. }
            | VoiceLeaseState::RecoveryRequired { .. } => {
                return Err(VoiceLeaseTransitionError::WrongState);
            }
        };
        if actual != lease_id {
            return Err(VoiceLeaseTransitionError::WrongOwner);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LeasePhase {
    Acquiring,
    Active,
    Closing,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub(crate) enum VoiceLeaseTransitionError {
    #[error("voice lease is not free")]
    NotFree,
    #[error("voice lease is in the wrong state for this transition")]
    WrongState,
    #[error("voice lease is owned by another session")]
    WrongOwner,
    #[error("voice lease generation exhausted")]
    GenerationExhausted,
}

#[cfg(test)]
#[path = "lease_tests.rs"]
mod tests;

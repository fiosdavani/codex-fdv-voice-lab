/// The trusted origin assigned to an input by Core.
///
/// Constructing this enum does not grant permission. Enforcement must separately validate a
/// [`SubmissionAuthority`] and a permit issued by the voice-control state machine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InputOrigin {
    HumanExternalClient,
    HumanVoice,
    CorrelatedResponse,
    InternalAgent,
    SystemContinuation,
    Unknown,
}

/// The effect requested by an input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InputEffect {
    StartTurn,
    Say,
    Steer,
    Continue,
}

/// Authority assigned inside Core to the code path submitting an input.
///
/// `AdministrativeRecovery` is deliberately distinct from voice and external-client authority. It
/// is reserved for narrowly scoped future recovery operations and is never eligible for `Say` or
/// `Steer`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SubmissionAuthority {
    ExternalClient,
    Voice,
    InternalAgent,
    System,
    AdministrativeRecovery,
    Unknown,
}

/// Trusted provenance attached to work before admission is evaluated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct InputProvenance {
    pub(crate) origin: InputOrigin,
    pub(crate) effect: InputEffect,
}

impl InputProvenance {
    pub(crate) fn input_class(self) -> InputClass {
        match self.origin {
            InputOrigin::HumanExternalClient => InputClass::ExternalHuman,
            InputOrigin::HumanVoice => InputClass::VoiceHuman,
            InputOrigin::CorrelatedResponse => InputClass::CorrelatedResponse,
            InputOrigin::InternalAgent => InputClass::InternalAgent,
            InputOrigin::SystemContinuation => InputClass::SystemContinuation,
            InputOrigin::Unknown => InputClass::Unknown,
        }
    }
}

/// Semantic class used by input admission independently of the requested effect.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InputClass {
    ExternalHuman,
    VoiceHuman,
    CorrelatedResponse,
    InternalAgent,
    SystemContinuation,
    Unknown,
}

#[cfg(test)]
#[path = "provenance_tests.rs"]
mod tests;

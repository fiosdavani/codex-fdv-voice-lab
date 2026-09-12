use super::*;
use pretty_assertions::assert_eq;

#[test]
fn input_class_does_not_change_with_the_requested_effect() {
    let cases = [
        (
            InputProvenance {
                origin: InputOrigin::HumanExternalClient,
                effect: InputEffect::StartTurn,
            },
            InputClass::ExternalHuman,
        ),
        (
            InputProvenance {
                origin: InputOrigin::HumanExternalClient,
                effect: InputEffect::Steer,
            },
            InputClass::ExternalHuman,
        ),
        (
            InputProvenance {
                origin: InputOrigin::HumanVoice,
                effect: InputEffect::Say,
            },
            InputClass::VoiceHuman,
        ),
        (
            InputProvenance {
                origin: InputOrigin::CorrelatedResponse,
                effect: InputEffect::Continue,
            },
            InputClass::CorrelatedResponse,
        ),
        (
            InputProvenance {
                origin: InputOrigin::InternalAgent,
                effect: InputEffect::StartTurn,
            },
            InputClass::InternalAgent,
        ),
        (
            InputProvenance {
                origin: InputOrigin::SystemContinuation,
                effect: InputEffect::Continue,
            },
            InputClass::SystemContinuation,
        ),
        (
            InputProvenance {
                origin: InputOrigin::Unknown,
                effect: InputEffect::Continue,
            },
            InputClass::Unknown,
        ),
    ];

    assert_eq!(
        cases.map(|(provenance, _)| provenance.input_class()),
        cases.map(|(_, expected)| expected)
    );
}

use super::*;
use pretty_assertions::assert_eq;

#[test]
fn only_new_human_turns_are_subject_to_voice_lease_admission() {
    let cases = [
        (
            InputProvenance {
                origin: InputOrigin::HumanExternalClient,
                effect: InputEffect::StartTurn,
            },
            true,
        ),
        (
            InputProvenance {
                origin: InputOrigin::Unknown,
                effect: InputEffect::StartTurn,
            },
            true,
        ),
        (
            InputProvenance {
                origin: InputOrigin::CorrelatedResponse,
                effect: InputEffect::Continue,
            },
            false,
        ),
        (
            InputProvenance {
                origin: InputOrigin::InternalAgent,
                effect: InputEffect::StartTurn,
            },
            false,
        ),
        (
            InputProvenance {
                origin: InputOrigin::SystemContinuation,
                effect: InputEffect::Continue,
            },
            false,
        ),
        (
            InputProvenance {
                origin: InputOrigin::HumanVoice,
                effect: InputEffect::Steer,
            },
            false,
        ),
    ];

    assert_eq!(
        cases.map(|(provenance, _)| provenance.is_new_human_input()),
        cases.map(|(_, expected)| expected)
    );
}

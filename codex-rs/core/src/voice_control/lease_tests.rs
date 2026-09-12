use super::*;
use pretty_assertions::assert_eq;

#[test]
fn lease_follows_the_explicit_lifecycle() {
    let lease_id = VoiceLeaseId::new("voice-session");
    let mut lease = VoiceLease::default();

    let acquired_generation = lease.begin_acquire(lease_id.clone()).expect("acquire");
    assert_eq!(
        lease.state(),
        &VoiceLeaseState::Acquiring {
            lease_id: lease_id.clone()
        }
    );
    lease.activate(&lease_id).expect("activate");
    assert_eq!(
        lease.state(),
        &VoiceLeaseState::Active {
            lease_id: lease_id.clone()
        }
    );
    lease.begin_close(&lease_id).expect("begin close");
    assert_eq!(
        lease.state(),
        &VoiceLeaseState::Closing {
            lease_id: lease_id.clone()
        }
    );
    lease.finish_close(&lease_id).expect("finish close");

    assert_eq!(lease.state(), &VoiceLeaseState::Free);
    assert_ne!(lease.generation(), Ok(acquired_generation));
}

#[test]
fn uncertainty_fails_closed_instead_of_releasing_the_lease() {
    let lease_id = VoiceLeaseId::new("voice-session");
    let mut lease = VoiceLease::default();
    lease.begin_acquire(lease_id.clone()).expect("acquire");
    lease.activate(&lease_id).expect("activate");

    lease.mark_recovery_required().expect("mark recovery");

    assert_eq!(
        lease.state(),
        &VoiceLeaseState::RecoveryRequired {
            lease_id: Some(lease_id)
        }
    );
}

#[test]
fn generation_overflow_enters_recovery_and_fails_closed() {
    let mut lease = VoiceLease {
        state: VoiceLeaseState::Free,
        generation: LeaseGeneration(u64::MAX),
        generation_exhausted: false,
    };

    assert_eq!(
        lease.begin_acquire(VoiceLeaseId::new("voice-session")),
        Err(VoiceLeaseTransitionError::GenerationExhausted)
    );
    assert_eq!(
        lease.state(),
        &VoiceLeaseState::RecoveryRequired { lease_id: None }
    );
    assert_eq!(
        lease.generation(),
        Err(VoiceLeaseTransitionError::GenerationExhausted)
    );
}

#[test]
fn another_owner_cannot_advance_or_release_a_lease() {
    let owner = VoiceLeaseId::new("owner");
    let other = VoiceLeaseId::new("other");
    let mut lease = VoiceLease::default();
    lease.begin_acquire(owner.clone()).expect("acquire");

    assert_eq!(
        lease.activate(&other),
        Err(VoiceLeaseTransitionError::WrongOwner)
    );
    assert_eq!(
        lease.state(),
        &VoiceLeaseState::Acquiring { lease_id: owner }
    );
}

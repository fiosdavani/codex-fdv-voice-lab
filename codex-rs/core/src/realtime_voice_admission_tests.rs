use super::*;
use pretty_assertions::assert_eq;

#[test]
fn identity_depends_on_origin_and_generation_not_text() {
    let thread = ThreadId::new();
    let scope = VoiceAdmissionScope {
        voice_session_generation: 1,
        native_session_id: "FAKE_NATIVE_SESSION".to_string(),
    };
    let mut handoff = RealtimeHandoffRequested {
        handoff_id: "FAKE_ORIGIN_A".to_string(),
        item_id: "FAKE_ITEM".to_string(),
        input_transcript: "same words".to_string(),
        active_transcript: Vec::new(),
    };
    let first = voice_admission_input(thread, &scope, &handoff, "same words".to_string()).unwrap();
    let duplicate =
        voice_admission_input(thread, &scope, &handoff, "same words".to_string()).unwrap();
    assert_eq!(first, duplicate);
    let changed =
        voice_admission_input(thread, &scope, &handoff, "other words".to_string()).unwrap();
    assert_eq!(first.origin_id, changed.origin_id);
    handoff.handoff_id = "FAKE_ORIGIN_B".to_string();
    let second = voice_admission_input(thread, &scope, &handoff, "same words".to_string()).unwrap();
    assert_ne!(first.origin_id, second.origin_id);
    let successor = VoiceAdmissionScope {
        voice_session_generation: 2,
        ..scope.clone()
    };
    let newer =
        voice_admission_input(thread, &successor, &handoff, "same words".to_string()).unwrap();
    assert_ne!(second.origin_id, newer.origin_id);
    handoff.handoff_id.clear();
    assert!(voice_admission_input(thread, &scope, &handoff, "same words".to_string()).is_err());
}

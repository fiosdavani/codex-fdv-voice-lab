use super::*;
use codex_protocol::ThreadId;
use pretty_assertions::assert_eq;

fn origin(scope: &VoiceAdmissionScope, id: &str) -> VoiceAdmissionInput {
    VoiceAdmissionInput {
        thread_id: ThreadId::new(), scope: scope.clone(), origin_id: id.to_string(),
        handoff_id: Some(format!("handoff-{id}")), item_id: Some(format!("item-{id}")),
        text: "same spoken words".to_string(),
    }
}

#[test]
fn received_is_not_started_and_completion_does_not_consume_pending_origin() {
    let scope = VoiceAdmissionScope { native_session_id: "native-A".into(), voice_session_generation: 1 };
    let mut routes = VoiceTurnRoutes::new(scope.clone());
    let a = origin(&scope, "origin-A");
    let b = origin(&scope, "origin-B");
    routes.received(&a).unwrap();
    routes.started("turn-A", Some("origin-A")).unwrap();
    let binding_a = routes.binding("turn-A").unwrap().clone();
    routes.received(&b).unwrap();
    routes.received(&b).unwrap();
    assert_eq!(routes.binding("turn-A"), Some(&binding_a));
    assert_eq!(routes.binding("turn-B"), None);
    routes.complete("turn-A");
    assert_eq!(routes.binding("turn-A"), None);
    routes.started("turn-B", Some("origin-B")).unwrap();
    assert_eq!(routes.binding("turn-B"), Some(&VoiceTurnBinding {
        native_session_id: "native-A".into(), voice_session_generation: 1,
        turn_id: "turn-B".into(), origin_id: "origin-B".into(), client_id: "origin-B".into(),
        handoff_id: "handoff-origin-B".into(),
    }));
    routes.complete("turn-A");
    assert!(routes.binding("turn-B").is_some());
    assert!(routes.started("another-turn", Some("origin-A")).is_err());
    assert!(routes.started("another-turn", Some("origin-B")).is_err());
}

#[test]
fn mismatched_session_and_unknown_client_id_never_gain_routes() {
    let scope = VoiceAdmissionScope { native_session_id: "native-A".into(), voice_session_generation: 7 };
    let mut routes = VoiceTurnRoutes::new(scope.clone());
    let mut wrong_session = origin(&scope, "origin-A");
    wrong_session.scope.native_session_id = "native-B".into();
    assert!(routes.received(&wrong_session).is_err());
    routes.started("turn-A", Some("origin-A")).unwrap();
    assert_eq!(routes.binding("turn-A"), None);
    routes.received(&origin(&scope, "origin-A")).unwrap();
    routes.started("turn-B", Some("not-the-origin")).unwrap();
    assert_eq!(routes.binding("turn-B"), None);
}

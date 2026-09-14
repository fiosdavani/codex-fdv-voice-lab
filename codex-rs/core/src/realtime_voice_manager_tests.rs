//! The actual manager/output channel is exercised here. No realtime transport,
//! model, queue double, Windows process or native audio is started by these tests.

use super::*;
use codex_protocol::ThreadId;
use pretty_assertions::assert_eq;

fn handoff(id: &str) -> RealtimeHandoffRequested {
    RealtimeHandoffRequested {
        handoff_id: id.to_string(), item_id: format!("item-{id}"),
        input_transcript: "same words".into(), active_transcript: Vec::new(),
    }
}

async fn manager(
    parser: RealtimeEventParser,
) -> (RealtimeConversationManager, RealtimeHandoffState, Receiver<RealtimeOutbound>, VoiceAdmissionScope) {
    let scope = VoiceAdmissionScope { native_session_id: "native-A".into(), voice_session_generation: 1 };
    let session_kind = match parser {
        RealtimeEventParser::V1 | RealtimeEventParser::FramelessBidi => RealtimeSessionKind::V1,
        RealtimeEventParser::RealtimeV2 => RealtimeSessionKind::V2,
    };
    let (output_tx, output_rx) = async_channel::bounded(16);
    let handoff = RealtimeHandoffState {
        output_tx, last_output: Arc::new(Mutex::new(HashMap::new())),
        voice_routes: Some(Arc::new(Mutex::new(VoiceTurnRoutes::new(scope.clone())))),
        stream: Arc::new(Mutex::new(RealtimeHandoffStreamState::default())),
        client_managed_handoffs: false, codex_responses_as_items: false,
        codex_response_item_prefix: None,
        codex_response_handoff_mode: CodexResponseHandoffMode::Thinking,
        codex_response_handoff_channel_prefixes: Arc::new(BTreeMap::new()),
        session_kind, event_parser: parser,
    };
    let (audio_tx, _) = async_channel::bounded(1);
    let (text_tx, _) = async_channel::bounded(1);
    let manager = RealtimeConversationManager {
        state: Mutex::new(Some(ConversationState {
            audio_tx, text_tx, session_kind, handoff: handoff.clone(),
            input_task: tokio::spawn(async {}), fanout_task: None,
            realtime_active: Arc::new(AtomicBool::new(true)),
            route_handoffs: Arc::new(RealtimeHandoffAdmission::new()),
            stop_token: CancellationToken::new(),
        })),
        mode_instructions: Mutex::new(None),
    };
    (manager, handoff, output_rx, scope)
}

#[tokio::test]
async fn strict_v1_v2_frameless_arrival_never_rebinds_or_acknowledges_steer() {
    for parser in [RealtimeEventParser::V1, RealtimeEventParser::RealtimeV2, RealtimeEventParser::FramelessBidi] {
        let (manager, state, output, scope) = manager(parser).await;
        let a = voice_admission_input(ThreadId::new(), &scope, &handoff("A"), "same words".into()).unwrap();
        let b = voice_admission_input(a.thread_id, &scope, &handoff("B"), "same words".into()).unwrap();
        manager.remember_voice_origin(&a).await.unwrap();
        manager.activate_voice_turn("turn-A", Some(&a.origin_id)).await.unwrap();
        // This is the actual arrival decision consumed by handle_realtime_server_event.
        // Its SteerAcknowledgement effect is the only branch that writes a steer ACK.
        assert_eq!(state.receive_handoff(&handoff("B")).await, HandoffArrivalEffect::Queued);
        manager.remember_voice_origin(&b).await.unwrap();
        manager.remember_voice_origin(&b).await.unwrap();
        assert!(!state.voice_routes.as_ref().unwrap().lock().await.was_started("B"));
        if parser == RealtimeEventParser::RealtimeV2 {
            assert_eq!(state.v2_handoff_payload("A", V2HandoffStage::Progress, "A progress".into()).await, None);
            assert_eq!(state.v2_handoff_payload("B", V2HandoffStage::Final, "not started B".into()).await, None);
            assert_eq!(state.v2_handoff_payload("A", V2HandoffStage::Final, "A final".into()).await, Some("A final".into()));
        }
        assert_eq!(state.stream.lock().await.active_handoff, None);
        assert!(output.is_empty());
        manager.handoff_out("turn-A", "output A".into(), None).await.unwrap();
        assert_eq!(output.recv().await.unwrap(), RealtimeOutbound::HandoffUpdate {
            handoff_id: "A".into(), text: realtime_backend_output("output A".into(), state.session_kind), phase: None,
        });
        manager.handoff_out("turn-B", "must remain held".into(), None).await.unwrap();
        assert!(output.is_empty());
        manager.handoff_complete("turn-A").await.unwrap();
        if parser == RealtimeEventParser::RealtimeV2 {
            assert_eq!(output.recv().await.unwrap(), RealtimeOutbound::CompletedHandoff {
                handoff_id: "A".into(), text: realtime_backend_output("output A".into(), state.session_kind), phase: None,
            });
        }
        manager.clear_active_handoff("turn-A").await;
        manager.activate_voice_turn("turn-B", Some(&b.origin_id)).await.unwrap();
        assert!(state.voice_routes.as_ref().unwrap().lock().await.was_started("B"));
        // Late completion A cannot clear B, its first item, or B's last output.
        manager.handoff_out("turn-B", "output B".into(), None).await.unwrap();
        manager.clear_active_handoff("turn-A").await;
        assert_eq!(output.recv().await.unwrap(), RealtimeOutbound::HandoffUpdate {
            handoff_id: "B".into(), text: realtime_backend_output("output B".into(), state.session_kind), phase: None,
        });
        manager.handoff_out("turn-B", "B still alive".into(), None).await.unwrap();
        assert!(matches!(output.recv().await.unwrap(), RealtimeOutbound::HandoffUpdate { handoff_id, .. } if handoff_id == "B"));
        manager.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn v2_terminal_event_arrives_after_successor_start_and_keeps_each_last_output() {
    let (manager, state, output, scope) = manager(RealtimeEventParser::RealtimeV2).await;
    let a = voice_admission_input(ThreadId::new(), &scope, &handoff("A"), "A".into()).unwrap();
    let b = voice_admission_input(a.thread_id, &scope, &handoff("B"), "B".into()).unwrap();
    manager.remember_voice_origin(&a).await.unwrap();
    manager.activate_voice_turn("turn-A", Some(&a.origin_id)).await.unwrap();
    manager.handoff_out("turn-A", "final A".into(), None).await.unwrap();
    manager.remember_voice_origin(&b).await.unwrap();
    manager.activate_voice_turn("turn-B", Some(&b.origin_id)).await.unwrap();
    manager.handoff_out("turn-B", "final B".into(), None).await.unwrap();
    for (handoff_id, text) in [("A", "final A"), ("B", "final B")] {
        assert_eq!(output.recv().await.unwrap(), RealtimeOutbound::HandoffUpdate {
            handoff_id: handoff_id.into(), text: realtime_backend_output(text.into(), state.session_kind), phase: None,
        });
    }
    manager.handoff_complete("turn-A").await.unwrap();
    manager.clear_active_handoff("turn-A").await;
    manager.handoff_complete("turn-B").await.unwrap();
    for (handoff_id, text) in [("A", "final A"), ("B", "final B")] {
        assert_eq!(output.recv().await.unwrap(), RealtimeOutbound::CompletedHandoff {
            handoff_id: handoff_id.into(), text: realtime_backend_output(text.into(), state.session_kind), phase: None,
        });
    }
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn frameless_stream_items_and_delayed_completion_are_turn_bound() {
    let (manager, state, output, scope) = manager(RealtimeEventParser::FramelessBidi).await;
    let a = voice_admission_input(ThreadId::new(), &scope, &handoff("A"), "words A".into()).unwrap();
    let b = voice_admission_input(a.thread_id, &scope, &handoff("B"), "words B".into()).unwrap();
    manager.remember_voice_origin(&a).await.unwrap();
    manager.activate_voice_turn("turn-A", Some(&a.origin_id)).await.unwrap();
    manager.register_handoff_stream_item("turn-A", "item-A".into(), None, String::new()).await;
    manager.remember_voice_origin(&b).await.unwrap();
    assert_eq!(state.receive_handoff(&handoff("B")).await, HandoffArrivalEffect::Queued);
    manager.stream_handoff_delta("turn-A", "item-A", "output A".into()).await.unwrap();
    manager.stream_handoff_delta("turn-B", "item-A", "must not leak".into()).await.unwrap();
    assert!(manager.finish_handoff_stream_item("turn-A", "item-A").await);
    assert_eq!(output.recv().await.unwrap(), RealtimeOutbound::HandoffAppend {
        handoff_id: "A".into(), text: "output A".into(), phase: None,
    });
    manager.activate_voice_turn("turn-B", Some(&b.origin_id)).await.unwrap();
    manager.register_handoff_stream_item("turn-B", "item-B".into(), None, String::new()).await;
    manager.clear_active_handoff("turn-A").await;
    manager.stream_handoff_delta("turn-B", "item-B", "output B".into()).await.unwrap();
    flush_streamed_handoff_item(&state, "turn-A", "item-B").await;
    assert!(output.is_empty());
    assert!(manager.finish_handoff_stream_item("turn-B", "item-B").await);
    assert_eq!(output.recv().await.unwrap(), RealtimeOutbound::HandoffAppend {
        handoff_id: "B".into(), text: "output B".into(), phase: None,
    });
    manager.shutdown().await.unwrap();
}

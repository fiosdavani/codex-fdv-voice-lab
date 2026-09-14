//! The actual manager/output channel is exercised here. No realtime transport,
//! model, queue double, Windows process or native audio is started by these tests.

use super::*;
use codex_protocol::ThreadId;
use pretty_assertions::assert_eq;

#[derive(Default)]
struct NativeObserver {
    signals: std::sync::Mutex<Vec<codex_extension_api::VoiceNativeSessionSignal>>,
    input_stopped: Option<Arc<AtomicBool>>,
    reject_closed: bool,
}

impl codex_extension_api::VoiceNativeSessionObserver for NativeObserver {
    fn emit(&self, signal: codex_extension_api::VoiceNativeSessionSignal) -> codex_extension_api::VoiceNativeSessionFuture<'_> {
        Box::pin(async move {
            let closing = matches!(&signal.event, codex_extension_api::VoiceNativeSessionEvent::Closed { .. });
            if closing && let Some(stopped) = &self.input_stopped {
                assert!(stopped.load(Ordering::Acquire), "Closed must follow input task termination");
            }
            self.signals.lock().unwrap().push(signal);
            if closing && self.reject_closed {
                return Err("close delivery failed".into());
            }
            Ok(())
        })
    }
}

#[tokio::test]
async fn scoped_close_rejects_predecessor_after_successor_install() {
    let observer = Arc::new(NativeObserver::default());
    let thread_id = ThreadId::new();
    let old = NativeVoiceSession::begin(NativeVoiceSessionStart {
        thread_id, start_id: "same-start-id".into(), hooks: VoiceNativeSessionHooks(observer.clone()),
    }).await.unwrap();
    let provider = |id: &str| RealtimeEvent::SessionUpdated {
        realtime_session_id: id.into(), instructions: None,
    };
    old.observe(&provider("provider-A")).await.unwrap();
    let old_scope = old.scope().await.unwrap();
    old.close().await.unwrap();
    let successor = NativeVoiceSession::begin(NativeVoiceSessionStart {
        thread_id, start_id: "same-start-id".into(), hooks: VoiceNativeSessionHooks(observer.clone()),
    }).await.unwrap();
    successor.observe(&provider("provider-B")).await.unwrap();
    let scope = successor.scope().await.unwrap();
    let (manager, _, _, _) = manager(RealtimeEventParser::RealtimeV2).await;
    {
        let mut state = manager.state.lock().await;
        let state = state.as_mut().unwrap();
        state.native_voice = Some(successor.clone());
        state.handoff.voice_routes = Some(successor.routes.clone());
    }
    assert_eq!(manager.shutdown_native("same-start-id", old_scope.voice_session_generation).await, Ok(false));
    assert_eq!(successor.scope().await.unwrap(), scope);
    let input = voice_admission_input(thread_id, &scope, &handoff("B"), "words B".into()).unwrap();
    manager.remember_voice_origin(&input).await.unwrap();
    manager.activate_voice_turn("turn-B", Some(&input.origin_id)).await.unwrap();
    let cancellation = successor.routes.lock().await.output_route("turn-B").unwrap().1;
    assert_eq!(manager.shutdown_native("same-start-id", scope.voice_session_generation).await, Ok(true));
    assert!(cancellation.is_cancelled());
    assert!(manager.running_state().await.is_none());
    assert_eq!(manager.shutdown_native("same-start-id", scope.voice_session_generation).await, Ok(false));
    assert_eq!(observer.signals.lock().unwrap().iter().filter(|signal|
        matches!(&signal.event, codex_extension_api::VoiceNativeSessionEvent::Closed { .. })).count(), 2);
}

#[tokio::test]
async fn native_close_waits_for_input_task_and_preserves_delivery_failure() {
    let stopped = Arc::new(AtomicBool::new(false));
    let observer = Arc::new(NativeObserver {
        input_stopped: Some(stopped.clone()), reject_closed: true, ..Default::default()
    });
    let native = NativeVoiceSession::begin(NativeVoiceSessionStart {
        thread_id: ThreadId::new(), start_id: "start".into(), hooks: VoiceNativeSessionHooks(observer.clone()),
    }).await.unwrap();
    native.observe(&RealtimeEvent::SessionUpdated {
        realtime_session_id: "provider-A".into(), instructions: None,
    }).await.unwrap();
    let scope = native.scope().await.unwrap();
    let (manager, _, _, _) = manager(RealtimeEventParser::RealtimeV2).await;
    {
        let mut state = manager.state.lock().await;
        let state = state.as_mut().unwrap();
        state.native_voice = Some(native);
        let stop_token = state.stop_token.clone();
        state.input_task = tokio::spawn(async move {
            stop_token.cancelled().await;
            stopped.store(true, Ordering::Release);
        });
    }
    assert_eq!(manager.shutdown_native("start", scope.voice_session_generation).await,
        Err("close delivery failed".into()));
    assert_eq!(manager.shutdown_native("start", scope.voice_session_generation).await,
        Err("close delivery failed".into()));
    assert_eq!(*manager.native_close_error.lock().await, Some("close delivery failed".into()));
    assert_eq!(observer.signals.lock().unwrap().iter().filter(|signal|
        matches!(&signal.event, codex_extension_api::VoiceNativeSessionEvent::Closed { .. })).count(), 1);
}

fn aborted_event() -> EventMsg {
    EventMsg::TurnAborted(codex_protocol::protocol::TurnAbortedEvent {
        // The dispatcher must use the TurnContext argument, not this optional ID.
        turn_id: Some("not-the-turn-context".into()),
        reason: codex_protocol::protocol::TurnAbortReason::Interrupted,
        started_at: None,
        completed_at: None,
        duration_ms: None,
    })
}

fn completed_event(turn_id: &str) -> EventMsg {
    EventMsg::TurnComplete(codex_protocol::protocol::TurnCompleteEvent {
        turn_id: turn_id.into(),
        last_agent_message: None,
        error: None,
        started_at: None,
        completed_at: None,
        duration_ms: None,
        time_to_first_token_ms: None,
    })
}

async fn buffer_without_timer(
    manager: &RealtimeConversationManager,
    state: &RealtimeHandoffState,
    turn_id: &str,
    item_id: &str,
) {
    manager
        .register_handoff_stream_item(turn_id, item_id.into(), None, String::new())
        .await;
    // Deterministic fixture: exercise the real flush function without wall-clock sleeps.
    let mut stream = state.stream.lock().await;
    let item = stream.items.get_mut(item_id).unwrap();
    item.push_text("buffered output");
    item.flush_scheduled = true;
}

fn handoff(id: &str) -> RealtimeHandoffRequested {
    RealtimeHandoffRequested {
        handoff_id: id.to_string(),
        item_id: format!("item-{id}"),
        input_transcript: "same words".into(),
        active_transcript: Vec::new(),
    }
}

async fn manager(
    parser: RealtimeEventParser,
) -> (
    RealtimeConversationManager,
    RealtimeHandoffState,
    Receiver<RealtimeOutbound>,
    VoiceAdmissionScope,
) {
    let scope = VoiceAdmissionScope {
        native_session_id: "native-A".into(),
        voice_session_generation: 1,
    };
    let session_kind = match parser {
        RealtimeEventParser::V1 | RealtimeEventParser::FramelessBidi => RealtimeSessionKind::V1,
        RealtimeEventParser::RealtimeV2 => RealtimeSessionKind::V2,
    };
    let (output_tx, output_rx) = async_channel::bounded(16);
    let handoff = RealtimeHandoffState {
        output_tx,
        last_output: Arc::new(Mutex::new(HashMap::new())),
        voice_routes: Some(Arc::new(Mutex::new(VoiceTurnRoutes::new(scope.clone())))),
        stream: Arc::new(Mutex::new(RealtimeHandoffStreamState::default())),
        client_managed_handoffs: false,
        codex_responses_as_items: false,
        codex_response_item_prefix: None,
        codex_response_handoff_mode: CodexResponseHandoffMode::Thinking,
        codex_response_handoff_channel_prefixes: Arc::new(BTreeMap::new()),
        session_kind,
        event_parser: parser,
    };
    let (audio_tx, _) = async_channel::bounded(1);
    let (text_tx, _) = async_channel::bounded(1);
    let manager = RealtimeConversationManager {
        lifecycle_gate: Semaphore::new(1),
        native_close_error: Mutex::new(None),
        state: Mutex::new(Some(ConversationState {
            native_voice: None,
            audio_tx,
            text_tx,
            session_kind,
            handoff: handoff.clone(),
            input_task: tokio::spawn(async {}),
            fanout_task: None,
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
    for parser in [
        RealtimeEventParser::V1,
        RealtimeEventParser::RealtimeV2,
        RealtimeEventParser::FramelessBidi,
    ] {
        let (manager, state, output, scope) = manager(parser).await;
        let a = voice_admission_input(ThreadId::new(), &scope, &handoff("A"), "same words".into())
            .unwrap();
        let b =
            voice_admission_input(a.thread_id, &scope, &handoff("B"), "same words".into()).unwrap();
        manager.remember_voice_origin(&a).await.unwrap();
        manager
            .activate_voice_turn("turn-A", Some(&a.origin_id))
            .await
            .unwrap();
        // This is the actual arrival decision consumed by handle_realtime_server_event.
        // Its SteerAcknowledgement effect is the only branch that writes a steer ACK.
        assert_eq!(
            state.receive_handoff(&handoff("B")).await,
            HandoffArrivalEffect::Queued
        );
        manager.remember_voice_origin(&b).await.unwrap();
        manager.remember_voice_origin(&b).await.unwrap();
        assert!(
            !state
                .voice_routes
                .as_ref()
                .unwrap()
                .lock()
                .await
                .was_started("B")
        );
        if parser == RealtimeEventParser::RealtimeV2 {
            assert_eq!(
                state
                    .v2_handoff_payload("A", V2HandoffStage::Progress, "A progress".into())
                    .await,
                None
            );
            assert_eq!(
                state
                    .v2_handoff_payload("B", V2HandoffStage::Final, "not started B".into())
                    .await,
                None
            );
            assert_eq!(
                state
                    .v2_handoff_payload("A", V2HandoffStage::Final, "A final".into())
                    .await,
                Some("A final".into())
            );
        }
        assert_eq!(state.stream.lock().await.active_handoff, None);
        assert!(output.is_empty());
        manager
            .handoff_out("turn-A", "output A".into(), None)
            .await
            .unwrap();
        assert_eq!(
            output.recv().await.unwrap(),
            RealtimeOutbound::HandoffUpdate {
                handoff_id: "A".into(),
                text: realtime_backend_output("output A".into(), state.session_kind),
                phase: None,
            }
        );
        manager
            .handoff_out("turn-B", "must remain held".into(), None)
            .await
            .unwrap();
        assert!(output.is_empty());
        manager.handoff_complete("turn-A").await.unwrap();
        if parser == RealtimeEventParser::RealtimeV2 {
            assert_eq!(
                output.recv().await.unwrap(),
                RealtimeOutbound::CompletedHandoff {
                    handoff_id: "A".into(),
                    text: realtime_backend_output("output A".into(), state.session_kind),
                    phase: None,
                }
            );
        }
        manager.clear_active_handoff("turn-A").await;
        manager
            .activate_voice_turn("turn-B", Some(&b.origin_id))
            .await
            .unwrap();
        assert!(
            state
                .voice_routes
                .as_ref()
                .unwrap()
                .lock()
                .await
                .was_started("B")
        );
        // Late completion A cannot clear B, its first item, or B's last output.
        manager
            .handoff_out("turn-B", "output B".into(), None)
            .await
            .unwrap();
        manager.clear_active_handoff("turn-A").await;
        assert_eq!(
            output.recv().await.unwrap(),
            RealtimeOutbound::HandoffUpdate {
                handoff_id: "B".into(),
                text: realtime_backend_output("output B".into(), state.session_kind),
                phase: None,
            }
        );
        manager
            .handoff_out("turn-B", "B still alive".into(), None)
            .await
            .unwrap();
        assert!(
            matches!(output.recv().await.unwrap(), RealtimeOutbound::HandoffUpdate { handoff_id, .. } if handoff_id == "B")
        );
        manager.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn v2_terminal_event_arrives_after_successor_start_and_keeps_each_last_output() {
    let (manager, state, output, scope) = manager(RealtimeEventParser::RealtimeV2).await;
    let a = voice_admission_input(ThreadId::new(), &scope, &handoff("A"), "A".into()).unwrap();
    let b = voice_admission_input(a.thread_id, &scope, &handoff("B"), "B".into()).unwrap();
    manager.remember_voice_origin(&a).await.unwrap();
    manager
        .activate_voice_turn("turn-A", Some(&a.origin_id))
        .await
        .unwrap();
    manager
        .handoff_out("turn-A", "final A".into(), None)
        .await
        .unwrap();
    manager.remember_voice_origin(&b).await.unwrap();
    manager
        .activate_voice_turn("turn-B", Some(&b.origin_id))
        .await
        .unwrap();
    manager
        .handoff_out("turn-B", "final B".into(), None)
        .await
        .unwrap();
    for (handoff_id, text) in [("A", "final A"), ("B", "final B")] {
        assert_eq!(
            output.recv().await.unwrap(),
            RealtimeOutbound::HandoffUpdate {
                handoff_id: handoff_id.into(),
                text: realtime_backend_output(text.into(), state.session_kind),
                phase: None,
            }
        );
    }
    manager.handoff_complete("turn-A").await.unwrap();
    manager.clear_active_handoff("turn-A").await;
    manager.handoff_complete("turn-B").await.unwrap();
    for (handoff_id, text) in [("A", "final A"), ("B", "final B")] {
        assert_eq!(
            output.recv().await.unwrap(),
            RealtimeOutbound::CompletedHandoff {
                handoff_id: handoff_id.into(),
                text: realtime_backend_output(text.into(), state.session_kind),
                phase: None,
            }
        );
    }
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn frameless_stream_items_and_delayed_completion_are_turn_bound() {
    let (manager, state, output, scope) = manager(RealtimeEventParser::FramelessBidi).await;
    let a =
        voice_admission_input(ThreadId::new(), &scope, &handoff("A"), "words A".into()).unwrap();
    let b = voice_admission_input(a.thread_id, &scope, &handoff("B"), "words B".into()).unwrap();
    manager.remember_voice_origin(&a).await.unwrap();
    manager
        .activate_voice_turn("turn-A", Some(&a.origin_id))
        .await
        .unwrap();
    manager
        .register_handoff_stream_item("turn-A", "item-A".into(), None, String::new())
        .await;
    manager.remember_voice_origin(&b).await.unwrap();
    assert_eq!(
        state.receive_handoff(&handoff("B")).await,
        HandoffArrivalEffect::Queued
    );
    manager
        .stream_handoff_delta("turn-A", "item-A", "output A".into())
        .await
        .unwrap();
    manager
        .stream_handoff_delta("turn-B", "item-A", "must not leak".into())
        .await
        .unwrap();
    assert!(manager.finish_handoff_stream_item("turn-A", "item-A").await);
    assert_eq!(
        output.recv().await.unwrap(),
        RealtimeOutbound::HandoffAppend {
            handoff_id: "A".into(),
            text: "output A".into(),
            phase: None,
        }
    );
    manager
        .activate_voice_turn("turn-B", Some(&b.origin_id))
        .await
        .unwrap();
    manager
        .register_handoff_stream_item("turn-B", "item-B".into(), None, String::new())
        .await;
    manager.clear_active_handoff("turn-A").await;
    manager
        .stream_handoff_delta("turn-B", "item-B", "output B".into())
        .await
        .unwrap();
    flush_streamed_handoff_item(&state, "turn-A", "item-B").await;
    assert!(output.is_empty());
    assert!(manager.finish_handoff_stream_item("turn-B", "item-B").await);
    assert_eq!(
        output.recv().await.unwrap(),
        RealtimeOutbound::HandoffAppend {
            handoff_id: "B".into(),
            text: "output B".into(),
            phase: None,
        }
    );
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn aborted_event_retires_a_and_preserves_pending_and_started_b() {
    let (manager, state, output, scope) = manager(RealtimeEventParser::FramelessBidi).await;
    let a = voice_admission_input(ThreadId::new(), &scope, &handoff("A"), "A".into()).unwrap();
    let b = voice_admission_input(a.thread_id, &scope, &handoff("B"), "B".into()).unwrap();
    manager.remember_voice_origin(&a).await.unwrap();
    manager
        .activate_voice_turn("turn-A", Some(&a.origin_id))
        .await
        .unwrap();
    let cancellation_a = state
        .voice_routes
        .as_ref()
        .unwrap()
        .lock()
        .await
        .output_route("turn-A")
        .unwrap()
        .1;
    buffer_without_timer(&manager, &state, "turn-A", "item-A").await;
    manager
        .handoff_out("turn-A", "last A".into(), None)
        .await
        .unwrap();
    assert!(
        matches!(output.recv().await.unwrap(), RealtimeOutbound::HandoffUpdate { handoff_id, .. } if handoff_id == "A")
    );
    manager.remember_voice_origin(&b).await.unwrap();
    assert_eq!(
        state.receive_handoff(&handoff("B")).await,
        HandoffArrivalEffect::Queued
    );

    // Same dispatcher called by Session::maybe_clear_realtime_handoff_for_event.
    manager
        .handle_terminal_handoff_event("turn-A", &aborted_event())
        .await
        .unwrap();
    assert!(output.is_empty()); // Abort must never emit CompletedHandoff.
    assert!(cancellation_a.is_cancelled());
    assert!(
        state
            .voice_routes
            .as_ref()
            .unwrap()
            .lock()
            .await
            .binding("turn-A")
            .is_none()
    );
    assert!(
        !state
            .voice_routes
            .as_ref()
            .unwrap()
            .lock()
            .await
            .was_started("A")
    );
    assert!(
        state
            .voice_routes
            .as_ref()
            .unwrap()
            .lock()
            .await
            .binding("turn-B")
            .is_none()
    );
    assert!(
        !state
            .voice_routes
            .as_ref()
            .unwrap()
            .lock()
            .await
            .was_started("B")
    );
    assert!(!state.stream.lock().await.items.contains_key("item-A"));
    assert!(!state.last_output.lock().await.contains_key("turn-A"));
    manager.remember_voice_origin(&a).await.unwrap();
    assert!(
        manager
            .activate_voice_turn("turn-A", Some(&a.origin_id))
            .await
            .is_err()
    );
    assert!(
        manager
            .activate_voice_turn("retry-A", Some(&a.origin_id))
            .await
            .is_err()
    );
    flush_streamed_handoff_item(&state, "turn-A", "item-A").await;
    manager
        .register_handoff_stream_item("turn-A", "late-A".into(), None, "late".into())
        .await;
    manager
        .handoff_out("turn-A", "late output".into(), None)
        .await
        .unwrap();
    manager
        .handle_terminal_handoff_event("turn-A", &completed_event("turn-A"))
        .await
        .unwrap();
    assert!(output.is_empty());
    assert!(!state.stream.lock().await.items.contains_key("late-A"));

    manager
        .activate_voice_turn("turn-B", Some(&b.origin_id))
        .await
        .unwrap();
    buffer_without_timer(&manager, &state, "turn-B", "item-B").await;
    manager
        .handoff_out("turn-B", "last B".into(), None)
        .await
        .unwrap();
    assert!(
        matches!(output.recv().await.unwrap(), RealtimeOutbound::HandoffUpdate { handoff_id, .. } if handoff_id == "B")
    );
    manager
        .handle_terminal_handoff_event("turn-A", &aborted_event())
        .await
        .unwrap();
    assert!(
        state
            .voice_routes
            .as_ref()
            .unwrap()
            .lock()
            .await
            .binding("turn-B")
            .is_some()
    );
    assert!(state.stream.lock().await.items.contains_key("item-B"));
    assert!(state.last_output.lock().await.contains_key("turn-B"));
    flush_streamed_handoff_item(&state, "turn-B", "item-B").await;
    assert!(
        matches!(output.recv().await.unwrap(), RealtimeOutbound::HandoffAppend { handoff_id, text, .. }
        if handoff_id == "B" && text == "buffered output")
    );
    assert!(output.is_empty());
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn strict_v2_abort_has_no_completion_but_complete_preserves_final_drain() {
    for abort in [true, false] {
        let (manager, state, output, scope) = manager(RealtimeEventParser::RealtimeV2).await;
        let a = voice_admission_input(ThreadId::new(), &scope, &handoff("A"), "A".into()).unwrap();
        manager.remember_voice_origin(&a).await.unwrap();
        manager
            .activate_voice_turn("turn-A", Some(&a.origin_id))
            .await
            .unwrap();
        let cancellation = state
            .voice_routes
            .as_ref()
            .unwrap()
            .lock()
            .await
            .output_route("turn-A")
            .unwrap()
            .1;
        manager
            .handoff_out("turn-A", "last A".into(), None)
            .await
            .unwrap();
        assert!(matches!(
            output.recv().await.unwrap(),
            RealtimeOutbound::HandoffUpdate { .. }
        ));
        let terminal = if abort {
            aborted_event()
        } else {
            completed_event("turn-A")
        };
        manager
            .handle_terminal_handoff_event("turn-A", &terminal)
            .await
            .unwrap();
        assert!(cancellation.is_cancelled());
        assert!(
            state
                .voice_routes
                .as_ref()
                .unwrap()
                .lock()
                .await
                .binding("turn-A")
                .is_none()
        );
        assert!(!state.last_output.lock().await.contains_key("turn-A"));
        if abort {
            assert!(output.is_empty());
            assert_eq!(
                state
                    .v2_handoff_payload("A", V2HandoffStage::Final, "late final".into())
                    .await,
                None
            );
        } else {
            assert_eq!(
                output.recv().await.unwrap(),
                RealtimeOutbound::CompletedHandoff {
                    handoff_id: "A".into(),
                    text: realtime_backend_output("last A".into(), state.session_kind),
                    phase: None,
                }
            );
            assert_eq!(
                state
                    .v2_handoff_payload("A", V2HandoffStage::Final, "last A".into())
                    .await,
                Some("last A".into())
            );
        }
        manager
            .handle_terminal_handoff_event("turn-A", &terminal)
            .await
            .unwrap();
        assert!(output.is_empty());
        manager.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn non_strict_aborted_event_preserves_upstream_handoff_state() {
    let (manager, mut state, output, _) = manager(RealtimeEventParser::FramelessBidi).await;
    state.voice_routes = None;
    manager
        .state
        .lock()
        .await
        .as_mut()
        .unwrap()
        .handoff
        .voice_routes = None;
    assert_eq!(
        state.receive_handoff(&handoff("ordinary")).await,
        HandoffArrivalEffect::Activated
    );
    buffer_without_timer(&manager, &state, "turn-A", "item-A").await;
    manager
        .handoff_out("turn-A", "ordinary last".into(), None)
        .await
        .unwrap();
    assert!(matches!(
        output.recv().await.unwrap(),
        RealtimeOutbound::HandoffUpdate { .. }
    ));
    manager
        .handle_terminal_handoff_event("turn-A", &aborted_event())
        .await
        .unwrap();
    assert!(output.is_empty());
    assert_eq!(
        state.stream.lock().await.active_handoff.as_deref(),
        Some("ordinary")
    );
    assert!(state.stream.lock().await.items.contains_key("item-A"));
    assert_eq!(
        state.last_output.lock().await.get("turn-A").unwrap().text,
        "ordinary last"
    );
    flush_streamed_handoff_item(&state, "turn-A", "item-A").await;
    assert!(
        matches!(output.recv().await.unwrap(), RealtimeOutbound::HandoffAppend { handoff_id, .. } if handoff_id == "ordinary")
    );
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn abort_cancels_an_already_drained_flush_blocked_on_channel_capacity() {
    use std::future::Future;
    use std::task::Poll;

    let (manager, state, output, scope) = manager(RealtimeEventParser::FramelessBidi).await;
    let a = voice_admission_input(ThreadId::new(), &scope, &handoff("A"), "A".into()).unwrap();
    manager.remember_voice_origin(&a).await.unwrap();
    manager
        .activate_voice_turn("turn-A", Some(&a.origin_id))
        .await
        .unwrap();
    buffer_without_timer(&manager, &state, "turn-A", "item-A").await;
    for _ in 0..16 {
        state
            .output_tx
            .try_send(RealtimeOutbound::StandaloneSpeech {
                text: "sentinel".into(),
            })
            .unwrap();
    }
    let mut flush = Box::pin(flush_streamed_handoff_item(&state, "turn-A", "item-A"));
    std::future::poll_fn(|cx| {
        assert!(flush.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    {
        let stream = state.stream.lock().await;
        let item = stream.items.get("item-A").unwrap();
        assert!(item.buffered_text.is_empty());
        assert!(item.sent_bytes > 0);
    }
    manager
        .handle_terminal_handoff_event("turn-A", &aborted_event())
        .await
        .unwrap();
    // No channel capacity was released: only retirement can make this send ready.
    std::future::poll_fn(|cx| {
        assert!(flush.as_mut().poll(cx).is_ready());
        Poll::Ready(())
    })
    .await;
    assert!(!state.stream.lock().await.items.contains_key("item-A"));
    for _ in 0..16 {
        assert_eq!(
            output.try_recv().unwrap(),
            RealtimeOutbound::StandaloneSpeech {
                text: "sentinel".into()
            }
        );
    }
    assert!(output.is_empty());
    manager.shutdown().await.unwrap();
}

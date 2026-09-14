use super::*;
use crate::realtime_voice_admission::voice_admission_input;
use codex_extension_api::VoiceNativeSessionFuture;
use codex_extension_api::VoiceNativeSessionObserver;
use codex_protocol::protocol::RealtimeHandoffRequested;
use pretty_assertions::assert_eq;

#[derive(Default)]
struct Observer {
    signals: std::sync::Mutex<Vec<VoiceNativeSessionSignal>>,
    reject_ready: bool,
    reject_closed: bool,
}

impl VoiceNativeSessionObserver for Observer {
    fn emit(&self, signal: VoiceNativeSessionSignal) -> VoiceNativeSessionFuture<'_> {
        Box::pin(async move {
            let reject = (self.reject_ready && matches!(&signal.event, VoiceNativeSessionEvent::Ready { .. }))
                || (self.reject_closed && matches!(&signal.event, VoiceNativeSessionEvent::Closed { .. }));
            self.signals.lock().unwrap().push(signal);
            if reject { Err("observer disconnected".into()) } else { Ok(()) }
        })
    }
}

fn provider_session(id: &str) -> RealtimeEvent {
    RealtimeEvent::SessionUpdated { realtime_session_id: id.into(), instructions: None }
}

fn start(observer: Arc<Observer>) -> NativeVoiceSessionStart {
    NativeVoiceSessionStart {
        thread_id: ThreadId::from_string("00000000-0000-0000-0000-000000000001").unwrap(),
        start_id: "reused-start-request".into(),
        hooks: VoiceNativeSessionHooks(observer),
    }
}

#[test]
fn generation_exhaustion_never_wraps_or_exceeds_javascript_safe_integer() {
    let counter = AtomicU64::new(MAX_GENERATION - 1);
    assert_eq!(allocate_generation(&counter), Ok(MAX_GENERATION));
    for _ in 0..2 {
        assert_eq!(allocate_generation(&counter), Err("Native Voice generation exhausted".into()));
    }
    assert_eq!(counter.load(Ordering::Acquire), MAX_GENERATION);
}

#[tokio::test]
async fn provider_lifecycle_seals_before_admission_and_exports_immutable_scope() {
    let observer = Arc::new(Observer::default());
    let native = NativeVoiceSession::begin(start(observer.clone())).await.unwrap();
    assert!(native.scope().await.is_err());
    native.observe(&provider_session("provider-A")).await.unwrap();
    let scope = native.scope().await.unwrap();
    native.observe(&provider_session("provider-A")).await.unwrap();
    assert!(native.observe(&provider_session("provider-B")).await.is_err());
    assert_eq!(native.scope().await.unwrap(), scope);
    let input = voice_admission_input(native.start.thread_id, &scope, &RealtimeHandoffRequested {
        handoff_id: "handoff-A".into(), item_id: "item-A".into(),
        input_transcript: "words".into(), active_transcript: Vec::new(),
    }, "words".into()).unwrap();
    let cancellation = {
        let mut routes = native.routes.lock().await;
        routes.received(&input).unwrap();
        routes.started("turn-A", Some(&input.origin_id)).unwrap();
        routes.output_route("turn-A").unwrap().1
    };
    native.close().await.unwrap();
    native.close().await.unwrap();
    assert!(native.scope().await.is_err());
    assert!(cancellation.is_cancelled());
    assert!(native.routes.lock().await.output_route("turn-A").is_none());
    assert!(native.routes.lock().await.received(&input).is_err());
    let signals = observer.signals.lock().unwrap();
    assert_eq!(signals.iter().map(VoiceNativeSessionSignal::to_json).collect::<Vec<_>>(), vec![
        serde_json::json!({ "type": "nativeSessionStarting", "threadId": native.start.thread_id.to_string(), "startId": "reused-start-request", "voiceGeneration": scope.voice_session_generation }),
        serde_json::json!({ "type": "nativeSessionReady", "threadId": native.start.thread_id.to_string(), "startId": "reused-start-request", "voiceGeneration": scope.voice_session_generation, "nativeSessionId": "provider-A" }),
        serde_json::json!({ "type": "nativeSessionClosed", "threadId": native.start.thread_id.to_string(), "startId": "reused-start-request", "voiceGeneration": scope.voice_session_generation, "nativeSessionId": "provider-A" }),
    ]);
}

#[tokio::test]
async fn observer_rejection_never_seals_ready() {
    let observer = Arc::new(Observer { reject_ready: true, ..Default::default() });
    let native = NativeVoiceSession::begin(start(observer.clone())).await.unwrap();
    assert!(native.observe(&provider_session("provider-A")).await.is_err());
    assert!(native.scope().await.is_err());
    native.close().await.unwrap();
    assert_eq!(observer.signals.lock().unwrap().last().unwrap().event,
        VoiceNativeSessionEvent::Closed { native_session_id: Some("provider-A".into()) });
}

#[tokio::test]
async fn retired_callbacks_cannot_bind_successor_with_reused_start_id() {
    let observer = Arc::new(Observer::default());
    let old = NativeVoiceSession::begin(start(observer.clone())).await.unwrap();
    old.close().await.unwrap();
    let successor = NativeVoiceSession::begin(start(observer.clone())).await.unwrap();
    assert!(old.generation < successor.generation && successor.generation <= MAX_GENERATION);
    assert!(old.observe(&provider_session("late-old-provider")).await.is_err());
    assert!(successor.scope().await.is_err());
    successor.observe(&provider_session("provider-B")).await.unwrap();
    old.close().await.unwrap();
    assert_eq!(successor.scope().await.unwrap(), VoiceAdmissionScope {
        native_session_id: "provider-B".into(), voice_session_generation: successor.generation,
    });
    assert_eq!(observer.signals.lock().unwrap().iter()
        .filter(|signal| matches!(&signal.event, VoiceNativeSessionEvent::Ready { .. })).count(), 1);
}

#[tokio::test]
async fn missing_or_invalid_provider_identity_never_enables_admission() {
    let observer = Arc::new(Observer::default());
    let native = NativeVoiceSession::begin(start(observer.clone())).await.unwrap();
    for id in [String::new(), " ".into(), "s".repeat(1025)] {
        assert!(native.observe(&provider_session(&id)).await.is_err());
        assert!(native.scope().await.is_err());
    }
    native.close().await.unwrap();
    assert_eq!(observer.signals.lock().unwrap().last().unwrap().event,
        VoiceNativeSessionEvent::Closed { native_session_id: None });
}

#[tokio::test]
async fn failed_close_delivery_is_sticky_and_never_acknowledged_by_retry() {
    let observer = Arc::new(Observer { reject_closed: true, ..Default::default() });
    let native = NativeVoiceSession::begin(start(observer.clone())).await.unwrap();
    native.observe(&provider_session("provider-A")).await.unwrap();
    assert_eq!(native.close().await, Err("observer disconnected".into()));
    assert_eq!(native.close().await, Err("observer disconnected".into()));
    assert!(native.scope().await.is_err());
    assert_eq!(observer.signals.lock().unwrap().iter()
        .filter(|signal| matches!(&signal.event, VoiceNativeSessionEvent::Closed { .. })).count(), 1);
}

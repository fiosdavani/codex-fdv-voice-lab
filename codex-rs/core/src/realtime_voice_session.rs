//! Provider-sealed native lifecycle. Generation is process-local, not a durable
//! restart lease. There is no production host registration in this candidate.

use crate::realtime_voice_routing::VoiceTurnRoutes;
use codex_api::RealtimeEvent;
use codex_extension_api::VoiceAdmissionScope;
use codex_extension_api::VoiceNativeSessionEvent;
use codex_extension_api::VoiceNativeSessionHooks;
use codex_extension_api::VoiceNativeSessionSignal;
use codex_protocol::ThreadId;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use tokio::sync::Mutex;
use tokio::sync::Semaphore;

const MAX_GENERATION: u64 = (1 << 53) - 1;
static GENERATION: AtomicU64 = AtomicU64::new(0);

fn allocate_generation(counter: &AtomicU64) -> Result<u64, String> {
    counter
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            current.checked_add(1).filter(|next| *next <= MAX_GENERATION)
        })
        .map(|previous| previous + 1)
        .map_err(|_| "Native Voice generation exhausted".to_string())
}

pub(super) struct NativeVoiceSessionStart {
    pub(super) thread_id: ThreadId,
    pub(super) start_id: String,
    pub(super) hooks: VoiceNativeSessionHooks,
}

enum Phase {
    Starting,
    Sealing(VoiceAdmissionScope),
    Ready(VoiceAdmissionScope),
    Closed {
        native_session_id: Option<String>,
        delivery: Option<Result<(), String>>,
    },
}

pub(super) struct NativeVoiceSession {
    start: NativeVoiceSessionStart,
    generation: u64,
    // Serialize lifecycle I/O without retaining a phase-data mutex guard.
    operation_gate: Semaphore,
    phase: Mutex<Phase>,
    pub(super) routes: Arc<Mutex<VoiceTurnRoutes>>,
}

impl NativeVoiceSession {
    pub(super) fn matches(&self, start_id: &str, generation: u64) -> bool {
        self.start.start_id == start_id && self.generation == generation
    }

    pub(super) async fn begin(start: NativeVoiceSessionStart) -> Result<Arc<Self>, String> {
        let generation = allocate_generation(&GENERATION)?;
        let session = Arc::new(Self {
            start,
            generation,
            operation_gate: Semaphore::new(1),
            phase: Mutex::new(Phase::Starting),
            routes: Arc::new(Mutex::new(VoiceTurnRoutes::pending())),
        });
        if let Err(error) = session.emit(VoiceNativeSessionEvent::Starting).await {
            let _ = session.close().await;
            return Err(error);
        }
        Ok(session)
    }

    async fn emit(&self, event: VoiceNativeSessionEvent) -> Result<(), String> {
        self.start
            .hooks
            .0
            .emit(VoiceNativeSessionSignal {
                thread_id: self.start.thread_id,
                start_id: self.start.start_id.clone(),
                voice_generation: self.generation,
                event,
            })
            .await
    }

    /// Called in this connection's fanout, before handoff admission. Core's
    /// parser obtains this ID from provider session.id, never from start params.
    pub(super) async fn observe(&self, event: &RealtimeEvent) -> Result<(), String> {
        let RealtimeEvent::SessionUpdated {
            realtime_session_id,
            ..
        } = event
        else {
            return Ok(());
        };
        let _operation = self.operation_gate.acquire().await
            .map_err(|_| "Native Voice lifecycle is closed".to_string())?;
        {
            let phase = self.phase.lock().await;
            match &*phase {
                Phase::Closed { .. } => return Err("Native Voice session is closed".into()),
                Phase::Ready(scope) if scope.native_session_id == *realtime_session_id => {
                    return Ok(());
                }
                Phase::Ready(_) => return Err("Provider changed sealed native session identity".into()),
                Phase::Sealing(_) => return Err("Native Voice readiness delivery is unresolved".into()),
                Phase::Starting => {}
            }
        }
        if realtime_session_id.trim().is_empty() || realtime_session_id.len() > 1024 {
            return Err("Provider native session identity is invalid".into());
        }
        let scope = VoiceAdmissionScope {
            native_session_id: realtime_session_id.clone(),
            voice_session_generation: self.generation,
        };
        *self.phase.lock().await = Phase::Sealing(scope.clone());
        self.emit(VoiceNativeSessionEvent::Ready {
            native_session_id: realtime_session_id.clone(),
        })
        .await?;
        self.routes.lock().await.seal(scope.clone());
        *self.phase.lock().await = Phase::Ready(scope);
        Ok(())
    }

    pub(super) async fn scope(&self) -> Result<VoiceAdmissionScope, &'static str> {
        match &*self.phase.lock().await {
            Phase::Ready(scope) => Ok(scope.clone()),
            Phase::Starting | Phase::Sealing(_) | Phase::Closed { .. } => {
                Err("Native Voice session is not ready")
            }
        }
    }

    pub(super) async fn retire(&self) {
        let Ok(_operation) = self.operation_gate.acquire().await else {
            return;
        };
        self.retire_serialized().await;
    }

    /// Both callers hold operation_gate through retirement and any observer I/O.
    async fn retire_serialized(&self) {
        {
            let mut phase = self.phase.lock().await;
            match &*phase {
                Phase::Closed { .. } => {}
                Phase::Starting => {
                    *phase = Phase::Closed {
                        native_session_id: None,
                        delivery: None,
                    };
                }
                Phase::Ready(scope) | Phase::Sealing(scope) => {
                    let native_session_id = Some(scope.native_session_id.clone());
                    *phase = Phase::Closed {
                        native_session_id,
                        delivery: None,
                    };
                }
            }
        }
        self.routes.lock().await.close();
    }

    /// Call after the owned transport has stopped. Failed delivery is sticky;
    /// another close cannot convert uncertainty into an acknowledgement.
    pub(super) async fn close(&self) -> Result<(), String> {
        let _operation = self.operation_gate.acquire().await
            .map_err(|_| "Native Voice lifecycle is closed".to_string())?;
        self.retire_serialized().await;
        let native_session_id = {
            let phase = self.phase.lock().await;
            let Phase::Closed { native_session_id, delivery } = &*phase else {
                unreachable!("retire closes the native lifecycle");
            };
            if let Some(result) = delivery {
                return result.clone();
            }
            native_session_id.clone()
        };
        let result = self
            .emit(VoiceNativeSessionEvent::Closed {
                native_session_id: native_session_id.clone(),
            })
            .await;
        *self.phase.lock().await = Phase::Closed {
            native_session_id,
            delivery: Some(result.clone()),
        };
        result
    }
}

#[cfg(test)]
#[path = "realtime_voice_session_tests.rs"]
mod tests;

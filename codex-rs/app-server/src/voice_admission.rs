//! Host adapter: Core's abstract Voice seam to the already installed queue.

use std::sync::Arc;

use codex_core::TurnInput;
use codex_extension_api::VoiceAdmission;
use codex_extension_api::VoiceAdmissionAck;
use codex_extension_api::VoiceAdmissionFuture;
use codex_extension_api::VoiceAdmissionInput;
use codex_protocol::user_input::UserInput;
use codex_queue_extension::QueuedItemService;
use codex_queue_extension::VoiceQueueOrigin;

pub(super) struct QueueVoiceAdmission(pub(super) Arc<QueuedItemService>);

impl VoiceAdmission for QueueVoiceAdmission {
    fn admit(&self, input: VoiceAdmissionInput) -> VoiceAdmissionFuture<'_> {
        Box::pin(async move {
            let receipt = self
                .0
                .enqueue_voice(
                    input.thread_id,
                    TurnInput::UserInput {
                        content: vec![UserInput::Text {
                            text: input.text,
                            text_elements: Vec::new(),
                        }],
                        client_id: Some(input.origin_id.clone()),
                    },
                    VoiceQueueOrigin {
                        native_session_id: input.scope.native_session_id,
                        voice_session_generation: input.scope.voice_session_generation,
                        origin_id: input.origin_id,
                        handoff_id: input.handoff_id,
                        item_id: input.item_id,
                    },
                )
                .await
                .map_err(|error| error.to_string())?;
            Ok(VoiceAdmissionAck {
                queued_item_id: receipt.queued_item_id,
            })
        })
    }
}

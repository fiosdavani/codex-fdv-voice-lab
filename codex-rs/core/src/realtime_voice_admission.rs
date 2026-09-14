//! Origin preparation for the explicitly enabled durable Voice path.

use codex_extension_api::VoiceAdmissionInput;
use codex_extension_api::VoiceAdmissionScope;
use codex_protocol::ThreadId;
use codex_protocol::protocol::RealtimeHandoffRequested;

pub(crate) fn voice_admission_input(
    thread_id: ThreadId,
    scope: &VoiceAdmissionScope,
    handoff: &RealtimeHandoffRequested,
    text: String,
) -> Result<VoiceAdmissionInput, &'static str> {
    if scope.voice_session_generation == 0
        || scope.voice_session_generation > i64::MAX as u64
        || scope.native_session_id.is_empty()
        || scope.native_session_id.len() > 1024
        || handoff.handoff_id.is_empty()
        || handoff.handoff_id.len() > 1024
        || handoff.item_id.len() > 1024
        || text.trim().is_empty()
    {
        return Err("Voice origin identity is missing or invalid");
    }
    // An unambiguous identity tuple, never a text hash. A provider handoff is
    // the admission domain here; it is not asserted to equal every STT utterance.
    let origin_id = serde_json::to_string(&(
        &scope.native_session_id,
        scope.voice_session_generation,
        &handoff.handoff_id,
    ))
    .map_err(|_| "Voice origin serialization failed")?;
    Ok(VoiceAdmissionInput {
        thread_id,
        scope: scope.clone(),
        origin_id,
        handoff_id: Some(handoff.handoff_id.clone()),
        item_id: (!handoff.item_id.is_empty()).then(|| handoff.item_id.clone()),
        text,
    })
}

#[cfg(test)]
#[path = "realtime_voice_admission_tests.rs"]
mod tests;

//! Session-local routing, deliberately distinct from durable queue admission.
//!
//! Receiving an origin records no active handoff. Only Core's accepted StartIfIdle
//! path binds a turn, before spawning its task. This map is not replay authority.

use std::collections::HashMap;

use codex_extension_api::VoiceAdmissionInput;
use codex_extension_api::VoiceAdmissionScope;
use tokio_util::sync::CancellationToken;

const MAX_SESSION_ORIGINS: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VoiceTurnBinding {
    pub(crate) native_session_id: String,
    pub(crate) voice_session_generation: u64,
    pub(crate) turn_id: String,
    pub(crate) origin_id: String,
    pub(crate) client_id: String,
    pub(crate) handoff_id: String,
}

#[derive(Debug)]
struct ReceivedOrigin {
    handoff_id: String,
    item_id: Option<String>,
    turn_id: Option<String>,
    completed: bool,
    output_cancellation: CancellationToken,
}

#[derive(Debug)]
pub(crate) struct VoiceTurnRoutes {
    scope: Option<VoiceAdmissionScope>,
    origins: HashMap<String, ReceivedOrigin>,
    turns: HashMap<String, VoiceTurnBinding>,
}

impl VoiceTurnRoutes {
    #[cfg(test)]
    pub(crate) fn new(scope: VoiceAdmissionScope) -> Self {
        Self {
            scope: Some(scope),
            origins: HashMap::new(),
            turns: HashMap::new(),
        }
    }

    pub(crate) fn pending() -> Self {
        Self {
            scope: None,
            origins: HashMap::new(),
            turns: HashMap::new(),
        }
    }

    pub(crate) fn seal(&mut self, scope: VoiceAdmissionScope) {
        assert!(self.scope.is_none());
        self.scope = Some(scope);
    }

    pub(crate) fn close(&mut self) {
        for origin in self.origins.values_mut() {
            origin.completed = true;
            origin.output_cancellation.cancel();
        }
        self.turns.clear();
        self.scope = None;
    }

    pub(crate) fn received(&mut self, input: &VoiceAdmissionInput) -> Result<(), &'static str> {
        if self.scope.as_ref() != Some(&input.scope) {
            return Err("Voice origin belongs to another native session");
        }
        let handoff_id = input
            .handoff_id
            .as_deref()
            .filter(|id| !id.is_empty())
            .ok_or("Voice handoff identity missing")?;
        if let Some(existing) = self.origins.get(&input.origin_id) {
            return if existing.handoff_id == handoff_id && existing.item_id == input.item_id {
                Ok(())
            } else {
                Err("Conflicting Voice origin routing identity")
            };
        }
        if self.origins.len() >= MAX_SESSION_ORIGINS {
            return Err("Voice routing session capacity reached; no implicit eviction");
        }
        self.origins.insert(
            input.origin_id.clone(),
            ReceivedOrigin {
                handoff_id: handoff_id.to_string(),
                item_id: input.item_id.clone(),
                turn_id: None,
                completed: false,
                output_cancellation: CancellationToken::new(),
            },
        );
        Ok(())
    }

    /// Called after StartIfIdle reserves and accepts the turn, before task spawn.
    /// Unknown client IDs are ordinary non-Voice turns: they gain no Voice route.
    pub(crate) fn started(
        &mut self,
        turn_id: &str,
        client_id: Option<&str>,
    ) -> Result<(), &'static str> {
        let Some(client_id) = client_id else {
            return Ok(());
        };
        let Some(origin) = self.origins.get_mut(client_id) else {
            return Ok(());
        };
        if turn_id.is_empty() {
            return Err("Voice turn identity missing");
        }
        if let Some(existing) = &origin.turn_id {
            return if existing == turn_id && !origin.completed {
                Ok(())
            } else {
                Err("Voice origin already bound; no rebind after completion")
            };
        }
        if self.turns.contains_key(turn_id) {
            return Err("Turn already belongs to another Voice origin");
        }
        let scope = self.scope.as_ref().ok_or("Voice session is not sealed")?;
        origin.turn_id = Some(turn_id.to_string());
        self.turns.insert(
            turn_id.to_string(),
            VoiceTurnBinding {
                native_session_id: scope.native_session_id.clone(),
                voice_session_generation: scope.voice_session_generation,
                turn_id: turn_id.to_string(),
                origin_id: client_id.to_string(),
                client_id: client_id.to_string(),
                handoff_id: origin.handoff_id.clone(),
            },
        );
        Ok(())
    }

    pub(crate) fn binding(&self, turn_id: &str) -> Option<&VoiceTurnBinding> {
        let binding = self.turns.get(turn_id)?;
        (!self.origins.get(&binding.origin_id)?.completed).then_some(binding)
    }

    pub(crate) fn output_route(
        &self,
        turn_id: &str,
    ) -> Option<(VoiceTurnBinding, CancellationToken)> {
        let binding = self.binding(turn_id)?;
        let origin = self.origins.get(&binding.origin_id)?;
        Some((binding.clone(), origin.output_cancellation.clone()))
    }

    /// Queued outbound frames carry their handoff identity already. A completed
    /// origin remains known so its final queued frame can drain after TurnComplete.
    pub(crate) fn was_started(&self, handoff_id: &str) -> bool {
        self.turns
            .values()
            .any(|binding| binding.handoff_id == handoff_id)
    }

    pub(crate) fn complete(&mut self, turn_id: &str) {
        if let Some(binding) = self.turns.get(turn_id)
            && let Some(origin) = self.origins.get_mut(&binding.origin_id)
        {
            origin.completed = true;
            origin.output_cancellation.cancel();
        }
    }

    /// Retain the origin tombstone, but do not authorize aborted final frames.
    /// A duplicate arrival/start must never create a fresh emission token.
    pub(crate) fn abort(&mut self, turn_id: &str) {
        self.complete(turn_id);
        self.turns.remove(turn_id);
    }
}

#[cfg(test)]
#[path = "realtime_voice_routing_tests.rs"]
mod tests;

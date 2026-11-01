//! `PinContext` — pin precedence per ADR §D5.
//!
//! "Precedence (highest wins), with tests asserting each level: explicit
//! per-turn user pin -> per-agent override (`OrchestrationConfig.agents` —
//! an explicit pin, router never overrides it) -> session pin = hard wall
//! (a pinned session gets NO routing votes; the router only acts where the
//! user has not pinned) -> router decision -> configured default."
//!
//! This module only encodes the precedence resolution as a pure function;
//! it does not import `host_config` (that coupling belongs to whichever
//! call site wires `triage()` up, per the lane fence keeping this crate to a
//! single shared-file touch).

use serde::{Deserialize, Serialize};

/// The full set of pin inputs at every precedence level, as supplied by the
/// caller for one triage call. Each level is `None` when nothing pins that
/// level for this request.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PinContext {
    /// Explicit per-turn user pin — highest precedence.
    pub per_turn_pin: Option<String>,
    /// Per-agent override, e.g. from `OrchestrationConfig.agents`.
    pub per_agent_override: Option<String>,
    /// Session pin — a HARD WALL. When set, the router contributes no vote
    /// at all for this request; the session pin is returned as-is.
    pub session_pin: Option<String>,
    /// The configured default model id, used only if every other level
    /// (including the router's own decision) yields nothing.
    pub configured_default: Option<String>,
}

/// Which precedence level, if any, resolved the pin. `Router` means no pin
/// applied and the caller should proceed to `triage()`'s heuristic
/// decision; `ConfiguredDefault` is the last-resort fallback used only when
/// the router itself produces no candidate (should not happen in practice
/// since `triage()` always returns a model, but kept as an explicit,
/// exhaustively-testable level).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PinSource {
    PerTurn,
    PerAgentOverride,
    SessionPin,
    Router,
    ConfiguredDefault,
}

/// Result of resolving pin precedence: either a pin wins outright (`model_id`
/// is `Some`, `source` names the winning level, and the router must not run),
/// or nothing pins this request and the router should decide
/// (`model_id` is `None`, `source` is `PinSource::Router`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PinResolution {
    pub model_id: Option<String>,
    pub source: PinSource,
}

impl PinResolution {
    /// True when a pin resolved the model choice and the router must not
    /// override it (every level above `Router` is a hard pin).
    pub fn is_pinned(&self) -> bool {
        self.source != PinSource::Router
    }
}

/// Resolve pin precedence for one request. Pure function: same
/// [`PinContext`] always yields the same [`PinResolution`].
pub fn resolve(ctx: &PinContext) -> PinResolution {
    if let Some(model_id) = &ctx.per_turn_pin {
        return PinResolution {
            model_id: Some(model_id.clone()),
            source: PinSource::PerTurn,
        };
    }
    if let Some(model_id) = &ctx.per_agent_override {
        return PinResolution {
            model_id: Some(model_id.clone()),
            source: PinSource::PerAgentOverride,
        };
    }
    if let Some(model_id) = &ctx.session_pin {
        return PinResolution {
            model_id: Some(model_id.clone()),
            source: PinSource::SessionPin,
        };
    }
    PinResolution {
        model_id: None,
        source: PinSource::Router,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_turn_pin_wins_over_everything() {
        let ctx = PinContext {
            per_turn_pin: Some("turn-model".to_string()),
            per_agent_override: Some("agent-model".to_string()),
            session_pin: Some("session-model".to_string()),
            configured_default: Some("default-model".to_string()),
        };
        let resolution = resolve(&ctx);
        assert_eq!(resolution.model_id.as_deref(), Some("turn-model"));
        assert_eq!(resolution.source, PinSource::PerTurn);
        assert!(resolution.is_pinned());
    }

    #[test]
    fn per_agent_override_wins_when_no_per_turn_pin() {
        let ctx = PinContext {
            per_turn_pin: None,
            per_agent_override: Some("agent-model".to_string()),
            session_pin: Some("session-model".to_string()),
            configured_default: Some("default-model".to_string()),
        };
        let resolution = resolve(&ctx);
        assert_eq!(resolution.model_id.as_deref(), Some("agent-model"));
        assert_eq!(resolution.source, PinSource::PerAgentOverride);
    }

    #[test]
    fn session_pin_is_a_hard_wall_when_higher_levels_absent() {
        let ctx = PinContext {
            per_turn_pin: None,
            per_agent_override: None,
            session_pin: Some("session-model".to_string()),
            configured_default: Some("default-model".to_string()),
        };
        let resolution = resolve(&ctx);
        assert_eq!(resolution.model_id.as_deref(), Some("session-model"));
        assert_eq!(resolution.source, PinSource::SessionPin);
        assert!(resolution.is_pinned());
    }

    #[test]
    fn no_pins_falls_through_to_router() {
        let ctx = PinContext::default();
        let resolution = resolve(&ctx);
        assert_eq!(resolution.model_id, None);
        assert_eq!(resolution.source, PinSource::Router);
        assert!(!resolution.is_pinned());
    }

    #[test]
    fn configured_default_never_resolved_by_this_function_directly() {
        // configured_default is the LAST resort, consumed by the caller only
        // if triage() itself fails to produce a candidate — resolve() never
        // returns it directly, since the router level sits above it.
        let ctx = PinContext {
            configured_default: Some("default-model".to_string()),
            ..Default::default()
        };
        let resolution = resolve(&ctx);
        assert_eq!(resolution.source, PinSource::Router);
    }
}

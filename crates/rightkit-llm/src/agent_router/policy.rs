//! `RoutingPolicy` — the named-weight, single-threshold aggressiveness knob
//! (ADR §D3: "One numeric knob: route-down confidence threshold.
//! conservative = 0.8 (default), balanced = 0.7, aggressive = 0.6.").
//!
//! Mirrors `crate::agent_providers::model_metadata`'s loader pattern: a bundled
//! TOML asset (`assets/policy.conservative.toml`) embedded via
//! `include_str!` as the offline-safe compile-time default, describing every
//! signal weight the router consults plus the route-down threshold. Balanced
//! and aggressive presets are derived from the conservative threshold at
//! load time (threshold - 0.1 / - 0.2) rather than shipped as separate
//! committed files, per the lane spec.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Named per-signal weights feeding the route-down confidence score. Every
/// signal `triage()` consults must have a named weight here.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SignalWeights {
    pub low_tool_call_density: f64,
    pub no_failure_streak: f64,
    pub narrow_prompt_shape: f64,
    pub single_file_scope: f64,
    pub budget_headroom: f64,
}

/// One named aggressiveness preset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingPolicy {
    pub name: String,
    pub route_down_confidence_threshold: f64,
    pub weights: SignalWeights,
    pub context_window_headroom_ratio: f64,
}

/// Errors loading or parsing a routing-policy file.
#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("failed to parse routing policy TOML: {0}")]
    Toml(String),
}

const EMBEDDED_CONSERVATIVE_TOML: &str = include_str!("../../assets/policy.conservative.toml");

fn parse(toml_str: &str) -> Result<RoutingPolicy, PolicyError> {
    toml::from_str(toml_str).map_err(|e| PolicyError::Toml(e.to_string()))
}

impl RoutingPolicy {
    /// Load the bundled conservative policy (the default aggressiveness).
    /// Panics only if the bundled asset itself fails to parse — a build-time
    /// defect, never a runtime one.
    pub fn conservative() -> Self {
        parse(EMBEDDED_CONSERVATIVE_TOML).expect("bundled policy.conservative.toml must parse")
    }

    /// Derive the balanced preset: conservative's threshold minus 0.1, per
    /// ADR §D3. Weights and headroom ratio are unchanged — only the
    /// aggressiveness knob moves.
    pub fn balanced() -> Self {
        let mut policy = Self::conservative();
        policy.name = "balanced".to_string();
        policy.route_down_confidence_threshold -= 0.1;
        policy
    }

    /// Derive the aggressive preset: conservative's threshold minus 0.2.
    pub fn aggressive() -> Self {
        let mut policy = Self::conservative();
        policy.name = "aggressive".to_string();
        policy.route_down_confidence_threshold -= 0.2;
        policy
    }

    /// Load a policy override from `<dir>/policy.<name>.toml`, falling back
    /// to the named built-in preset (conservative/balanced/aggressive) when
    /// the file is missing, unreadable, or malformed — a broken override
    /// must never crash the daemon.
    pub fn load_from_dir_or_default(dir: &Path, name: &str) -> Self {
        let path = dir.join(format!("policy.{name}.toml"));
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(policy) = parse(&content) {
                return policy;
            }
        }
        Self::preset_by_name(name)
    }

    /// Resolve a named preset, defaulting to conservative for any unknown
    /// name (fail safe, never fail aggressive).
    pub fn preset_by_name(name: &str) -> Self {
        match name {
            "balanced" => Self::balanced(),
            "aggressive" => Self::aggressive(),
            _ => Self::conservative(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conservative_threshold_is_point_eight() {
        let policy = RoutingPolicy::conservative();
        assert!((policy.route_down_confidence_threshold - 0.8).abs() < 1e-9);
        assert_eq!(policy.name, "conservative");
    }

    #[test]
    fn balanced_is_conservative_minus_point_one() {
        let conservative = RoutingPolicy::conservative();
        let balanced = RoutingPolicy::balanced();
        assert!(
            (balanced.route_down_confidence_threshold
                - (conservative.route_down_confidence_threshold - 0.1))
                .abs()
                < 1e-9
        );
        assert_eq!(balanced.name, "balanced");
    }

    #[test]
    fn aggressive_is_conservative_minus_point_two() {
        let conservative = RoutingPolicy::conservative();
        let aggressive = RoutingPolicy::aggressive();
        assert!(
            (aggressive.route_down_confidence_threshold
                - (conservative.route_down_confidence_threshold - 0.2))
                .abs()
                < 1e-9
        );
        assert_eq!(aggressive.name, "aggressive");
    }

    #[test]
    fn weights_all_present_and_positive() {
        let policy = RoutingPolicy::conservative();
        assert!(policy.weights.low_tool_call_density > 0.0);
        assert!(policy.weights.no_failure_streak > 0.0);
        assert!(policy.weights.narrow_prompt_shape > 0.0);
        assert!(policy.weights.single_file_scope > 0.0);
        assert!(policy.weights.budget_headroom > 0.0);
    }

    #[test]
    fn context_window_headroom_ratio_is_twenty_percent() {
        let policy = RoutingPolicy::conservative();
        assert!((policy.context_window_headroom_ratio - 0.20).abs() < 1e-9);
    }

    #[test]
    fn override_file_wins_over_builtin() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("policy.conservative.toml"),
            r#"
            name = "conservative"
            route_down_confidence_threshold = 0.95
            context_window_headroom_ratio = 0.30

            [weights]
            low_tool_call_density = 0.1
            no_failure_streak = 0.1
            narrow_prompt_shape = 0.1
            single_file_scope = 0.1
            budget_headroom = 0.1
            "#,
        )
        .unwrap();

        let policy = RoutingPolicy::load_from_dir_or_default(dir.path(), "conservative");
        assert!((policy.route_down_confidence_threshold - 0.95).abs() < 1e-9);
    }

    #[test]
    fn missing_override_falls_back_to_builtin_preset() {
        let dir = tempfile::tempdir().unwrap();
        let policy = RoutingPolicy::load_from_dir_or_default(dir.path(), "balanced");
        assert_eq!(policy.name, "balanced");
    }

    #[test]
    fn malformed_override_falls_back_to_builtin_preset() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("policy.conservative.toml"), "not = = toml").unwrap();
        let policy = RoutingPolicy::load_from_dir_or_default(dir.path(), "conservative");
        assert_eq!(policy.name, "conservative");
    }

    #[test]
    fn unknown_preset_name_defaults_to_conservative() {
        let policy = RoutingPolicy::preset_by_name("nonexistent");
        assert_eq!(policy.name, "conservative");
    }
}

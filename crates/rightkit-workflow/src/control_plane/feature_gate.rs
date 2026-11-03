//! Explicit feature-gate seam for the workflow control plane.
//!
//! The workflow plane is **disabled by default** at every level — daemon
//! supervisor (WP-03), API surface (WP-09), and desktop UI (WP-09).
//! This module re-exports the V1 default as a single function so any
//! accidental code path that wants to act on a workflow must first
//! consult [`FeatureGate::is_enabled`].

/// Returns the V1 product default for the workflow feature gate.
///
/// The default is `false`. Flipping it to `true` before the WP-08 release
/// gate is a contract violation; see ADR-0021.
pub const fn workflow_feature_default() -> bool {
    false
}

/// Lightweight value object mirroring
/// `host_config::types::FeatureConfig::workflows_enabled`.
///
/// The daemon owns the canonical config struct; this wrapper lets pure
/// domain code ask the gate question without taking a dependency on
/// `host_config`. The daemon bridges the two in WP-09.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeatureGate {
    enabled: bool,
}

impl FeatureGate {
    /// Build a feature gate from a raw bool.
    pub const fn from_bool(enabled: bool) -> Self {
        Self { enabled }
    }

    /// Build the V1 default feature gate (disabled).
    pub const fn v1_default() -> Self {
        Self {
            enabled: workflow_feature_default(),
        }
    }

    /// Resolve the gate from a `Option<bool>` config value, applying the
    /// V1 default when unset.
    pub fn from_config(config_value: Option<bool>) -> Self {
        Self {
            enabled: config_value.unwrap_or(workflow_feature_default()),
        }
    }

    /// Resolve the gate from a process environment variable. Recognizes
    /// `RIGHTKIT_WORKFLOWS_ENABLED=1|true|yes` (case-insensitive). When
    /// unset, malformed, or unrecognized, falls back to the V1 default.
    ///
    /// Production config still wins: the daemon passes a [`FeatureGate`]
    /// derived from `CodeRightConfig.features.workflows_enabled` rather
    /// than letting `env::var` override the config.
    pub fn from_env_or_default() -> Self {
        match std::env::var("RIGHTKIT_WORKFLOWS_ENABLED") {
            Ok(raw) => match raw.to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => Self::from_bool(true),
                "0" | "false" | "no" | "off" => Self::from_bool(false),
                _ => Self::v1_default(),
            },
            Err(_) => Self::v1_default(),
        }
    }

    /// True when the workflow plane may run.
    pub const fn is_enabled(self) -> bool {
        self.enabled
    }

    /// Convenience: returns `Ok(())` if enabled, otherwise a typed error
    /// describing what is gated.
    pub fn require_enabled(self) -> Result<(), FeatureGateError> {
        if self.enabled {
            Ok(())
        } else {
            Err(FeatureGateError::Disabled)
        }
    }
}

impl Default for FeatureGate {
    fn default() -> Self {
        Self::v1_default()
    }
}

/// Typed failure for "feature gate disabled". Replaces free-form
/// "feature disabled" strings throughout the workflow crate.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FeatureGateError {
    /// The V1 default is `false`; this is the gate refusal.
    #[error(
        "autonomous workflow control plane is disabled (set `features.workflows_enabled = true` in config to enable)"
    )]
    Disabled,
}

//! The feature flags of `[features]` as a table: name, state and default.
//!
//! `features list` prints it. The defaults are the ones the accessors on
//! [`FeatureConfig`] apply (`self_review_enabled`, `turn_checkpoints_enabled`, ...), so the table
//! and the code that reads each flag cannot disagree without a unit test here failing.

use crate::FeatureConfig;

/// One row of `features list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeatureFlagRow {
    /// The kebab-case key under `[features]`.
    pub name: &'static str,
    /// Whether the flag is on for this configuration.
    pub enabled: bool,
    /// What the flag is when no file sets it.
    pub default: bool,
    /// Whether a config layer (file, `-c` or environment) set it.
    pub set: bool,
}

impl FeatureConfig {
    /// Every on/off flag of `[features]`, in a fixed order.
    pub fn flag_rows(&self) -> Vec<FeatureFlagRow> {
        fn row(name: &'static str, value: Option<bool>, default: bool) -> FeatureFlagRow {
            FeatureFlagRow {
                name,
                enabled: value.unwrap_or(default),
                default,
                set: value.is_some(),
            }
        }
        vec![
            // headless-cli.md: `features.cli_acquisition_v1` defaults to true.
            row("cli-acquisition-v1", self.cli_acquisition_v1, true),
            row(
                "turn-checkpoints",
                self.turn_checkpoints,
                FeatureConfig::default().turn_checkpoints_enabled(),
            ),
            row(
                "auto-commit-on-exit",
                self.auto_commit_on_exit,
                FeatureConfig::default().auto_commit_on_exit_enabled(),
            ),
            row(
                "self-review",
                self.self_review,
                FeatureConfig::default().self_review_enabled(),
            ),
            row(
                "advisor",
                self.advisor,
                FeatureConfig::default().advisor_enabled(),
            ),
        ]
    }
}

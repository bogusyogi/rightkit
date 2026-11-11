//! Fine-tuning threshold and export logic.
//!
//! Determines when to export approved trajectories for on-policy fine-tuning via LoRA
//! and documents the decision for auditing.

use serde::{Deserialize, Serialize};

/// Decision record for fine-tuning export.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportPlan {
    /// Whether the threshold is met for export.
    pub should_export: bool,
    /// Number of qualifying records available.
    pub record_count: usize,
    /// Strategy being used (e.g., "LoRA on-policy via SDAR").
    pub strategy: String,
    /// Notes about the decision.
    pub notes: String,
}

/// Fine-tuning trigger: determines export readiness and strategy.
#[derive(Debug, Clone)]
pub struct FineTuneTrigger {
    /// Minimum number of records to trigger export.
    pub threshold_count: usize,
    /// Minimum Good Data Signal (GDS) score to include a record.
    pub min_gds: f64,
}

impl Default for FineTuneTrigger {
    fn default() -> Self {
        Self {
            threshold_count: 500,
            min_gds: 0.90,
        }
    }
}

impl FineTuneTrigger {
    /// Create a trigger with custom thresholds.
    pub fn new(threshold_count: usize, min_gds: f64) -> Self {
        Self {
            threshold_count,
            min_gds,
        }
    }

    /// Check whether we have enough qualifying records to export.
    pub fn should_export(&self, gds_records: &[f64]) -> bool {
        let qualifying = gds_records
            .iter()
            .filter(|&&gds| gds >= self.min_gds)
            .count();
        qualifying >= self.threshold_count
    }

    /// Generate an export plan documenting the decision.
    pub fn export_plan(&self, gds_records: &[f64]) -> ExportPlan {
        let qualifying = gds_records
            .iter()
            .filter(|&&gds| gds >= self.min_gds)
            .count();
        let should_export = self.should_export(gds_records);

        let strategy = if should_export {
            "LoRA on-policy via SDAR".to_string()
        } else {
            "No export: insufficient GDS records".to_string()
        };

        let notes = if should_export {
            format!(
                "Export on-policy trajectories to LoRA via SDAR. {} records with GDS >= {} qualified.",
                qualifying, self.min_gds
            )
        } else {
            format!(
                "Need {} records with GDS >= {}, have {} qualifying.",
                self.threshold_count, self.min_gds, qualifying
            )
        };

        ExportPlan {
            should_export,
            record_count: qualifying,
            strategy,
            notes,
        }
    }
}

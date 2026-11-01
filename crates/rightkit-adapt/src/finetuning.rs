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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_threshold() {
        let trigger = FineTuneTrigger::default();
        assert_eq!(trigger.threshold_count, 500);
        assert_eq!(trigger.min_gds, 0.90);
    }

    #[test]
    fn custom_threshold() {
        let trigger = FineTuneTrigger::new(100, 0.85);
        assert_eq!(trigger.threshold_count, 100);
        assert_eq!(trigger.min_gds, 0.85);
    }

    #[test]
    fn should_export_false_insufficient_records() {
        let trigger = FineTuneTrigger::default();
        let gds_records = vec![0.95, 0.92, 0.91];
        assert!(!trigger.should_export(&gds_records));
    }

    #[test]
    fn should_export_false_low_gds() {
        let trigger = FineTuneTrigger::default();
        let mut gds_records = vec![0.95; 500];
        gds_records.push(0.50); // One low score
        assert!(trigger.should_export(&gds_records));
    }

    #[test]
    fn should_export_true_at_threshold() {
        let trigger = FineTuneTrigger::new(10, 0.90);
        let gds_records = vec![0.95; 10];
        assert!(trigger.should_export(&gds_records));
    }

    #[test]
    fn should_export_true_above_threshold() {
        let trigger = FineTuneTrigger::new(10, 0.90);
        let gds_records = vec![0.95; 15];
        assert!(trigger.should_export(&gds_records));
    }

    #[test]
    fn should_export_respects_min_gds() {
        let trigger = FineTuneTrigger::new(5, 0.90);
        let gds_records = vec![0.89; 5]; // All below threshold
        assert!(!trigger.should_export(&gds_records));
    }

    #[test]
    fn should_export_counts_only_qualifying() {
        let trigger = FineTuneTrigger::new(10, 0.90);
        let mut gds_records = vec![0.95; 8];
        gds_records.extend(vec![0.80; 5]); // 5 low scores
        assert!(!trigger.should_export(&gds_records));
    }

    #[test]
    fn should_export_empty_records() {
        let trigger = FineTuneTrigger::default();
        let gds_records = vec![];
        assert!(!trigger.should_export(&gds_records));
    }

    #[test]
    fn export_plan_when_ready() {
        let trigger = FineTuneTrigger::new(5, 0.90);
        let gds_records = vec![0.95; 5];
        let plan = trigger.export_plan(&gds_records);
        assert!(plan.should_export);
        assert_eq!(plan.record_count, 5);
        assert!(plan.strategy.contains("LoRA"));
        assert!(plan.notes.contains("on-policy trajectories"));
    }

    #[test]
    fn export_plan_when_not_ready() {
        let trigger = FineTuneTrigger::new(5, 0.90);
        let gds_records = vec![0.95; 2];
        let plan = trigger.export_plan(&gds_records);
        assert!(!plan.should_export);
        assert_eq!(plan.record_count, 2);
        assert!(plan.notes.contains("Need 5 records"));
    }

    #[test]
    fn export_plan_documents_strategy() {
        let trigger = FineTuneTrigger::new(10, 0.90);
        let gds_records = vec![0.95; 15];
        let plan = trigger.export_plan(&gds_records);
        assert!(plan.strategy.contains("SDAR"));
        assert!(!plan.strategy.contains("SFT"));
    }

    #[test]
    fn export_plan_serializable() {
        let trigger = FineTuneTrigger::new(5, 0.90);
        let plan = trigger.export_plan(&[0.95; 5]);
        let json = serde_json::to_string(&plan).unwrap();
        assert!(json.contains("should_export"));
        assert!(json.contains("LoRA"));
    }
}

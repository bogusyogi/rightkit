//! Loop detector configuration — the single config block that owns
//! every threshold the loop-guard family uses (parity P-3).
//!
//! Replaces the hard-coded thresholds previously inlined at the call
//! sites in `engine_loop.rs` (`MopDetector::new(8, 4)`,
//! `ToolLoopDetector::default()`) AND adds the new idempotency-axis
//! thresholds. `LoopDetectorConfig::default()` reproduces the
//! pre-P-3 values byte-identically so a lifted caller sees no
//! behavior change.
//!
//! Idempotency classification (mutating vs read-only) reuses the
//! existing capability/permission metadata — `Tool::is_read_only()`
//! is the same surface `classify_programmatic` uses for the iteration
//! budget. No parallel taxonomy is introduced.

/// All loop-detector thresholds in one place. Defaults reproduce the
/// pre-P-3 behavior so unwired callers see no behavior change.
#[derive(Debug, Clone)]
pub struct LoopDetectorConfig {
    // MopDetector knobs (legacy — kept named the same way the
    // constructor took them).
    pub mop_window_size: usize,
    pub mop_repeat_threshold: usize,
    pub mop_min_distinct_ratio: f64,
    // ToolLoopDetector knobs (legacy — kept named the same way the
    // struct fields took them).
    pub tool_loop_window_size: usize,
    pub exact_warn_threshold: usize,
    pub exact_stop_threshold: usize,
    pub kind_warn_threshold: usize,
    pub kind_stop_threshold: usize,
    // NEW — idempotency axis (parity P-3).
    /// Repeated identical MUTATING-call warn threshold. Mutating
    /// tools have a lower ceiling than read-only — a redundant write
    /// is more dangerous than a redundant read.
    pub mutating_repeat_warn_threshold: usize,
    /// Repeated identical READ-ONLY-call warn threshold. Higher
    /// than the mutating ceiling because legitimate agents often do
    /// 3-5 reads in a row to gather context for one decision.
    pub read_only_repeat_warn_threshold: usize,
    /// Same tool, varying args, warn threshold. The legacy
    /// `kind_warn_threshold` axis (broad tool-name runaway) already
    /// exists at 30/40; this is the new LOWER-warn trigger so the
    /// model sees the message earlier than the existing hard-stop.
    pub same_name_varying_warn_threshold: usize,
}

impl Default for LoopDetectorConfig {
    fn default() -> Self {
        // All values reproduce pre-P-3 hard-coded call sites
        // (`engine_loop.rs`: MopDetector::new(8, 4); ToolLoopDetector::default()).
        // Idempotency axis uses the lane-specified defaults: 2 mutating,
        // 4 read-only, 6 same-name-varying.
        Self {
            mop_window_size: 8,
            mop_repeat_threshold: 4,
            mop_min_distinct_ratio: 0.34,
            tool_loop_window_size: 40,
            exact_warn_threshold: 3,
            exact_stop_threshold: 5,
            kind_warn_threshold: 30,
            kind_stop_threshold: 40,
            mutating_repeat_warn_threshold: 2,
            read_only_repeat_warn_threshold: 4,
            same_name_varying_warn_threshold: 6,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_reproduce_legacy_call_site_values() {
        let cfg = LoopDetectorConfig::default();
        // Pre-P-3 hardcoded call site was MopDetector::new(8, 4).
        assert_eq!(cfg.mop_window_size, 8);
        assert_eq!(cfg.mop_repeat_threshold, 4);
        // Pre-P-3 ToolLoopDetector::default() values.
        assert_eq!(cfg.tool_loop_window_size, 40);
        assert_eq!(cfg.exact_warn_threshold, 3);
        assert_eq!(cfg.exact_stop_threshold, 5);
        assert_eq!(cfg.kind_warn_threshold, 30);
        assert_eq!(cfg.kind_stop_threshold, 40);
    }

    #[test]
    fn defaults_apply_lane_specified_idempotency_thresholds() {
        let cfg = LoopDetectorConfig::default();
        assert_eq!(cfg.mutating_repeat_warn_threshold, 2);
        assert_eq!(cfg.read_only_repeat_warn_threshold, 4);
        assert_eq!(cfg.same_name_varying_warn_threshold, 6);
    }
}

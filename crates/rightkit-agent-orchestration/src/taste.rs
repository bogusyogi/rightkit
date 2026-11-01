//! Evaluation metrics — `TasteScore` and reliability-decay analytics.
//!
//! Accuracy alone is misleading: a role with 80% task success but 0% run-to-run
//! consistency is not operationally reliable. From a set of per-run records
//! (one per execution of a role/model) we compute the full picture: GDS, the
//! Reliability Decay Curve/Slope across duration buckets, the Variance
//! Amplification Factor, the Meltdown-Onset-Point rate, context efficiency, and
//! the four reliability dimensions.

/// Duration bucket a run fell into (short → very-long).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurationBucket {
    Short,
    Medium,
    Long,
    VeryLong,
}

impl DurationBucket {
    pub fn index(self) -> usize {
        match self {
            DurationBucket::Short => 0,
            DurationBucket::Medium => 1,
            DurationBucket::Long => 2,
            DurationBucket::VeryLong => 3,
        }
    }

    /// Long-horizon buckets, where meltdown is a concern.
    pub fn is_long(self) -> bool {
        matches!(self, DurationBucket::Long | DurationBucket::VeryLong)
    }
}

/// One observed execution of a (role, model).
#[derive(Debug, Clone, Copy)]
pub struct RunRecord {
    pub bucket: DurationBucket,
    /// Graceful Degradation Score for the run (0–1).
    pub gds: f64,
    /// Input tokens consumed (for context efficiency).
    pub input_tokens: u32,
    /// The run hit a Meltdown Onset Point (behavioral collapse).
    pub mop_fired: bool,
    /// The run completed without escalating to a higher tier.
    pub completed_without_escalation: bool,
    /// Output quality (0–1), e.g. from jury confidence.
    pub quality: f64,
}

/// The full per-role taste profile.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TasteScore {
    pub gds: f64,
    pub long_horizon_reliability: f64,
    pub reliability_decay_slope: f64,
    pub quality: f64,
    pub reliability: f64,
    pub consistency: f64,
    pub robustness: f64,
    pub predictability: f64,
    pub safety: f64,
    pub context_efficiency: f64,
    pub vaf: f64,
    pub mop_rate: f64,
}

fn mean(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        0.0
    } else {
        xs.iter().sum::<f64>() / xs.len() as f64
    }
}

fn variance(xs: &[f64]) -> f64 {
    if xs.len() < 2 {
        return 0.0;
    }
    let m = mean(xs);
    xs.iter().map(|x| (x - m).powi(2)).sum::<f64>() / xs.len() as f64
}

/// Mean GDS per duration bucket; `None` for buckets with no records.
pub fn reliability_decay_curve(records: &[RunRecord]) -> [Option<f64>; 4] {
    let mut sums = [0.0_f64; 4];
    let mut counts = [0_u32; 4];
    for r in records {
        let i = r.bucket.index();
        sums[i] += r.gds;
        counts[i] += 1;
    }
    let mut out = [None; 4];
    for i in 0..4 {
        if counts[i] > 0 {
            out[i] = Some(sums[i] / counts[i] as f64);
        }
    }
    out
}

/// Reliability Decay Slope: least-squares slope of GDS over the populated
/// buckets (x = bucket index 0..3). More negative = more fragile at long
/// horizons.
pub fn reliability_decay_slope(rdc: &[Option<f64>; 4]) -> f64 {
    let pts: Vec<(f64, f64)> = rdc
        .iter()
        .enumerate()
        .filter_map(|(i, v)| v.map(|y| (i as f64, y)))
        .collect();
    if pts.len() < 2 {
        return 0.0;
    }
    let n = pts.len() as f64;
    let sx: f64 = pts.iter().map(|p| p.0).sum();
    let sy: f64 = pts.iter().map(|p| p.1).sum();
    let sxy: f64 = pts.iter().map(|p| p.0 * p.1).sum();
    let sxx: f64 = pts.iter().map(|p| p.0 * p.0).sum();
    let denom = n * sxx - sx * sx;
    if denom.abs() < 1e-12 {
        0.0
    } else {
        (n * sxy - sx * sy) / denom
    }
}

/// Variance Amplification Factor: variance(GDS at long horizon) / variance(GDS
/// at short horizon). > 1 means duration amplifies outcome variance.
pub fn variance_amplification_factor(records: &[RunRecord]) -> f64 {
    let short: Vec<f64> = records
        .iter()
        .filter(|r| r.bucket == DurationBucket::Short)
        .map(|r| r.gds)
        .collect();
    let long: Vec<f64> = records
        .iter()
        .filter(|r| r.bucket.is_long())
        .map(|r| r.gds)
        .collect();
    let vs = variance(&short);
    let vl = variance(&long);
    if vs < 1e-12 {
        if vl < 1e-12 {
            1.0
        } else {
            // Short variance ~0 but long varies → strong amplification.
            f64::INFINITY.min(1.0 + vl * 1000.0)
        }
    } else {
        vl / vs
    }
}

/// Fraction of long-horizon runs that hit a Meltdown Onset Point.
pub fn mop_rate(records: &[RunRecord]) -> f64 {
    let long: Vec<&RunRecord> = records.iter().filter(|r| r.bucket.is_long()).collect();
    if long.is_empty() {
        return 0.0;
    }
    long.iter().filter(|r| r.mop_fired).count() as f64 / long.len() as f64
}

/// Compute the full [`TasteScore`] from a role's run records.
pub fn compute_taste_score(records: &[RunRecord]) -> TasteScore {
    let gdss: Vec<f64> = records.iter().map(|r| r.gds).collect();
    let rdc = reliability_decay_curve(records);
    let rds = reliability_decay_slope(&rdc);
    let vaf = variance_amplification_factor(records);
    let mop = mop_rate(records);

    let gds = mean(&gdss);
    let quality = mean(&records.iter().map(|r| r.quality).collect::<Vec<_>>());
    let reliability = if records.is_empty() {
        0.0
    } else {
        records
            .iter()
            .filter(|r| r.completed_without_escalation)
            .count() as f64
            / records.len() as f64
    };
    // Consistency: 1 - standard deviation of GDS (low spread = consistent).
    let consistency = (1.0 - variance(&gdss).sqrt()).clamp(0.0, 1.0);
    let long_horizon_reliability = rdc[3].or(rdc[2]).unwrap_or(gds);
    // Reliability dimensions derived from available signals:
    let robustness = consistency; // stability of outcome across runs
    let predictability = (1.0 - variance(&gdss)).clamp(0.0, 1.0); // forecastability
    let safety = (1.0 - mop).clamp(0.0, 1.0); // bounded failure severity (no meltdown)
    let mean_tokens = mean(
        &records
            .iter()
            .map(|r| r.input_tokens as f64)
            .collect::<Vec<_>>(),
    );
    let context_efficiency = if mean_tokens < 1e-9 {
        0.0
    } else {
        // Goal-directed score achieved per unit of context spent, normalized
        // against a reference turn budget so this lands in 0..1 like the other
        // dimensions. The raw `gds / mean_tokens` ratio is ~1e-4 and always
        // rendered as 0.00. Spending fewer tokens for the same GDS scores
        // higher; the reference is a heuristic baseline open to tuning.
        const REFERENCE_CONTEXT_TOKENS: f64 = 6000.0;
        (gds * (REFERENCE_CONTEXT_TOKENS / mean_tokens)).clamp(0.0, 1.0)
    };

    TasteScore {
        gds,
        long_horizon_reliability,
        reliability_decay_slope: rds,
        quality,
        reliability,
        consistency,
        robustness,
        predictability,
        safety,
        context_efficiency,
        vaf,
        mop_rate: mop,
    }
}

impl TasteScore {
    /// A single 0–1 overall score with the doc's default weighting: heavy on
    /// GDS and long-horizon reliability, penalizing a steep decay slope.
    pub fn overall(&self) -> f64 {
        let decay_penalty = (-self.reliability_decay_slope).clamp(0.0, 1.0);
        let raw = 0.30 * self.gds
            + 0.20 * self.long_horizon_reliability
            + 0.15 * self.reliability
            + 0.10 * self.quality
            + 0.10 * self.consistency
            + 0.10 * self.safety
            - 0.05 * decay_penalty;
        raw.clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(bucket: DurationBucket, gds: f64, mop: bool) -> RunRecord {
        RunRecord {
            bucket,
            gds,
            input_tokens: 1000,
            mop_fired: mop,
            completed_without_escalation: gds >= 0.7,
            quality: gds,
        }
    }

    #[test]
    fn rdc_and_negative_slope_for_decaying_model() {
        let records = vec![
            rec(DurationBucket::Short, 1.0, false),
            rec(DurationBucket::Medium, 0.8, false),
            rec(DurationBucket::Long, 0.5, false),
            rec(DurationBucket::VeryLong, 0.2, false),
        ];
        let rdc = reliability_decay_curve(&records);
        assert_eq!(rdc[0], Some(1.0));
        assert_eq!(rdc[3], Some(0.2));
        let rds = reliability_decay_slope(&rdc);
        assert!(
            rds < -0.1,
            "decaying model should have steep negative RDS, got {rds}"
        );
    }

    #[test]
    fn flat_model_has_near_zero_slope() {
        let records = vec![
            rec(DurationBucket::Short, 0.9, false),
            rec(DurationBucket::Medium, 0.9, false),
            rec(DurationBucket::Long, 0.9, false),
            rec(DurationBucket::VeryLong, 0.9, false),
        ];
        let rds = reliability_decay_slope(&reliability_decay_curve(&records));
        assert!(rds.abs() < 1e-9, "flat model slope should be ~0, got {rds}");
    }

    #[test]
    fn mop_rate_counts_only_long_runs() {
        let records = vec![
            rec(DurationBucket::Short, 1.0, true), // short MOP ignored
            rec(DurationBucket::Long, 0.4, true),
            rec(DurationBucket::VeryLong, 0.4, false),
        ];
        assert!((mop_rate(&records) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn vaf_above_one_when_long_more_variable() {
        let records = vec![
            rec(DurationBucket::Short, 0.8, false),
            rec(DurationBucket::Short, 0.8, false),
            rec(DurationBucket::Long, 0.1, false),
            rec(DurationBucket::Long, 1.0, false),
        ];
        assert!(variance_amplification_factor(&records) > 1.0);
    }

    #[test]
    fn taste_score_rewards_consistent_high_gds() {
        let strong = vec![
            rec(DurationBucket::Short, 0.95, false),
            rec(DurationBucket::Long, 0.95, false),
        ];
        let weak = vec![
            rec(DurationBucket::Short, 0.4, true),
            rec(DurationBucket::Long, 0.4, true),
        ];
        let s = compute_taste_score(&strong);
        let w = compute_taste_score(&weak);
        assert!(s.overall() > w.overall());
        assert!(s.safety > w.safety, "no-meltdown model is safer");
        assert!(s.gds > w.gds);
    }

    #[test]
    fn empty_records_are_safe() {
        let ts = compute_taste_score(&[]);
        assert_eq!(ts.gds, 0.0);
        assert_eq!(ts.mop_rate, 0.0);
    }
}

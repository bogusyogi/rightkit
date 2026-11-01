//! Detection of success-rate regressions against a baseline.

/// The verdict of a regression check.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RegressionVerdict {
    /// No meaningful regression detected.
    Stable,
    /// A regression was detected.
    Regressed {
        /// How far the recent rate dropped below baseline.
        drop: f64,
    },
}

/// Threshold below which a drop is considered a regression.
const REGRESSION_THRESHOLD: f64 = 0.1;

/// Compares recent success rates against a fixed baseline.
#[derive(Debug, Clone, Copy)]
pub struct RegressionDetector {
    baseline_success_rate: f64,
}

impl RegressionDetector {
    /// Create a detector with the given baseline success rate.
    pub fn new(baseline_success_rate: f64) -> Self {
        Self {
            baseline_success_rate,
        }
    }

    /// Check a recent success rate against the baseline.
    ///
    /// Returns [`RegressionVerdict::Regressed`] when the recent rate is more than
    /// `0.1` below the baseline.
    pub fn check(&self, recent_success_rate: f64) -> RegressionVerdict {
        let drop = self.baseline_success_rate - recent_success_rate;
        if drop > REGRESSION_THRESHOLD {
            RegressionVerdict::Regressed { drop }
        } else {
            RegressionVerdict::Stable
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regression_stable_when_steady() {
        let d = RegressionDetector::new(0.9);
        assert_eq!(d.check(0.9), RegressionVerdict::Stable);
    }

    #[test]
    fn regression_detected_on_drop() {
        let d = RegressionDetector::new(0.9);
        assert!(matches!(d.check(0.5), RegressionVerdict::Regressed { .. }));
    }

    #[test]
    fn regression_drop_amount() {
        let d = RegressionDetector::new(0.9);
        match d.check(0.5) {
            RegressionVerdict::Regressed { drop } => assert!((drop - 0.4).abs() < 1e-9),
            other => panic!("expected regression, got {other:?}"),
        }
    }

    #[test]
    fn regression_small_dip_is_stable() {
        let d = RegressionDetector::new(0.9);
        // 0.05 dip is within the 0.1 threshold
        assert_eq!(d.check(0.85), RegressionVerdict::Stable);
    }
}

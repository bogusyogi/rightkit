//! Journey budgets (`[budget]` in `journeys.toml`) and the speed verdict over recorded timings.
//!
//! Pure logic: the only I/O is TOML parsing done by the caller. A timed unit (a step, a journey or
//! a suite) over its declared budget is [`Verdict::Slow`] while the warn window is open, and
//! [`Verdict::OverBudget`] after it. The window end is inclusive. After the window, a step kind is
//! enforced at [`ENFORCE_FACTOR`] times the p95 of its recorded durations on this host, never
//! below the [`MIN_ENFORCE_MS`] floor; a host with fewer than [`MIN_P95_SAMPLES`] recorded
//! durations for the kind is enforced at the declared budget.
use crate::util::{err, Result};
use serde::Deserialize;
use std::fmt;

/// Multiplier on this host's p95 step duration that becomes the enforcement threshold.
pub const ENFORCE_FACTOR: u64 = 2;

/// The floor under the p95-derived enforcement threshold, in milliseconds. Timer granularity and
/// scheduler noise on a loaded host make a limit of a few milliseconds flaky, so a derived limit is
/// never below this, even when every recorded duration is 0 ms. It is a floor, not a disable: a
/// step over it still fails after the window.
pub const MIN_ENFORCE_MS: u64 = 100;

/// Recorded durations a step kind needs before its p95 may enforce. Below this count the declared
/// budget governs.
pub const MIN_P95_SAMPLES: usize = 20;

/// A calendar day from a `YYYY-MM-DD` string. Ordered by date.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Day {
    year: u32,
    month: u32,
    day: u32,
}

fn leap(year: u32) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}

impl Day {
    /// Validate a calendar day (month 1-12, day within the month, leap years included).
    pub fn new(year: u32, month: u32, day: u32) -> Result<Day> {
        let max = match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 => {
                if leap(year) {
                    29
                } else {
                    28
                }
            }
            _ => return err(format!("month {month} is not 1-12")),
        };
        if day == 0 || day > max {
            return err(format!("{year:04}-{month:02}-{day:02} is not a day"));
        }
        Ok(Day { year, month, day })
    }

    /// Parse a strict `YYYY-MM-DD` date.
    pub fn parse(text: &str) -> Result<Day> {
        let shape = text.len() == 10
            && text.bytes().enumerate().all(|(i, c)| {
                if i == 4 || i == 7 {
                    c == b'-'
                } else {
                    c.is_ascii_digit()
                }
            });
        if !shape {
            return err(format!("{text:?} is not a YYYY-MM-DD date"));
        }
        // The shape check makes the text ASCII, so these byte ranges are char boundaries.
        let num = |a: usize, b: usize| text[a..b].parse::<u32>();
        match (num(0, 4), num(5, 7), num(8, 10)) {
            (Ok(y), Ok(m), Ok(d)) => Day::new(y, m, d),
            _ => err(format!("{text:?} is not a YYYY-MM-DD date")),
        }
    }
}

impl fmt::Display for Day {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

/// Declared budgets for one journey kind, in milliseconds. An absent field is not enforced.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KindBudget {
    #[serde(default)]
    pub step_ms: Option<u64>,
    #[serde(default)]
    pub journey_ms: Option<u64>,
    #[serde(default)]
    pub suite_ms: Option<u64>,
}

/// The `[budget]` table of `journeys.toml`. `warn_until` is the last day (inclusive) on which
/// exceeding a budget only warns. Kinds are the journey `tier` values (manifest.rs `TIERS`), so
/// `[budget.fast]`, `[budget.full]` and `[budget.paid]` are the only kind tables; any other key is
/// refused.
///
/// ```toml
/// [budget]
/// warn_until = "2026-11-06"
///
/// [budget.fast]
/// step_ms = 2000
/// journey_ms = 60000
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetDecl {
    pub warn_until: String,
    #[serde(default)]
    pub fast: Option<KindBudget>,
    #[serde(default)]
    pub full: Option<KindBudget>,
    #[serde(default)]
    pub paid: Option<KindBudget>,
}

impl BudgetDecl {
    /// The warn window end as a day.
    pub fn window_end(&self) -> Result<Day> {
        Day::parse(&self.warn_until)
    }

    /// The budgets declared for one journey kind (`fast`, `full` or `paid`).
    pub fn kind(&self, kind: &str) -> Option<&KindBudget> {
        match kind {
            "fast" => self.fast.as_ref(),
            "full" => self.full.as_ref(),
            "paid" => self.paid.as_ref(),
            _ => None,
        }
    }

    /// Static problems in the table, in the same style as the manifest checks: an unparseable
    /// window date and a zero budget.
    pub fn issues(&self) -> Vec<String> {
        let mut out = vec![];
        if let Err(e) = self.window_end() {
            out.push(format!("[budget] warn_until: {}", e.0));
        }
        for (name, kind) in [
            ("fast", &self.fast),
            ("full", &self.full),
            ("paid", &self.paid),
        ] {
            if let Some(kb) = kind {
                for (field, ms) in [
                    ("step_ms", kb.step_ms),
                    ("journey_ms", kb.journey_ms),
                    ("suite_ms", kb.suite_ms),
                ] {
                    if ms == Some(0) {
                        out.push(format!("[budget.{name}] {field} must be above 0"));
                    }
                }
            }
        }
        out
    }
}

/// The 95th percentile of recorded durations by the nearest-rank method: the smallest value with
/// at least 95% of the durations at or below it. `None` for an empty list.
pub fn p95(durations_ms: &[u64]) -> Option<u64> {
    if durations_ms.is_empty() {
        return None;
    }
    let mut sorted = durations_ms.to_vec();
    sorted.sort_unstable();
    let rank = (sorted.len() * 95).div_ceil(100);
    Some(sorted[rank - 1])
}

/// The two thresholds that judge one timed unit on one host, in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The declared budget: exceeding it warns while the window is open.
    pub warn_ms: u64,
    /// The enforcement threshold: exceeding it fails once the window has closed.
    pub enforce_ms: u64,
}

impl Limits {
    /// A unit with no host baseline (a journey or suite): the declared budget is both thresholds.
    pub fn declared(ms: u64) -> Limits {
        Limits {
            warn_ms: ms,
            enforce_ms: ms,
        }
    }

    /// A step kind on this host: enforced at [`ENFORCE_FACTOR`] times the p95 of this host's
    /// recorded durations for the kind, and never below [`MIN_ENFORCE_MS`]. With fewer than
    /// [`MIN_P95_SAMPLES`] recorded durations, enforced at the declared budget, so one fast run
    /// cannot set an impossible limit. The declared budget is not floored.
    pub fn for_step(declared_ms: u64, host_durations_ms: &[u64]) -> Limits {
        let enforce_ms = match p95(host_durations_ms) {
            Some(p) if host_durations_ms.len() >= MIN_P95_SAMPLES => {
                p.saturating_mul(ENFORCE_FACTOR).max(MIN_ENFORCE_MS)
            }
            _ => declared_ms,
        };
        Limits {
            warn_ms: declared_ms,
            enforce_ms,
        }
    }
}

/// One measured unit. `label` is the step label for a step, or the journey or suite id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing<'a> {
    pub label: &'a str,
    pub ms: u64,
}

/// The result of judging one timing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Ok,
    /// Over the declared budget inside the warn window: a warning; the run still passes.
    /// `budget` is the declared budget.
    Slow {
        step: String,
        ms: u64,
        budget: u64,
    },
    /// Over the enforcement threshold after the window: a failure. `budget` is the enforcement
    /// threshold.
    OverBudget {
        step: String,
        ms: u64,
        budget: u64,
    },
}

impl Verdict {
    pub fn is_failure(&self) -> bool {
        matches!(self, Verdict::OverBudget { .. })
    }
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Verdict::Ok => write!(f, "ok"),
            Verdict::Slow { step, ms, budget } => {
                write!(f, "SLOW {step}: {ms} ms, budget {budget} ms (warning)")
            }
            Verdict::OverBudget { step, ms, budget } => {
                write!(f, "OVER BUDGET {step}: {ms} ms, enforced at {budget} ms")
            }
        }
    }
}

/// Judge one timing. The window is open while `now <= window_end`.
///
/// Inside the window only the declared budget matters: over it is [`Verdict::Slow`]. After the
/// window only the enforcement threshold matters: over it is [`Verdict::OverBudget`].
pub fn verdict(limits: Limits, timing: Timing<'_>, now: Day, window_end: Day) -> Verdict {
    if now <= window_end {
        if timing.ms > limits.warn_ms {
            return Verdict::Slow {
                step: timing.label.to_string(),
                ms: timing.ms,
                budget: limits.warn_ms,
            };
        }
        return Verdict::Ok;
    }
    if timing.ms > limits.enforce_ms {
        return Verdict::OverBudget {
            step: timing.label.to_string(),
            ms: timing.ms,
            budget: limits.enforce_ms,
        };
    }
    Verdict::Ok
}

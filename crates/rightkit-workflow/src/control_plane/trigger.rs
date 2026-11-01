//! WP-06 — Triggers, schedules, and internal events.
//!
//! This module is part of the workflow control plane and is **pure**: it has no
//! I/O, no time-zone database, and no async runtime. The trigger types and
//! the cron helper are stable inputs to the supervisor that lives in
//! `engine/host-app/src/workflow` (WP-07+).
//!
//! ## Scope of V1
//!
//! - Trigger kinds cover manual, one-time, cron, webhook, GitHub, and
//!   internal-event sources. Admission payloads remain opaque typed data at
//!   this pure boundary; durable validation belongs to supervisor storage.
//! - `next_occurrence` is a self-contained 5-field cron iterator. It does not
//!   pull in the `cron` crate and does not consult the OS timezone database.
//!   Time-zone handling is intentionally opaque for V1; the supervisor will
//!   resolve the IANA identifier at admission time once the WP-07 timezone
//!   library is selected.
//! - Non-UTC cron schedules are conservatively rejected here. This prevents
//!   ambiguous/absent wall-clock times until a timezone resolver is supplied.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{debug, warn};

use crate::control_plane::clock::UnixMs;

// ─── Trigger kinds ───────────────────────────────────────────────────────────

/// The supported V1 trigger kinds.
///
/// Each variant is wired to exactly one occurrence generator. The supervisor
/// (WP-07) chooses the generator at admission time; this crate only types
/// the trigger so callers can pattern-match exhaustively.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum TriggerKind {
    /// User-initiated run from chat, CLI, or dashboard.
    Manual,
    /// One-shot trigger at a fixed wall-clock instant.
    OneTime,
    /// Recurring schedule driven by a 5-field cron expression.
    Cron,
    /// External webhook ingestion.
    Webhook,
    /// GitHub-native event source (`push`, `pull_request`, …). Payload
    /// payload is admitted by the supervisor.
    GitHub,
    /// Internal event published by another node / run. Payload shape is
    /// payload is admitted by the supervisor.
    InternalEvent,
}

/// One trigger attached to a workflow definition.
///
/// The trigger is identified by `id`; the supervisor uses this id when
/// admitting occurrences into the journal. Cron / one-time triggers carry the
/// timing parameters here so a `Trigger` is a self-contained record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trigger {
    /// Stable trigger id (`wft_*`).
    pub id: String,
    /// Trigger kind. Drives the supervisor's occurrence generator.
    pub kind: TriggerKind,
    /// Cron expression for [`TriggerKind::Cron`]. Required when `kind ==
    /// Cron`; ignored otherwise.
    pub cron_expression: Option<String>,
    /// IANA timezone identifier (e.g. `America/Los_Angeles`). Treated as an
    /// opaque non-empty string for V1; resolved by the supervisor in WP-07.
    pub timezone: Option<String>,
    /// Optional explicit dedupe key. When `None`, the supervisor derives one
    /// from `(trigger_id, cron_fire_time, version_id)`.
    pub dedupe_key: Option<String>,
    /// ENG-95 invariant: `true` when this trigger admits runs from a
    /// source nobody at the keyboard started (webhook, GitHub, internal
    /// event). The permission path (`engine/crates/policy`) must treat an
    /// unattended admission's session as if `PermissionMode::Ask` were
    /// configured, **regardless of the session's configured mode** — a
    /// turn nobody started never gets auto-approve. `Trigger::webhook`
    /// sets this unconditionally; every other constructor defaults it to
    /// `false` (serde default keeps older-format triggers unattended
    /// only when the caller passes this field explicitly).
    #[serde(default)]
    pub unattended: bool,
}

impl Trigger {
    /// Construct a webhook-originated trigger. `unattended` is always
    /// `true` here by construction — there is no code path in this crate
    /// that can produce a webhook trigger with `unattended: false`. The
    /// admission layer (WP-07 supervisor / `remote::webhook_ingress`)
    /// must route every occurrence through this constructor rather than
    /// building `Trigger { kind: TriggerKind::Webhook, .. }` by hand, or
    /// the invariant below has no enforcement point.
    pub fn webhook(id: String, dedupe_key: Option<String>) -> Self {
        Self {
            id,
            kind: TriggerKind::Webhook,
            cron_expression: None,
            timezone: None,
            dedupe_key,
            unattended: true,
        }
    }

    /// `true` when this trigger's admitted runs must be forced to
    /// `PermissionMode::Ask` at session start regardless of the
    /// session's configured mode. Checks both the explicit flag and the
    /// trigger kind, so a caller that hand-builds a `Trigger` with
    /// `kind: Webhook` but forgets to set `unattended` is still caught —
    /// the invariant is "no webhook-originated turn ever auto-approves,"
    /// not "no turn with the flag set."
    pub fn requires_forced_ask(&self) -> bool {
        self.unattended || matches!(self.kind, TriggerKind::Webhook)
    }
}

/// Per-trigger occurrence policy. Controls how the supervisor turns fired
/// triggers into run admissions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OccurrencePolicy {
    /// Optional explicit version pin. When `Some`, occurrences always admit
    /// against this workflow version. When `None`, the trigger inherits the
    /// workflow's current published version.
    pub version_pin: Option<String>,
    /// Strategy for collapsing duplicate occurrences within a window.
    pub dedupe_strategy: DedupeStrategy,
    /// Strategy for handling missed fires (e.g. daemon downtime).
    pub misfire_policy: MisfirePolicy,
    /// Strategy for handling a trigger that fires while a previous run is
    /// still in flight.
    pub overlap_policy: OverlapPolicy,
    /// Hard cap on concurrent runs admitted from this trigger. Enforced by
    /// the supervisor.
    pub max_concurrency: u32,
}

/// Causal receipt chain for scheduled execution. Each receipt carries the
/// upstream id so replay can prove trigger → run → child task → result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum CausalReceiptKind {
    Trigger {
        trigger_id: String,
    },
    Run {
        run_id: String,
        trigger_receipt_id: String,
    },
    ChildTask {
        task_id: String,
        run_receipt_id: String,
    },
    Result {
        result_id: String,
        task_receipt_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CausalReceipt {
    pub id: String,
    pub sequence: u32,
    pub kind: CausalReceiptKind,
    pub created_at_unix_ms: i64,
}

impl CausalReceipt {
    pub fn validate_chain(chain: &[Self]) -> Result<(), String> {
        if chain.is_empty() {
            return Err("causal receipt chain is empty".into());
        }
        for (index, receipt) in chain.iter().enumerate() {
            if receipt.sequence != index as u32 {
                return Err("causal receipt sequence gap".into());
            }
            if receipt.id.trim().is_empty() {
                return Err("causal receipt id is empty".into());
            }
            if index == 0 && !matches!(receipt.kind, CausalReceiptKind::Trigger { .. }) {
                return Err("causal chain must start with trigger".into());
            }
            if let Some(previous) = chain.get(index.wrapping_sub(1)) {
                let linked = match &receipt.kind {
                    CausalReceiptKind::Run {
                        trigger_receipt_id, ..
                    } => trigger_receipt_id == &previous.id,
                    CausalReceiptKind::ChildTask { run_receipt_id, .. } => {
                        run_receipt_id == &previous.id
                    }
                    CausalReceiptKind::Result {
                        task_receipt_id, ..
                    } => task_receipt_id == &previous.id,
                    CausalReceiptKind::Trigger { .. } => false,
                };
                if index > 0 && !linked {
                    return Err("causal receipt parent mismatch".into());
                }
            }
        }
        Ok(())
    }
}

impl Default for OccurrencePolicy {
    fn default() -> Self {
        Self {
            version_pin: None,
            dedupe_strategy: DedupeStrategy::KeyHash,
            misfire_policy: MisfirePolicy::Skip,
            overlap_policy: OverlapPolicy::Forbid,
            max_concurrency: 1,
        }
    }
}

/// How the supervisor collapses duplicate occurrences inside a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DedupeStrategy {
    /// No dedupe — every occurrence admits a new run.
    None,
    /// Hash the `(trigger_id, occurrence_time, version_id)` triple.
    KeyHash,
    /// Honor an explicit idempotency key supplied at admission.
    IdempotencyKey,
}

/// What the supervisor does when it observes a missed fire (e.g. daemon
/// downtime that straddled a scheduled fire time).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MisfirePolicy {
    /// Drop missed fires silently.
    Skip,
    /// Coalesce all missed fires into one occurrence at daemon restart.
    Coalesce,
    /// Catch up to N missed fires, then stop. (Bound enforced elsewhere.)
    BoundedCatchUp,
}

/// What the supervisor does when a trigger fires while a previous run is
/// still in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlapPolicy {
    /// Refuse the new fire; record an `attention` projection.
    Forbid,
    /// Allow concurrent runs (subject to `max_concurrency`).
    Allow,
    /// Queue the new fire until the previous run completes.
    Queue,
}

/// Runtime-normalized IANA local-time result. Timezone database access stays
/// outside this pure crate; admission must supply its resolved classification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedOccurrence {
    pub timezone: String,
    pub local_key: String,
    pub resolution: LocalTimeResolution,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LocalTimeResolution {
    Unique {
        unix_ms: UnixMs,
    },
    Gap,
    Overlap {
        earlier_unix_ms: UnixMs,
        later_unix_ms: UnixMs,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DstResolutionPolicy {
    SkipGap,
    Earlier,
    Later,
    RejectOverlap,
}

/// Deterministically converts runtime IANA resolution into a durable fire.
/// Gaps are never silently shifted; overlaps require a published choice.
pub fn resolve_normalized_occurrence(
    value: &NormalizedOccurrence,
    policy: DstResolutionPolicy,
) -> Result<Option<UnixMs>, TriggerError> {
    if value.timezone.trim().is_empty() || value.local_key.trim().is_empty() {
        return Err(TriggerError::InvalidTimezone);
    }
    Ok(match value.resolution {
        LocalTimeResolution::Unique { unix_ms } => Some(unix_ms),
        LocalTimeResolution::Gap => match policy {
            DstResolutionPolicy::SkipGap => None,
            _ => {
                return Err(TriggerError::InvalidField {
                    field: "dst".into(),
                    reason: "gap requires SkipGap".into(),
                });
            }
        },
        LocalTimeResolution::Overlap {
            earlier_unix_ms,
            later_unix_ms,
        } => match policy {
            DstResolutionPolicy::Earlier => Some(earlier_unix_ms),
            DstResolutionPolicy::Later => Some(later_unix_ms),
            _ => {
                return Err(TriggerError::InvalidField {
                    field: "dst".into(),
                    reason: "overlap requires Earlier or Later".into(),
                });
            }
        },
    })
}

// ─── Errors ──────────────────────────────────────────────────────────────────

/// Typed failure from the trigger / cron helpers.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TriggerError {
    /// Cron expression has the wrong number of fields (must be exactly 5).
    #[error("cron expression must have 5 fields, got {0}")]
    InvalidFieldCount(usize),
    /// A cron field could not be parsed.
    #[error("invalid cron field `{field}`: {reason}")]
    InvalidField { field: String, reason: String },
    /// A cron value is outside its legal range.
    #[error("cron value {value} out of range for `{field}` (expected {min}..={max})")]
    OutOfRange {
        field: &'static str,
        value: u32,
        min: u32,
        max: u32,
    },
    /// Timezone identifier is empty.
    #[error("timezone identifier must be non-empty")]
    InvalidTimezone,
    /// No cron match found within the search horizon (default: one year).
    #[error("no cron match found within {0} ms of `after_unix_ms`")]
    NoMatchWithinHorizon(i64),
}

// ─── Cron parser (self-contained, 5-field) ───────────────────────────────────

/// Internal representation of a parsed 5-field cron expression.
#[derive(Debug, Clone)]
struct CronExpr {
    minute: FieldSpec,
    hour: FieldSpec,
    day_of_month: FieldSpec,
    month: FieldSpec,
    day_of_week: FieldSpec,
}

/// One parsed cron field. Stored as a fixed-size bit set keyed by `(value -
/// min)` so membership tests are O(1).
#[derive(Debug, Clone)]
struct FieldSpec {
    bits: Vec<bool>,
    min: u32,
    max: u32,
}

impl FieldSpec {
    fn parse(field: &str, min: u32, max: u32) -> Result<Self, TriggerError> {
        let raw = field.trim();
        if raw.is_empty() {
            return Err(TriggerError::InvalidField {
                field: field.to_string(),
                reason: "empty field".to_string(),
            });
        }
        let mut bits = vec![false; (max - min + 1) as usize];
        for part in raw.split(',') {
            apply_part(part.trim(), min, max, &mut bits)?;
        }
        Ok(Self { bits, min, max })
    }

    fn contains(&self, v: u32) -> bool {
        if v < self.min || v > self.max {
            return false;
        }
        self.bits[(v - self.min) as usize]
    }

    fn is_wildcard(&self) -> bool {
        self.bits.iter().all(|b| *b)
    }
}

fn apply_part(part: &str, min: u32, max: u32, bits: &mut [bool]) -> Result<(), TriggerError> {
    // `<step>` after a range or wildcard: `*/N`, `a-b/N`, or `N`.
    let (range_part, step) = match part.split_once('/') {
        Some((r, s)) => {
            let step: u32 = s.parse().map_err(|_| TriggerError::InvalidField {
                field: part.to_string(),
                reason: format!("step `{}` is not a positive integer", s),
            })?;
            if step == 0 {
                return Err(TriggerError::InvalidField {
                    field: part.to_string(),
                    reason: "step must be >= 1".to_string(),
                });
            }
            (r, step)
        }
        None => (part, 1u32),
    };

    let (lo, hi) = if range_part == "*" {
        (min, max)
    } else if let Some((a, b)) = range_part.split_once('-') {
        let lo: u32 = a.parse().map_err(|_| TriggerError::InvalidField {
            field: part.to_string(),
            reason: format!("range start `{}` is not an integer", a),
        })?;
        let hi: u32 = b.parse().map_err(|_| TriggerError::InvalidField {
            field: part.to_string(),
            reason: format!("range end `{}` is not an integer", b),
        })?;
        if lo < min || lo > max {
            return Err(TriggerError::OutOfRange {
                field: "range",
                value: lo,
                min,
                max,
            });
        }
        if hi < min || hi > max {
            return Err(TriggerError::OutOfRange {
                field: "range",
                value: hi,
                min,
                max,
            });
        }
        if hi < lo {
            return Err(TriggerError::InvalidField {
                field: part.to_string(),
                reason: format!("range end {} < start {}", hi, lo),
            });
        }
        (lo, hi)
    } else {
        let v: u32 = range_part.parse().map_err(|_| TriggerError::InvalidField {
            field: part.to_string(),
            reason: format!("`{}` is not an integer", range_part),
        })?;
        if v < min || v > max {
            return Err(TriggerError::OutOfRange {
                field: "value",
                value: v,
                min,
                max,
            });
        }
        (v, v)
    };

    let mut v = lo;
    // For wildcards (`*`) with a step, align the start to the smallest
    // multiple of `step` that is >= `min`. This gives `*/N` semantics
    // where the first fired value is the smallest step-aligned value
    // within the field (e.g. `*/30` for day-of-month (1-31) yields 30).
    if range_part == "*" && step > 1 {
        v = min.div_ceil(step) * step;
    }
    while v <= hi {
        bits[(v - min) as usize] = true;
        // Saturating add — overflow can't actually happen because hi <= max.
        v = v.saturating_add(step);
    }
    Ok(())
}

/// Parse a 5-field cron expression. Returns the structured representation
/// consumed by [`next_occurrence`].
fn parse_cron(cron: &str) -> Result<CronExpr, TriggerError> {
    let fields: Vec<&str> = cron.split_whitespace().collect();
    if fields.len() != 5 {
        return Err(TriggerError::InvalidFieldCount(fields.len()));
    }
    let minute = FieldSpec::parse(fields[0], 0, 59)?;
    let hour = FieldSpec::parse(fields[1], 0, 23)?;
    let day_of_month = FieldSpec::parse(fields[2], 1, 31)?;
    let month = FieldSpec::parse(fields[3], 1, 12)?;
    // Day-of-week: 0-7 where 7 == 0 (Sunday). We normalize 7 → 0 so the
    // FieldSpec only stores 0..=6.
    let mut dow_field = fields[4].to_string();
    if dow_field.contains('7') {
        dow_field = dow_field.replace('7', "0");
    }
    let day_of_week = FieldSpec::parse(&dow_field, 0, 6)?;
    Ok(CronExpr {
        minute,
        hour,
        day_of_month,
        month,
        day_of_week,
    })
}

// ─── Public helpers ──────────────────────────────────────────────────────────

/// One-year search horizon in milliseconds (366 days). Matches the spec's
/// "simple per-minute iteration up to ~1 year out" guidance.
const ONE_YEAR_MS: i64 = 366 * 24 * 60 * 60 * 1000;

/// Compute the next unix-ms instant at which `cron` fires, strictly after
/// `after_unix_ms`. The returned instant is always aligned to a minute
/// boundary (sub-minute cron expressions are not supported in V1).
///
/// `tz` is accepted as an opaque non-empty identifier. The supervisor
/// (WP-07) will resolve it against an IANA database at admission time.
/// For V1 the cron iterator runs in UTC; this is documented and accepted
/// because the WP-06 plan defers real timezone math to a later WP.
///
/// # Errors
///
/// - [`TriggerError::InvalidTimezone`] when `tz` is empty.
/// - [`TriggerError::InvalidFieldCount`] / [`TriggerError::InvalidField`] /
///   [`TriggerError::OutOfRange`] when `cron` is malformed.
/// - [`TriggerError::NoMatchWithinHorizon`] when no fire instant exists in
///   the next ~year (e.g. `cron = "0 0 30 2 *"` next to a non-leap window).
pub fn next_occurrence(cron: &str, tz: &str, after_unix_ms: i64) -> Result<UnixMs, TriggerError> {
    if tz.is_empty() {
        return Err(TriggerError::InvalidTimezone);
    }
    let expr = parse_cron(cron)?;

    // Iterate minute-by-minute from the next minute boundary. Sub-minute
    // crons are out of V1 scope.
    let mut current = next_minute_boundary(after_unix_ms);
    let deadline = after_unix_ms.saturating_add(ONE_YEAR_MS);
    let mut iterations: u64 = 0;

    while current <= deadline {
        iterations = iterations.saturating_add(1);
        let (year, month, day, hour, minute, _sec) = unix_ms_to_civil(current);
        let dow = rightkit_time::weekday(i64::from(year), month, day);

        if expr.month.contains(month) && expr.hour.contains(hour) && expr.minute.contains(minute) {
            // Day-of-month and day-of-week interaction (Vixie-style):
            //   * If BOTH day fields are restricted (not wildcard), match if
            //     EITHER contains the candidate.
            //   * If ONLY ONE is restricted, the restricted one must match.
            //   * If NEITHER is restricted, every day matches.
            let dom_restricted = !expr.day_of_month.is_wildcard();
            let dow_restricted = !expr.day_of_week.is_wildcard();
            let day_ok = match (dom_restricted, dow_restricted) {
                (true, true) => expr.day_of_month.contains(day) || expr.day_of_week.contains(dow),
                (true, false) => expr.day_of_month.contains(day),
                (false, true) => expr.day_of_week.contains(dow),
                (false, false) => true,
            };
            if day_ok {
                debug!(
                    cron = cron,
                    tz = tz,
                    after_ms = after_unix_ms,
                    candidate_ms = current,
                    iterations = iterations,
                    "cron match found"
                );
                return Ok(current);
            }
        }

        current = current.saturating_add(60_000);
    }

    warn!(
        cron = cron,
        tz = tz,
        after_ms = after_unix_ms,
        "no cron match within one-year horizon"
    );
    Err(TriggerError::NoMatchWithinHorizon(ONE_YEAR_MS))
}

/// Conservative deterministic DST policy. UTC has no DST transitions, so a
/// parseable UTC cron is safe. Every other zone is refused until an explicit
/// normalized-occurrence resolver is provided by the supervisor.
pub fn is_dst_safe(cron: &str, tz: &str) -> bool {
    if !matches!(tz, "UTC" | "Etc/UTC" | "Z") {
        return false;
    }
    parse_cron(cron).is_ok()
}

// ─── Civil time (UTC, rightkit-time) ───────────────────────────────────────────────────────────────────────

/// Round `ms` up to the next minute boundary. Uses `div_euclid` so the math
/// is correct for negative inputs too.
fn next_minute_boundary(ms: i64) -> i64 {
    let minute = ms.div_euclid(60_000);
    (minute + 1).saturating_mul(60_000)
}

/// Convert unix milliseconds (UTC) to a `(year, month, day, hour, minute,
/// second)` tuple. Time-zone conversion is the supervisor's responsibility.
fn unix_ms_to_civil(ms: i64) -> (i32, u32, u32, u32, u32, u32) {
    // Offset 0 is always valid, so `from_unix` cannot return `None`.
    let dt = rightkit_time::DateTime::from_unix(ms.div_euclid(1000), 0)
        .expect("offset 0 is a valid UTC offset");
    (
        dt.year as i32,
        dt.month,
        dt.day,
        dt.hour,
        dt.minute,
        dt.second,
    )
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // --- Parser unit tests ---------------------------------------------------

    #[test]
    fn field_spec_star_matches_everything() {
        let f = FieldSpec::parse("*", 0, 59).unwrap();
        for v in 0..=59 {
            assert!(f.contains(v), "star must match {}", v);
        }
        assert!(f.is_wildcard());
    }

    #[test]
    fn field_spec_step_every_n() {
        let f = FieldSpec::parse("*/15", 0, 59).unwrap();
        assert!(f.contains(0));
        assert!(f.contains(15));
        assert!(f.contains(30));
        assert!(f.contains(45));
        assert!(!f.contains(7));
        assert!(!f.contains(60));
    }

    #[test]
    fn field_spec_range() {
        let f = FieldSpec::parse("9-17", 0, 23).unwrap();
        assert!(!f.contains(8));
        assert!(f.contains(9));
        assert!(f.contains(17));
        assert!(!f.contains(18));
    }

    #[test]
    fn field_spec_list() {
        let f = FieldSpec::parse("0,15,30,45", 0, 59).unwrap();
        assert!(f.contains(0));
        assert!(f.contains(15));
        assert!(!f.contains(7));
        assert!(f.contains(45));
    }

    #[test]
    fn field_spec_combo() {
        let f = FieldSpec::parse("1-5,10,*/30", 1, 31).unwrap();
        assert!(f.contains(1));
        assert!(f.contains(5));
        assert!(f.contains(10));
        assert!(f.contains(30));
        assert!(!f.contains(6));
        assert!(!f.contains(15));
    }

    #[test]
    fn field_spec_rejects_out_of_range() {
        assert!(FieldSpec::parse("60", 0, 59).is_err());
        assert!(FieldSpec::parse("24", 0, 23).is_err());
        assert!(FieldSpec::parse("13", 1, 12).is_err());
    }

    #[test]
    fn field_spec_rejects_zero_step() {
        assert!(FieldSpec::parse("*/0", 0, 59).is_err());
    }

    #[test]
    fn field_spec_rejects_non_integer() {
        assert!(FieldSpec::parse("foo", 0, 59).is_err());
    }

    // --- next_occurrence tests ------------------------------------------------

    /// 2026-08-07T00:00:00Z (a Friday). Picked as a stable reference point.
    const REF_MS: i64 = 1_786_060_800_000;

    #[test]
    fn next_every_minute() {
        let n = next_occurrence("* * * * *", "UTC", REF_MS).unwrap();
        assert_eq!(n, REF_MS + 60_000);
    }

    #[test]
    fn next_top_of_hour() {
        // After 2026-08-07T00:00:00Z the next 0-minute-of-any-hour fire is
        // 2026-08-07T01:00:00Z.
        let n = next_occurrence("0 * * * *", "UTC", REF_MS).unwrap();
        assert_eq!(n, REF_MS + 60 * 60_000);
    }

    #[test]
    fn next_specific_minute_and_hour() {
        // "30 9 * * *" → next 09:30 UTC after REF_MS. The reference time is
        // 00:00 UTC, so the next occurrence is the same day at 09:30.
        let n = next_occurrence("30 9 * * *", "UTC", REF_MS).unwrap();
        let expected = REF_MS + (9 * 60 + 30) * 60_000;
        assert_eq!(n, expected);
    }

    #[test]
    fn next_specific_day_of_month() {
        // "0 0 8 * *" → next midnight on the 8th of any month.
        let n = next_occurrence("0 0 8 * *", "UTC", REF_MS).unwrap();
        let expected = REF_MS + 24 * 60 * 60_000; // 2026-08-08T00:00:00Z
        assert_eq!(n, expected);
    }

    #[test]
    fn next_specific_day_of_week() {
        // 2026-08-07 is a Friday; the next Saturday is 2026-08-08. "0 0 * * 6"
        // is midnight on Saturday.
        let n = next_occurrence("0 0 * * 6", "UTC", REF_MS).unwrap();
        let expected = REF_MS + 24 * 60 * 60_000;
        assert_eq!(n, expected);
    }

    #[test]
    fn next_sunday_in_iso_week() {
        // 2026-08-07 (Fri) → next Sunday (dow 0) is 2026-08-09.
        let n = next_occurrence("0 0 * * 0", "UTC", REF_MS).unwrap();
        let expected = REF_MS + 2 * 24 * 60 * 60_000;
        assert_eq!(n, expected);
    }

    #[test]
    fn next_step_every_15_minutes() {
        // From 00:00:00 UTC, next "*/15" is 00:15:00.
        let n = next_occurrence("*/15 * * * *", "UTC", REF_MS).unwrap();
        assert_eq!(n, REF_MS + 15 * 60_000);
    }

    #[test]
    fn next_uses_or_semantics_for_dom_and_dow() {
        // "0 0 1 * 1" — both dom=1 and dow=1 (Mon) are restricted; matches
        // EITHER via Vixie-style OR semantics. From REF_MS (Fri 2026-08-07)
        // the next match is the following Monday 2026-08-10, satisfying the
        // dow branch before the dom=1 branch on 2026-09-01.
        let n = next_occurrence("0 0 1 * 1", "UTC", REF_MS).unwrap();
        // 2026-08-10T00:00:00Z = +3 days from 08-07.
        let expected = REF_MS + 3 * 24 * 60 * 60_000;
        assert_eq!(n, expected);
    }

    #[test]
    fn next_dow_7_normalized_to_sunday() {
        // "0 0 * * 7" is the same as "0 0 * * 0".
        let a = next_occurrence("0 0 * * 0", "UTC", REF_MS).unwrap();
        let b = next_occurrence("0 0 * * 7", "UTC", REF_MS).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn next_feb_29_only_in_leap_year() {
        // "0 0 29 2 *" — Feb 29 only fires in leap years; the iterator
        // must skip non-leap Feb 28 (e.g. 2027). Start from 2027-03-01 so
        // the next Feb 29 (2028-02-29) is ~365 days out, comfortably
        // within the ~1-year search horizon.
        let start = unix_ms_from_civil(2027, 3, 1, 0, 0, 0);
        let n = next_occurrence("0 0 29 2 *", "UTC", start).unwrap();
        let (y, m, d, h, mi, _s) = unix_ms_to_civil(n);
        assert_eq!((y, m, d, h, mi), (2028, 2, 29, 0, 0));
    }

    #[test]
    fn next_non_leap_window_fails() {
        // "0 0 30 2 *" — Feb 30 never exists. We start within a year of
        // non-leap Feb 30, so the iterator should time out. Use a known
        // non-leap year start (2027-01-01).
        let start = unix_ms_from_civil(2027, 1, 1, 0, 0, 0);
        let err = next_occurrence("0 0 30 2 *", "UTC", start).unwrap_err();
        assert!(matches!(err, TriggerError::NoMatchWithinHorizon(_)));
    }

    #[test]
    fn next_rejects_empty_timezone() {
        let err = next_occurrence("* * * * *", "", REF_MS).unwrap_err();
        assert!(matches!(err, TriggerError::InvalidTimezone));
    }

    #[test]
    fn next_rejects_bad_field_count() {
        let err = next_occurrence("* * * *", "UTC", REF_MS).unwrap_err();
        assert!(matches!(err, TriggerError::InvalidFieldCount(4)));
    }

    #[test]
    fn next_rejects_bad_value() {
        let err = next_occurrence("60 * * * *", "UTC", REF_MS).unwrap_err();
        assert!(matches!(err, TriggerError::OutOfRange { .. }));
    }

    // --- is_dst_safe ----------------------------------------------------------

    #[test]
    fn is_dst_safe_true_only_for_utc_cron() {
        assert!(!is_dst_safe("0 9 * * *", "America/Los_Angeles"));
        assert!(is_dst_safe("*/5 * * * *", "UTC"));
    }

    #[test]
    fn is_dst_safe_false_for_invalid_inputs() {
        assert!(!is_dst_safe("not a cron", "UTC"));
        assert!(!is_dst_safe("* * * *", ""));
        assert!(!is_dst_safe("* * * * *", ""));
    }

    // --- civil helpers -------------------------------------------------------

    #[test]
    fn unix_ms_to_civil_known_dates() {
        // 1970-01-01T00:00:00Z is epoch day 0.
        assert_eq!(unix_ms_to_civil(0), (1970, 1, 1, 0, 0, 0));
        // 2000-01-01 is day 10957.
        assert_eq!(unix_ms_to_civil(10_957 * 86_400_000), (2000, 1, 1, 0, 0, 0));
        // 2024-02-29 is day 19782; the time of day and sub-second floor carry through.
        assert_eq!(
            unix_ms_to_civil(19_782 * 86_400_000 + 3_723_999),
            (2024, 2, 29, 1, 2, 3)
        );
        // Negative inputs floor toward the earlier second.
        assert_eq!(unix_ms_to_civil(-1), (1969, 12, 31, 23, 59, 59));
    }

    #[test]
    fn next_minute_boundary_handles_negatives() {
        assert_eq!(next_minute_boundary(0), 60_000);
        assert_eq!(next_minute_boundary(59_999), 60_000);
        assert_eq!(next_minute_boundary(60_000), 120_000);
        // Round up to epoch boundary for negative inputs.
        assert_eq!(next_minute_boundary(-1), 0);
        assert_eq!(next_minute_boundary(-60_000), 0);
        assert_eq!(next_minute_boundary(-120_000), -60_000);
    }

    // --- Trigger / OccurrencePolicy ergonomics --------------------------------

    #[test]
    fn default_occurrence_policy_is_conservative() {
        let p = OccurrencePolicy::default();
        assert_eq!(p.dedupe_strategy, DedupeStrategy::KeyHash);
        assert_eq!(p.misfire_policy, MisfirePolicy::Skip);
        assert_eq!(p.overlap_policy, OverlapPolicy::Forbid);
        assert_eq!(p.max_concurrency, 1);
    }

    #[test]
    fn trigger_serde_round_trip() {
        let t = Trigger {
            id: "wft_alpha".into(),
            kind: TriggerKind::Cron,
            cron_expression: Some("*/5 * * * *".into()),
            timezone: Some("America/Los_Angeles".into()),
            dedupe_key: None,
            unattended: false,
        };
        let json = serde_json::to_string(&t).unwrap();
        let back: Trigger = serde_json::from_str(&json).unwrap();
        assert_eq!(t, back);
    }

    #[test]
    fn webhook_trigger_is_always_unattended() {
        let t = Trigger::webhook("wft_hook".into(), Some("dedupe-1".into()));
        assert_eq!(t.kind, TriggerKind::Webhook);
        assert!(t.unattended);
        assert!(t.requires_forced_ask());
    }

    #[test]
    fn requires_forced_ask_catches_hand_built_webhook_trigger() {
        // A caller that bypasses `Trigger::webhook` and hand-builds the
        // struct with `unattended: false` must still be caught by
        // `requires_forced_ask` via the trigger kind — the invariant is
        // about the trigger's origin, not just the flag.
        let t = Trigger {
            id: "wft_hand_built".into(),
            kind: TriggerKind::Webhook,
            cron_expression: None,
            timezone: None,
            dedupe_key: None,
            unattended: false,
        };
        assert!(t.requires_forced_ask());
    }

    #[test]
    fn manual_trigger_does_not_require_forced_ask() {
        let t = Trigger {
            id: "wft_manual".into(),
            kind: TriggerKind::Manual,
            cron_expression: None,
            timezone: None,
            dedupe_key: None,
            unattended: false,
        };
        assert!(!t.requires_forced_ask());
    }

    #[test]
    fn deserializing_a_pre_eng95_trigger_json_defaults_to_attended() {
        // Older serialized triggers (pre-ENG-95) have no `unattended`
        // field at all. `#[serde(default)]` must not silently mark them
        // unattended==true — only `Trigger::webhook` and an explicit
        // `Webhook` kind do that (via `requires_forced_ask`).
        let json = serde_json::json!({
            "id": "wft_old",
            "kind": { "kind": "cron", "value": null },
            "cron_expression": "*/5 * * * *",
            "timezone": "UTC",
            "dedupe_key": null
        })
        .to_string();
        let t: Trigger = serde_json::from_str(&json).unwrap();
        assert!(!t.unattended);
        assert!(!t.requires_forced_ask());
    }

    #[test]
    fn causal_receipts_require_ordered_parent_links() {
        let chain = vec![
            CausalReceipt {
                id: "tr".into(),
                sequence: 0,
                kind: CausalReceiptKind::Trigger {
                    trigger_id: "t".into(),
                },
                created_at_unix_ms: 0,
            },
            CausalReceipt {
                id: "run".into(),
                sequence: 1,
                kind: CausalReceiptKind::Run {
                    run_id: "r".into(),
                    trigger_receipt_id: "tr".into(),
                },
                created_at_unix_ms: 1,
            },
            CausalReceipt {
                id: "task".into(),
                sequence: 2,
                kind: CausalReceiptKind::ChildTask {
                    task_id: "c".into(),
                    run_receipt_id: "run".into(),
                },
                created_at_unix_ms: 2,
            },
            CausalReceipt {
                id: "result".into(),
                sequence: 3,
                kind: CausalReceiptKind::Result {
                    result_id: "o".into(),
                    task_receipt_id: "task".into(),
                },
                created_at_unix_ms: 3,
            },
        ];
        CausalReceipt::validate_chain(&chain).expect("causal chain");
        let mut broken = chain;
        broken[2].sequence = 4;
        assert!(CausalReceipt::validate_chain(&broken).is_err());
    }

    // --- Test helpers ---------------------------------------------------------

    fn unix_ms_from_civil(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> i64 {
        // Inverse of unix_ms_to_civil, used to build deterministic inputs.
        // Compute days from 1970-01-01, then add seconds.
        let (ey, em) = if mo <= 2 {
            (y - 1, mo + 9)
        } else {
            (y, mo - 3)
        };
        let era = if ey >= 0 { ey } else { ey - 399 } / 400;
        let yoe = (ey - era * 400) as i64;
        let doy = (153 * (em as i64) + 2) / 5 + d as i64 - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        let days = (era as i64) * 146_097 + doe - 719_468;
        days * 86_400_000 + (h as i64) * 3_600_000 + (mi as i64) * 60_000 + (s as i64) * 1000
    }
}

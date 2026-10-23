//! Ownership admission. Port of `OwnershipAdmission` in
//! `packages/release/ownership-admission.mjs`.
//!
//! Every admission re-authenticates inputs with an inventory worker, including cache
//! hits. A verdict is reused only when the freshly computed six-binding cache key
//! matches and the scan root matches. Concurrent identical requests share one scan.
//! Worker processes are bounded globally and per repository (see [`crate::limits`]).
//!
//! Modes: `off` never scans. `report` records the same verdict as `enforce` but
//! never refuses. `enforce` refuses every result that is not clean.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use crate::clock::{Clock, SystemClock};
use serde_json::Value;

use crate::json::{field, truthy};
use crate::limits::{Kind, Limiter, WorkerLimits};
use crate::runner::{resolve_lexical, ProcessScanRunner, ScanRequest, ScanRunner};
use crate::verdict::{codes, ownership_cache_key, ownership_verdict};

const CACHE_LIMIT: usize = 128;
/// Grace added to a follower's wait beyond the worker deadlines it depends on.
const FOLLOWER_GRACE: Duration = Duration::from_secs(5);
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Configured `ownership.mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Off,
    Report,
    Enforce,
}

impl Mode {
    /// Parses the configured mode. Node rejects anything else with a TypeError.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "off" => Ok(Self::Off),
            "report" => Ok(Self::Report),
            "enforce" => Ok(Self::Enforce),
            _ => Err("ownership mode must be one of off, report, enforce".into()),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Report => "report",
            Self::Enforce => "enforce",
        }
    }
}

/// One violation as recorded in the admission result. Fields keep the report's raw
/// JSON (null when absent), as Node does.
#[derive(Debug, Clone, PartialEq)]
pub struct Violation {
    pub capability: Value,
    pub owner: Value,
    pub fingerprint: Value,
    pub file: Value,
    pub line: Value,
}

/// Admission result. Node's result object, typed.
#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    pub mode: Mode,
    /// Node's `outcome`: clean, violations, pending-enrollment, malformed-report,
    /// incomplete-coverage, invalid-registration, policy-downgrade, report-failed,
    /// problems, off or unscanned.
    pub outcome: &'static str,
    pub admit: bool,
    /// `None` only for `off`.
    pub code: Option<String>,
    pub scan_root: Option<String>,
    pub error: Option<String>,
    pub problems: Vec<String>,
    pub violations: Vec<Violation>,
    pub scan_ms: u64,
    pub cached: bool,
}

impl Verdict {
    fn off(mode: Mode) -> Self {
        Self {
            mode,
            outcome: "off",
            admit: false,
            code: None,
            scan_root: None,
            error: None,
            problems: Vec::new(),
            violations: Vec::new(),
            scan_ms: 0,
            cached: false,
        }
    }

    /// True when the verdict is `RK_OWNERSHIP_CLEAN` and admitted.
    pub fn is_clean(&self) -> bool {
        self.admit && self.code.as_deref() == Some(codes::CLEAN)
    }

    /// Human-readable reason for a refusal, for callers that carry only text.
    pub fn refusal_reason(&self) -> String {
        let code = self.code.as_deref().unwrap_or(codes::UNVERIFIED);
        let detail = if self.problems.is_empty() {
            self.error
                .clone()
                .unwrap_or_else(|| "ownership verdict is missing or malformed".into())
        } else {
            self.problems
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join("; ")
        };
        format!("{code}: {detail}")
    }
}

/// Enforce-mode refusal, carrying the verdict code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub code: String,
    pub message: String,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Refusal {}

/// Repository the request names. `repository_root` wins; `workspace_root` is the
/// fallback, as in Node.
#[derive(Debug, Clone, Copy, Default)]
pub struct Target<'a> {
    pub repository_root: Option<&'a str>,
    pub workspace_root: Option<&'a str>,
}

/// Everything an admission needs. Production defaults come from [`AdmissionConfig::new`].
pub struct AdmissionConfig {
    pub mode: Mode,
    pub timeout: Duration,
    /// Node-worker scanner root, passed through to [`ScanRequest::scanner_root`].
    /// Whether the scanner is staged is decided by [`ScanRunner::staged`], not by this
    /// field; only the Node-compat [`ProcessScanRunner`] path needs it.
    pub scanner_root: Option<PathBuf>,
    pub clock: Arc<dyn Clock>,
    pub inputs_runner: Arc<dyn ScanRunner>,
    pub scan_runner: Arc<dyn ScanRunner>,
    pub limits: WorkerLimits,
}

impl AdmissionConfig {
    /// Production defaults: the Node worker run through `node`, the system clock and
    /// default worker limits.
    pub fn new(mode: Mode, scanner_root: Option<PathBuf>, node: impl Into<PathBuf>) -> Self {
        let runner: Arc<dyn ScanRunner> =
            Arc::new(ProcessScanRunner::new(node).with_scanner_root(scanner_root.clone()));
        Self {
            mode,
            timeout: DEFAULT_TIMEOUT,
            scanner_root,
            clock: Arc::new(SystemClock),
            inputs_runner: Arc::clone(&runner),
            scan_runner: runner,
            limits: WorkerLimits::default(),
        }
    }
}

/// Result of Node `classifyOwnershipReport`: the shared verdict plus the outcome label
/// and the violation summary.
#[derive(Debug, Clone, PartialEq)]
pub struct Classification {
    pub admit: bool,
    pub code: &'static str,
    pub problems: Vec<String>,
    pub outcome: &'static str,
    pub violations: Vec<Violation>,
    /// Reported warnings on an admitted verdict (Node spreads the verdict's `warnings`).
    pub warnings: Vec<String>,
}

/// Classifies one report against the protected trust registry.
pub fn classify_ownership_report(report: &Value, trust: &Value) -> Classification {
    let decision = ownership_verdict(report, trust);
    Classification {
        outcome: outcome_for_code(decision.code),
        admit: decision.admit,
        code: decision.code,
        problems: decision.problems,
        violations: violations_of(report),
        warnings: decision.warnings,
    }
}

fn outcome_for_code(code: &str) -> &'static str {
    match code {
        codes::CLEAN => "clean",
        codes::VIOLATION => "violations",
        codes::PENDING_ENROLLMENT => "pending-enrollment",
        codes::MALFORMED_REPORT => "malformed-report",
        codes::INCOMPLETE_COVERAGE => "incomplete-coverage",
        codes::INVALID_REGISTRATION => "invalid-registration",
        codes::POLICY_DOWNGRADE => "policy-downgrade",
        codes::REPORT_FAILED => "report-failed",
        codes::UNVERIFIED => "problems",
        _ => "unscanned",
    }
}

fn violations_of(report: &Value) -> Vec<Violation> {
    let Some(list) = field(report, "violations").as_array() else {
        return Vec::new();
    };
    list.iter()
        .filter(|entry| entry.is_object() || entry.is_array())
        .map(|entry| Violation {
            capability: field(entry, "capability").clone(),
            owner: field(entry, "package").clone(),
            fingerprint: field(entry, "fingerprint").clone(),
            file: field(entry, "file").clone(),
            line: field(entry, "line").clone(),
        })
        .collect()
}

/// Node `ownershipScanRoot`: repository root, else workspace root, lexically resolved.
fn scan_root(target: Target<'_>) -> Result<String, String> {
    match target.repository_root.or(target.workspace_root) {
        Some(root) if !root.is_empty() => Ok(resolve_lexical(Path::new(root))
            .to_string_lossy()
            .into_owned()),
        _ => Err("repository root is missing or malformed".into()),
    }
}

/// Result of a single-flight scan. Followers wait on it.
#[derive(Default)]
struct Flight {
    result: Mutex<Option<Verdict>>,
    ready: Condvar,
}

impl Flight {
    fn complete(&self, verdict: Verdict) {
        *self.result.lock().unwrap_or_else(PoisonError::into_inner) = Some(verdict);
        self.ready.notify_all();
    }

    fn wait(&self, bound: Duration) -> Option<Verdict> {
        let deadline = std::time::Instant::now() + bound;
        let mut slot = self.result.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(verdict) = slot.as_ref() {
                return Some(verdict.clone());
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return None;
            }
            let (guard, _) = self
                .ready
                .wait_timeout(slot, remaining)
                .unwrap_or_else(PoisonError::into_inner);
            slot = guard;
        }
    }
}

#[derive(Default)]
struct AdmissionState {
    cache: HashMap<String, Verdict>,
    cache_order: VecDeque<String>,
    inflight: HashMap<String, Arc<Flight>>,
}

enum Claim {
    Cached(Verdict),
    Follower(Arc<Flight>),
    Leader(Arc<Flight>),
}

/// Ownership admission gate. Thread-safe; share it behind an `Arc`.
pub struct OwnershipAdmission {
    mode: Mode,
    timeout: Duration,
    scanner_root: Option<PathBuf>,
    clock: Arc<dyn Clock>,
    inputs_runner: Arc<dyn ScanRunner>,
    scan_runner: Arc<dyn ScanRunner>,
    limits: WorkerLimits,
    limiter: Limiter,
    state: Mutex<AdmissionState>,
}

impl OwnershipAdmission {
    pub fn new(config: AdmissionConfig) -> Self {
        Self {
            mode: config.mode,
            timeout: config.timeout,
            scanner_root: config.scanner_root,
            clock: config.clock,
            inputs_runner: config.inputs_runner,
            scan_runner: config.scan_runner,
            limits: config.limits,
            limiter: Limiter::new(config.limits),
            state: Mutex::new(AdmissionState::default()),
        }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    fn lock(&self) -> MutexGuard<'_, AdmissionState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Convenience for the broker's `(repository_root)` call shape.
    pub fn evaluate_repository(&self, repository_root: Option<&str>) -> Verdict {
        self.evaluate(Target {
            repository_root,
            workspace_root: None,
        })
    }

    /// Admit or refuse a root. Node `evaluate`.
    pub fn evaluate(&self, target: Target<'_>) -> Verdict {
        if self.mode == Mode::Off {
            return Verdict::off(self.mode);
        }
        let root = match scan_root(target) {
            Ok(root) => root,
            Err(message) => return self.failure(None, message, Some(codes::INVALID_REGISTRATION)),
        };
        if !(self.inputs_runner.staged() && self.scan_runner.staged()) {
            return self.failure(
                Some(&root),
                "ownership scanner is not staged in this runtime".into(),
                None,
            );
        }
        let scanner = self
            .scanner_root
            .as_deref()
            .unwrap_or_else(|| Path::new(""));
        self.admit(&root, scanner)
    }

    fn admit(&self, root: &str, scanner: &Path) -> Verdict {
        // Inputs are re-authenticated on every admission, including cache hits.
        let observed = match self.limited(root, Kind::Probe, || {
            self.inputs_runner.run(&ScanRequest {
                scanner_root: scanner,
                scan_root: root,
                timeout: self.timeout,
                inputs_only: true,
            })
        }) {
            Ok(value) => value,
            Err(message) => return self.failure(Some(root), message, None),
        };
        if truthy(field(&observed, "error")) {
            return self.failure(
                Some(root),
                text_of(field(&observed, "error")),
                code_of(&observed),
            );
        }
        let Some(complete) = field(&observed, "complete").as_bool() else {
            return self.failure(
                Some(root),
                "source inventory completeness is missing or malformed".into(),
                Some(codes::INVALID_INPUTS),
            );
        };
        // Exceptions expire by UTC date ("valid through `expires`"), so a verdict is
        // reusable only on the day it was computed and can never outlive one.
        let bound = match ownership_cache_key(field(&observed, "inputs")) {
            Ok(bound) => bound,
            Err(error) => return self.failure(Some(root), error.message, Some(error.code)),
        };
        let key = format!("{bound}:{}", utc_date(self.clock.now_ms()));
        if !complete {
            return self.failure(
                Some(root),
                "source content inventory has incomplete coverage".into(),
                Some(codes::INCOMPLETE_COVERAGE),
            );
        }
        let trust = field(&observed, "trust").clone();
        let flight_key = format!("{root}:{key}");
        match self.claim(root, &key, &flight_key) {
            Claim::Cached(verdict) => verdict,
            Claim::Follower(flight) => flight.wait(self.follower_bound()).unwrap_or_else(|| {
                self.failure(
                    Some(root),
                    "ownership scan did not finish within its bound".into(),
                    None,
                )
            }),
            Claim::Leader(flight) => {
                let (verdict, cacheable) = self.scan(root, &bound, &trust, scanner);
                {
                    let mut state = self.lock();
                    if cacheable {
                        store(&mut state, &key, verdict.clone());
                    }
                    state.inflight.remove(&flight_key);
                }
                flight.complete(verdict.clone());
                verdict
            }
        }
    }

    /// Cache lookup and in-flight lookup under one lock, so a finished scan cannot be
    /// missed between the two checks.
    fn claim(&self, root: &str, key: &str, flight_key: &str) -> Claim {
        let mut state = self.lock();
        if let Some(hit) = state.cache.get(key) {
            if hit.scan_root.as_deref() == Some(root) {
                let mut verdict = hit.clone();
                verdict.cached = true;
                return Claim::Cached(verdict);
            }
        }
        if let Some(flight) = state.inflight.get(flight_key) {
            return Claim::Follower(Arc::clone(flight));
        }
        let flight = Arc::new(Flight::default());
        state
            .inflight
            .insert(flight_key.to_string(), Arc::clone(&flight));
        Claim::Leader(flight)
    }

    /// Protected scan for one leader. Returns the verdict and whether it may be cached.
    fn scan(&self, root: &str, key: &str, trust: &Value, scanner: &Path) -> (Verdict, bool) {
        let started = self.clock.now_ms();
        let scanned = match self.limited(root, Kind::Scan, || {
            self.scan_runner.run(&ScanRequest {
                scanner_root: scanner,
                scan_root: root,
                timeout: self.timeout,
                inputs_only: false,
            })
        }) {
            Ok(value) => value,
            Err(message) => return (self.failure(Some(root), message, None), false),
        };
        if truthy(field(&scanned, "error")) {
            return (
                self.failure(
                    Some(root),
                    text_of(field(&scanned, "error")),
                    code_of(&scanned),
                ),
                false,
            );
        }
        match ownership_cache_key(field(&scanned, "inputs")) {
            Err(error) => {
                return (
                    self.failure(Some(root), error.message, Some(error.code)),
                    false,
                )
            }
            Ok(bound) if bound != key => {
                return (
                    self.failure(
                        Some(root),
                        "ownership inputs changed between admission & protected scan".into(),
                        Some(codes::INPUTS_CHANGED),
                    ),
                    false,
                );
            }
            Ok(_) => {}
        }
        let classified = classify_ownership_report(field(&scanned, "report"), trust);
        let cacheable = !matches!(classified.code, codes::MALFORMED_REPORT | codes::UNVERIFIED);
        let verdict = Verdict {
            mode: self.mode,
            outcome: classified.outcome,
            admit: classified.admit,
            code: Some(classified.code.to_string()),
            scan_root: Some(root.to_string()),
            error: None,
            problems: classified.problems,
            violations: classified.violations,
            scan_ms: self.clock.now_ms().saturating_sub(started),
            cached: false,
        };
        (verdict, cacheable)
    }

    /// Runs `work` while holding one worker slot for `root`.
    fn limited(
        &self,
        root: &str,
        kind: Kind,
        work: impl FnOnce() -> Value,
    ) -> Result<Value, String> {
        let _permit = self.limiter.acquire(root, kind)?;
        Ok(work())
    }

    /// Longest a follower waits for a leader: one scan deadline plus a slot wait plus grace.
    fn follower_bound(&self) -> Duration {
        self.timeout + self.limits.acquire_timeout() + FOLLOWER_GRACE
    }

    fn failure(&self, root: Option<&str>, error: String, code: Option<&str>) -> Verdict {
        let code = code.unwrap_or(codes::UNVERIFIED);
        let outcome = if code == codes::UNVERIFIED {
            "unscanned"
        } else {
            outcome_for_code(code)
        };
        Verdict {
            mode: self.mode,
            outcome,
            admit: false,
            code: Some(code.to_string()),
            scan_root: root.map(str::to_string),
            error: Some(error.clone()),
            problems: vec![error],
            violations: Vec::new(),
            scan_ms: 0,
            cached: false,
        }
    }

    /// Enforce refuses every result that is not clean. Report and off never refuse.
    /// Node `assertAdmitted`. Note: `RK_OWNERSHIP_ADVISORY` is refused in enforce.
    pub fn assert_admitted(&self, verdict: &Verdict) -> Result<(), Refusal> {
        if self.mode != Mode::Enforce || verdict.is_clean() {
            return Ok(());
        }
        let detail = if verdict.problems.is_empty() {
            verdict.error.clone().filter(|e| !e.is_empty())
        } else {
            Some(
                verdict
                    .problems
                    .iter()
                    .take(5)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        };
        Err(Refusal {
            code: verdict
                .code
                .clone()
                .unwrap_or_else(|| codes::UNVERIFIED.to_string()),
            message: format!(
                "RightKit ownership refuses this build: {}",
                detail.unwrap_or_else(|| "ownership verdict is missing or malformed".into())
            ),
        })
    }
}

fn store(state: &mut AdmissionState, key: &str, verdict: Verdict) {
    if state.cache.insert(key.to_string(), verdict).is_none() {
        state.cache_order.push_back(key.to_string());
    }
    if state.cache_order.len() > CACHE_LIMIT {
        if let Some(oldest) = state.cache_order.pop_front() {
            state.cache.remove(&oldest);
        }
    }
}

/// Node `String(x)` for an error field, used when a worker reports a non-string error.
fn text_of(value: &Value) -> String {
    match value.as_str() {
        Some(text) => text.to_string(),
        None => value.to_string(),
    }
}

/// Worker-supplied code, when it is a string. Anything else takes the default.
fn code_of(value: &Value) -> Option<&str> {
    value.get("code").and_then(Value::as_str)
}

/// `YYYY-MM-DD` in UTC for Unix milliseconds (civil-from-days).
pub(crate) fn utc_date(ms: u64) -> String {
    let z = (ms / 86_400_000) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod expiry_tests {
    use super::utc_date;

    #[test]
    fn cache_day_is_the_utc_date() {
        assert_eq!(utc_date(0), "1970-01-01");
        assert_eq!(utc_date(1_791_504_000_000), "2026-10-09");
        assert_eq!(utc_date(1_791_590_399_999), "2026-10-09");
        assert_eq!(utc_date(1_791_590_400_000), "2026-10-10");
    }
}

//! End-to-end journeys: the `journeys.toml` manifest, the adapter protocol and the
//! `rightkit.journeys.receipt/1` receipt (`docs/E2E-TESTING-STANDARD.md` D3, D4, D5).
//!
//! * `manifest`: the manifest and mapping types and [`check`], the static validation.
//! * `adapter`: what an app's own Rust test adapter links: read the selection, append
//!   `start`, `journey` and `end` lines to the results file.
//! * `receipt`: the receipt types and `validate_receipt`, which recomputes the verdict
//!   from the evidence and never trusts the receipt's own `verdict` field.
//!
//! Not here yet: `journeys run` (select, build, launch, collect), `census` and `gate`. The
//! CLI offers `journeys check` and `journeys validate-receipt` only.
//!
//! Hard rule of this module: a journey counts as passed only on positive evidence. Zero
//! journeys, a missing or duplicate or unselected result, a skipped or errored journey, a stale
//! `run_id` or `nonce`, a digest that does not match, a failed isolation or a non-zero adapter
//! exit each make the verdict something other than `passed`, and an input that cannot be
//! checked (no results file) is a problem, not a pass.
pub mod adapter;
pub mod manifest;
pub mod receipt;

#[cfg(test)]
mod tests;

pub use manifest::{check, check_manifest, check_mapping, Checked, Manifest, Mapping};
pub use receipt::{
    parse_receipt, validate_receipt, Counts, Receipt, ValidateOptions, Validation, Verdict,
};

use serde::{Deserialize, Serialize};

/// Receipt schema identifier (D5).
pub const RECEIPT_SCHEMA: &str = "rightkit.journeys.receipt/1";
/// Adapter protocol version (D4).
pub const PROTOCOL: u32 = 1;

/// Machine-readable problem codes. The first twelve are the D5 codes; the rest are added by this
/// implementation where D5 states a rule without naming a code.
pub mod code {
    pub const ZERO_EXECUTED: &str = "zero_executed";
    pub const MISSING_RESULT: &str = "missing_result";
    pub const DUPLICATE_RESULT: &str = "duplicate_result";
    pub const UNSELECTED_RESULT: &str = "unselected_result";
    pub const SKIPPED: &str = "skipped";
    pub const PROTOCOL_VIOLATION: &str = "protocol_violation";
    pub const STALE_RESULTS: &str = "stale_results";
    pub const ARTIFACT_MISMATCH: &str = "artifact_mismatch";
    pub const ARTIFACT_NOT_FROM_BUILD: &str = "artifact_not_from_build";
    pub const SOURCE_MISMATCH: &str = "source_mismatch";
    pub const ISOLATION: &str = "isolation";
    pub const ADAPTER_EXIT: &str = "adapter_exit";
    // Added here (D5 states the rule without a code):
    /// A selected journey ended `failed` or `errored`.
    pub const JOURNEY_FAILED: &str = "journey_failed";
    /// A `passed` journey with no unit, with a failed or skipped unit, or with units that do not add up.
    pub const EMPTY_JOURNEY: &str = "empty_journey";
    /// The receipt does not have the `rightkit.journeys.receipt/1` shape or a field is malformed.
    pub const SCHEMA: &str = "schema";
    /// The receipt's `counts` disagree with what its journeys say.
    pub const COUNTS_MISMATCH: &str = "counts_mismatch";
    /// The receipt's verdict or problem list contradicts the recomputed verdict.
    pub const VERDICT_MISMATCH: &str = "verdict_mismatch";
    /// The receipt's journeys disagree with the adapter's results file.
    pub const RECEIPT_RESULTS_MISMATCH: &str = "receipt_results_mismatch";
    /// No results file was supplied, so the adapter's records could not be re-checked.
    pub const RESULTS_UNVERIFIED: &str = "results_unverified";
    /// Nothing was recorded as launched.
    pub const NO_LAUNCH: &str = "no_launch";
    /// The selection is empty-id or lists an id twice.
    pub const SELECTION_INVALID: &str = "selection_invalid";
    /// An evidence file is missing, unsafe to read or does not match its recorded hash.
    pub const EVIDENCE_MISMATCH: &str = "evidence_mismatch";
    /// The tree is dirty (refused unless explicitly allowed).
    pub const DIRTY_TREE: &str = "dirty_tree";
    /// The receipt's host does not match the platform it claims to cover.
    pub const HOST_MISMATCH: &str = "host_mismatch";

    /// Codes that mean "no usable result" (verdict `incomplete`) rather than "a journey failed".
    pub fn is_incomplete(code: &str) -> bool {
        matches!(
            code,
            ZERO_EXECUTED
                | MISSING_RESULT
                | PROTOCOL_VIOLATION
                | STALE_RESULTS
                | RESULTS_UNVERIFIED
                | NO_LAUNCH
                | SCHEMA
        )
    }
}

/// A named reason a receipt or results file is not a pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Problem {
    pub code: String,
    pub id: Option<String>,
    pub detail: String,
}

impl Problem {
    pub fn new(code: &str, id: Option<&str>, detail: impl Into<String>) -> Self {
        Problem {
            code: code.to_string(),
            id: id.map(str::to_string),
            detail: detail.into(),
        }
    }
}

/// Result of one journey. `Missing` is synthesized by the runner and never written by an adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JourneyStatus {
    Passed,
    Failed,
    Skipped,
    Errored,
    Missing,
}

/// Scenario or test counts inside one journey.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Units {
    pub total: u64,
    pub passed: u64,
    pub failed: u64,
    pub skipped: u64,
}

impl Units {
    /// The D4 rule for `passed`: at least one unit, none failed, none skipped, and the parts add up.
    pub fn supports_pass(&self) -> bool {
        self.total >= 1 && self.failed == 0 && self.skipped == 0 && self.passed == self.total
    }
    /// The parts add up to the total.
    pub fn consistent(&self) -> bool {
        self.passed
            .checked_add(self.failed)
            .and_then(|x| x.checked_add(self.skipped))
            == Some(self.total)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EvidenceKind {
    Log,
    Screenshot,
    Report,
    Trace,
}

/// One evidence file of a journey: a path relative to the run root and its SHA-256.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub path: String,
    pub sha256: String,
    pub kind: EvidenceKind,
}

/// A binary an adapter launches, hashed when it was reported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactDigest {
    pub role: String,
    pub path: String,
    pub sha256: String,
}

/// Lowercase hex of exactly `len` characters.
pub(crate) fn is_hex_lower(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `YYYY-MM-DDTHH:MM:SS[.fraction](Z|+HH:MM|-HH:MM)`: shape and field ranges only.
pub(crate) fn is_rfc3339(s: &str) -> bool {
    let b = s.as_bytes();
    let digits =
        |r: std::ops::Range<usize>| b.len() >= r.end && b[r].iter().all(|c| c.is_ascii_digit());
    if b.len() < 20
        || !digits(0..4)
        || b[4] != b'-'
        || !digits(5..7)
        || b[7] != b'-'
        || !digits(8..10)
        || !(b[10] == b'T' || b[10] == b't')
        || !digits(11..13)
        || b[13] != b':'
        || !digits(14..16)
        || b[16] != b':'
        || !digits(17..19)
    {
        return false;
    }
    let num = |r: std::ops::Range<usize>| s[r].parse::<u32>().unwrap_or(99);
    if !(1..=12).contains(&num(5..7))
        || !(1..=31).contains(&num(8..10))
        || num(11..13) > 23
        || num(14..16) > 59
        || num(17..19) > 60
    {
        return false;
    }
    let mut i = 19;
    if b[i] == b'.' {
        let start = i + 1;
        i = start;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == start {
            return false;
        }
    }
    match &s[i..] {
        "Z" | "z" => true,
        z if z.len() == 6 && (z.starts_with('+') || z.starts_with('-')) => {
            let zb = z.as_bytes();
            zb[1].is_ascii_digit()
                && zb[2].is_ascii_digit()
                && zb[3] == b':'
                && zb[4].is_ascii_digit()
                && zb[5].is_ascii_digit()
        }
        _ => false,
    }
}

/// A relative path that stays inside its root: not empty, not absolute, no drive, no `..`.
pub(crate) fn is_safe_relative(p: &str) -> bool {
    if p.is_empty() || p.starts_with('/') || p.starts_with('\\') || p.contains('\0') {
        return false;
    }
    let pb = p.as_bytes();
    if pb.len() >= 2 && pb[1] == b':' {
        return false;
    }
    !p.split(['/', '\\']).any(|c| c == "..")
}

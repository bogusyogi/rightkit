//! `rightkit.journeys.receipt/1` (D5): the receipt types and [`validate_receipt`].
//!
//! [`validate_receipt`] never believes the receipt's `verdict`. It recomputes counts and verdict
//! from the journeys, the provenance bindings, the isolation record, the adapter exit and (when
//! supplied) the adapter's own `results.jsonl`, the manifest and the evidence files. A receipt is
//! accepted only when [`Validation::passed`] is true: no problem found, recomputed verdict
//! `passed` and claimed verdict `passed`. Anything that could not be checked because an input was
//! not supplied is either a problem (`results_unverified`) or listed in `Validation::unchecked`
//! (manifest binding, evidence hashes) and never counted as a pass of that check.
use super::adapter::{JourneyRecord, Record, StartRecord};
use super::manifest::Manifest;
use super::{
    code, is_hex_lower, is_rfc3339, is_safe_relative, Evidence, JourneyStatus, Problem, Units,
    PROTOCOL, RECEIPT_SCHEMA,
};
use crate::util::{sha256_hex, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Passed,
    Failed,
    Incomplete,
}

impl Verdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            Verdict::Passed => "passed",
            Verdict::Failed => "failed",
            Verdict::Incomplete => "incomplete",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReceiptKind {
    Functional,
    Visual,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Host {
    pub os: String,
    pub arch: String,
    pub session: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub head: String,
    pub tree: String,
    pub dirty: bool,
    pub diff_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRef {
    pub path: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrokerReceipt {
    pub id: String,
    pub path: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProducedBy {
    BrokerReceipt,
    BuildOutput,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildArtifact {
    pub role: String,
    pub path: String,
    pub size: u64,
    pub sha256: String,
    pub produced_by: ProducedBy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Build {
    pub command: Vec<String>,
    pub source_tree: String,
    pub tree_after_build: String,
    pub started_at: String,
    pub ended_at: String,
    pub broker_receipt: Option<BrokerReceipt>,
    pub artifacts: Vec<BuildArtifact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Launched {
    pub role: String,
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptIsolation {
    pub data_dir: String,
    pub guard: String,
    pub qa_mode: String,
    pub real_data_touched: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterRun {
    pub name: String,
    pub version: String,
    pub protocol: u32,
    pub argv: Vec<String>,
    pub exit_code: i64,
    pub results_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptSelection {
    pub ids: Vec<String>,
    pub platform: String,
    pub tier: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptJourney {
    pub id: String,
    pub status: JourneyStatus,
    pub release_critical: bool,
    pub started_at: String,
    pub duration_ms: u64,
    pub units: Units,
    pub evidence: Vec<Evidence>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    pub selected: u64,
    pub executed: u64,
    pub passed: u64,
    pub failed: u64,
    pub skipped: u64,
    pub errored: u64,
    pub missing: u64,
    pub duplicate: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub schema: String,
    pub run_id: String,
    pub nonce: String,
    pub app: String,
    pub kind: ReceiptKind,
    pub verdict: Verdict,
    pub started_at: String,
    pub ended_at: String,
    pub host: Host,
    pub source: Source,
    pub manifest: FileRef,
    pub mapping: Option<FileRef>,
    pub build: Build,
    pub launched: Vec<Launched>,
    pub isolation: ReceiptIsolation,
    pub adapter: AdapterRun,
    pub selection: ReceiptSelection,
    pub journeys: Vec<ReceiptJourney>,
    pub counts: Counts,
    pub problems: Vec<Problem>,
}

/// Parse a receipt. A file that is not a receipt of this shape is an error, never a pass.
pub fn parse_receipt(bytes: &[u8]) -> Result<Receipt> {
    Ok(serde_json::from_slice(bytes)?)
}

/// Inputs that let [`validate_receipt`] check more than the receipt says about itself.
#[derive(Debug, Clone, Copy, Default)]
pub struct ValidateOptions<'a> {
    /// The git tree the caller claims (`--tree`); `source.tree` must equal it.
    pub expected_tree: Option<&'a str>,
    /// The manifest and its SHA-256 (of its exact bytes) the receipt must be bound to.
    pub manifest: Option<(&'a Manifest, &'a str)>,
    /// The adapter's `results.jsonl`. Without it the adapter's records cannot be re-checked and
    /// the receipt is refused (`results_unverified`).
    pub results: Option<&'a [u8]>,
    /// Directory evidence paths are relative to. Without it evidence hashes are listed as unchecked.
    pub evidence_root: Option<&'a Path>,
    /// Accept `source.dirty = true` (release signoff never does).
    pub allow_dirty: bool,
}

/// The outcome of validating a receipt.
#[derive(Debug, Clone)]
pub struct Validation {
    /// Every rule that failed. Empty only when all checks that ran succeeded.
    pub problems: Vec<Problem>,
    /// The verdict recomputed from the evidence.
    pub verdict: Verdict,
    /// The verdict the receipt claims.
    pub claimed: Verdict,
    /// Counts recomputed from the receipt's journeys.
    pub counts: Counts,
    /// Checks that were skipped because their input was not supplied.
    pub unchecked: Vec<String>,
}

impl Validation {
    /// True only when the receipt is internally truthful AND a pass: no problem, recomputed and
    /// claimed verdicts both `passed`.
    pub fn passed(&self) -> bool {
        self.problems.is_empty()
            && self.verdict == Verdict::Passed
            && self.claimed == Verdict::Passed
    }
}

fn oid<T: AsRef<str> + ?Sized>(x: &T) -> Option<&str> {
    Some(x.as_ref())
}

fn add(p: &mut Vec<Problem>, c: &str, id: Option<&str>, detail: impl Into<String>) {
    p.push(Problem::new(c, id, detail));
}

fn verdict_of(p: &[Problem]) -> Verdict {
    if p.is_empty() {
        Verdict::Passed
    } else if p.iter().any(|x| code::is_incomplete(&x.code)) {
        Verdict::Incomplete
    } else {
        Verdict::Failed
    }
}

/// Parse `results.jsonl` into records. Every malformed or empty line is reported; a final line
/// without a newline is reported too (a write that did not finish).
pub fn parse_results(bytes: &[u8]) -> (Vec<Record>, Vec<String>) {
    let text = match std::str::from_utf8(bytes) {
        Ok(t) => t,
        Err(e) => return (vec![], vec![format!("results are not UTF-8: {e}")]),
    };
    let mut recs = vec![];
    let mut errs = vec![];
    if !text.is_empty() && !text.ends_with('\n') {
        errs.push("the last line is not newline-terminated (unfinished write)".to_string());
    }
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            errs.push(format!("line {}: empty", i + 1));
            continue;
        }
        match serde_json::from_str::<Record>(line) {
            Ok(r) => recs.push(r),
            Err(e) => errs.push(format!("line {}: {e}", i + 1)),
        }
    }
    (recs, errs)
}

/// Re-check every D5 pass condition. See the module docs for what is and is not trusted.
pub fn validate_receipt(r: &Receipt, o: &ValidateOptions<'_>) -> Validation {
    let mut p: Vec<Problem> = vec![];
    let mut unchecked: Vec<String> = vec![];
    if r.schema != RECEIPT_SCHEMA {
        add(
            &mut p,
            code::SCHEMA,
            None,
            format!("schema is {:?}, expected {RECEIPT_SCHEMA:?}", r.schema),
        );
        return finish(r, p, Counts::default(), unchecked);
    }
    check_formats(r, &mut p);
    let counts = check_journeys(r, &mut p);
    check_provenance(r, o, &mut p);
    check_isolation_and_adapter(r, &mut p);
    check_results(r, o, &counts, &mut p);
    check_manifest_binding(r, o, &mut p, &mut unchecked);
    check_evidence(r, o, &mut p, &mut unchecked);
    finish(r, p, counts, unchecked)
}

fn finish(r: &Receipt, mut p: Vec<Problem>, counts: Counts, unchecked: Vec<String>) -> Validation {
    let base = verdict_of(&p);
    if r.verdict == Verdict::Passed && !r.problems.is_empty() {
        add(
            &mut p,
            code::VERDICT_MISMATCH,
            None,
            format!(
                "receipt claims passed but lists {} problem(s)",
                r.problems.len()
            ),
        );
    }
    if r.verdict == Verdict::Passed && base != Verdict::Passed {
        add(
            &mut p,
            code::VERDICT_MISMATCH,
            None,
            format!(
                "receipt claims passed; recomputed verdict is {}",
                base.as_str()
            ),
        );
    }
    if r.verdict != Verdict::Passed && p.is_empty() {
        add(
            &mut p,
            code::VERDICT_MISMATCH,
            None,
            format!("receipt claims {} but no rule fails", r.verdict.as_str()),
        );
    }
    let verdict = if base != Verdict::Passed {
        base
    } else if p.is_empty() {
        Verdict::Passed
    } else {
        Verdict::Failed
    };
    Validation {
        problems: p,
        verdict,
        claimed: r.verdict,
        counts,
        unchecked,
    }
}

fn check_formats(r: &Receipt, p: &mut Vec<Problem>) {
    let mut bad = |what: &str| add(p, code::SCHEMA, None, format!("malformed {what}"));
    if r.run_id.trim().is_empty() {
        bad("run_id");
    }
    if !is_hex_lower(&r.nonce, 32) {
        bad("nonce (32 lowercase hex)");
    }
    if r.app.trim().is_empty() {
        bad("app");
    }
    if !is_rfc3339(&r.started_at) || !is_rfc3339(&r.ended_at) {
        bad("started_at or ended_at (RFC 3339)");
    }
    if !is_hex_lower(&r.source.head, 40) {
        bad("source.head (40 hex)");
    }
    if !is_hex_lower(&r.source.tree, 40) {
        bad("source.tree (40 hex)");
    }
    if r.source
        .diff_sha256
        .as_deref()
        .is_some_and(|d| !is_hex_lower(d, 64))
    {
        bad("source.diff_sha256");
    }
    if r.manifest.path.trim().is_empty() || !is_hex_lower(&r.manifest.sha256, 64) {
        bad("manifest {path, sha256}");
    }
    if r.mapping
        .as_ref()
        .is_some_and(|m| m.path.trim().is_empty() || !is_hex_lower(&m.sha256, 64))
    {
        bad("mapping {path, sha256}");
    }
    if !is_hex_lower(&r.build.source_tree, 40) || !is_hex_lower(&r.build.tree_after_build, 40) {
        bad("build.source_tree or build.tree_after_build (40 hex)");
    }
    if r.build.command.is_empty() {
        bad("build.command (empty)");
    }
    if !is_rfc3339(&r.build.started_at) || !is_rfc3339(&r.build.ended_at) {
        bad("build.started_at or build.ended_at (RFC 3339)");
    }
    if r.build.broker_receipt.as_ref().is_some_and(|b| {
        b.id.trim().is_empty() || b.path.trim().is_empty() || !is_hex_lower(&b.sha256, 64)
    }) {
        bad("build.broker_receipt");
    }
    if r.build
        .artifacts
        .iter()
        .any(|a| a.role.trim().is_empty() || !is_hex_lower(&a.sha256, 64))
    {
        bad("build.artifacts (role, sha256)");
    }
    if r.launched
        .iter()
        .any(|a| a.role.trim().is_empty() || !is_hex_lower(&a.sha256, 64))
    {
        bad("launched (role, sha256)");
    }
    if !is_hex_lower(&r.adapter.results_sha256, 64) {
        bad("adapter.results_sha256");
    }
    if r.adapter.name.trim().is_empty() || r.adapter.version.trim().is_empty() {
        bad("adapter name or version");
    }
    if !super::manifest::PLATFORMS.contains(&r.host.os.as_str()) {
        bad("host.os (macos, windows, linux)");
    }
    if r.host.session != "interactive" && r.host.session != "noninteractive" {
        bad("host.session (interactive, noninteractive)");
    }
    if r.host.arch.trim().is_empty() {
        bad("host.arch");
    }
    if r.selection.platform.trim().is_empty() || r.selection.tier.trim().is_empty() {
        bad("selection platform or tier");
    }
    for j in &r.journeys {
        if !is_rfc3339(&j.started_at) && j.status != JourneyStatus::Missing {
            bad("journey started_at (RFC 3339)");
        }
    }
}

/// Rules 1 to 3; returns the counts recomputed from the receipt's journeys.
fn check_journeys(r: &Receipt, p: &mut Vec<Problem>) -> Counts {
    let mut selected: Vec<&str> = vec![];
    let mut selset: BTreeSet<&str> = BTreeSet::new();
    for id in &r.selection.ids {
        if id.trim().is_empty() {
            add(
                p,
                code::SELECTION_INVALID,
                None,
                "selection holds an empty id",
            );
        } else if !selset.insert(id.as_str()) {
            add(
                p,
                code::SELECTION_INVALID,
                oid(id),
                "selection lists the id twice",
            );
        } else {
            selected.push(id.as_str());
        }
    }
    let mut by_id: BTreeMap<&str, Vec<&ReceiptJourney>> = BTreeMap::new();
    for j in &r.journeys {
        by_id.entry(j.id.as_str()).or_default().push(j);
    }
    let mut c = Counts {
        selected: selected.len() as u64,
        ..Counts::default()
    };
    if c.selected == 0 {
        add(
            p,
            code::ZERO_EXECUTED,
            None,
            "no journey was selected; zero is never a pass",
        );
    }
    for id in &selected {
        let Some(entries) = by_id.get(id) else {
            c.missing += 1;
            add(
                p,
                code::MISSING_RESULT,
                oid(id),
                "no result for a selected journey",
            );
            continue;
        };
        if entries.len() > 1 {
            c.duplicate += (entries.len() - 1) as u64;
            add(
                p,
                code::DUPLICATE_RESULT,
                oid(id),
                format!("{} results for one selected journey", entries.len()),
            );
        }
        let j = entries[0];
        if j.status == JourneyStatus::Missing {
            c.missing += 1;
            add(
                p,
                code::MISSING_RESULT,
                oid(id),
                "journey result is missing",
            );
            continue;
        }
        c.executed += 1;
        if !j.units.consistent() {
            add(
                p,
                code::EMPTY_JOURNEY,
                oid(id),
                "units do not add up to total",
            );
        }
        if j.status != JourneyStatus::Passed
            && j.reason.as_deref().is_none_or(|s| s.trim().is_empty())
        {
            add(
                p,
                code::PROTOCOL_VIOLATION,
                oid(id),
                "a reason is required unless passed",
            );
        }
        match j.status {
            JourneyStatus::Passed => {
                c.passed += 1;
                if !j.units.supports_pass() {
                    add(
                        p,
                        code::EMPTY_JOURNEY,
                        oid(id),
                        "passed without a unit, or with a failed or skipped unit",
                    );
                }
            }
            JourneyStatus::Failed => {
                c.failed += 1;
                add(p, code::JOURNEY_FAILED, oid(id), "journey failed");
            }
            JourneyStatus::Errored => {
                c.errored += 1;
                add(p, code::JOURNEY_FAILED, oid(id), "journey errored");
            }
            JourneyStatus::Skipped => {
                c.skipped += 1;
                add(p, code::SKIPPED, oid(id), "skipped is not a pass");
            }
            JourneyStatus::Missing => {}
        }
    }
    for id in by_id.keys() {
        if !selset.contains(id) {
            add(
                p,
                code::UNSELECTED_RESULT,
                oid(id),
                "result for a journey that was not selected",
            );
        }
    }
    if c.selected > 0 && c.executed == 0 {
        add(p, code::ZERO_EXECUTED, None, "no selected journey executed");
    }
    // A runner may record duplicates it dropped from the journey list; never trust fewer.
    if r.counts.duplicate > c.duplicate {
        add(
            p,
            code::DUPLICATE_RESULT,
            None,
            format!("receipt counts {} duplicate result(s)", r.counts.duplicate),
        );
        c.duplicate = r.counts.duplicate;
    }
    if r.counts != c {
        add(
            p,
            code::COUNTS_MISMATCH,
            None,
            format!(
                "receipt counts {:?} but its journeys give {:?}",
                r.counts, c
            ),
        );
    }
    c
}

/// Rule 5 (provenance and artifact bindings) and the dirty-tree refusal.
fn check_provenance(r: &Receipt, o: &ValidateOptions<'_>, p: &mut Vec<Problem>) {
    if r.source.tree != r.build.source_tree || r.build.source_tree != r.build.tree_after_build {
        add(
            p,
            code::SOURCE_MISMATCH,
            None,
            "source.tree, build.source_tree and build.tree_after_build are not all equal",
        );
    }
    if let Some(t) = o.expected_tree {
        if !is_hex_lower(t, 40) {
            add(
                p,
                code::SOURCE_MISMATCH,
                None,
                "expected tree is not 40 lowercase hex characters",
            );
        } else if t != r.source.tree {
            add(
                p,
                code::SOURCE_MISMATCH,
                None,
                format!("source.tree {} is not the claimed tree {t}", r.source.tree),
            );
        }
    }
    if r.source.dirty && r.source.diff_sha256.is_none() {
        add(
            p,
            code::SOURCE_MISMATCH,
            None,
            "dirty tree without diff_sha256",
        );
    }
    if r.source.dirty && !o.allow_dirty {
        add(
            p,
            code::DIRTY_TREE,
            None,
            "the tree was dirty; release signoff requires a clean tree",
        );
    }
    if r.build.artifacts.is_empty() {
        add(
            p,
            code::ARTIFACT_NOT_FROM_BUILD,
            None,
            "the build recorded no artifacts",
        );
    }
    let mut roles = BTreeSet::new();
    for a in &r.build.artifacts {
        if !roles.insert(a.role.as_str()) {
            add(
                p,
                code::ARTIFACT_MISMATCH,
                oid(&a.role),
                "build.artifacts repeats a role",
            );
        }
        if a.produced_by == ProducedBy::BrokerReceipt && r.build.broker_receipt.is_none() {
            add(
                p,
                code::SOURCE_MISMATCH,
                oid(&a.role),
                "produced_by broker_receipt but no broker_receipt is recorded",
            );
        }
    }
    if r.launched.is_empty() {
        add(p, code::NO_LAUNCH, None, "nothing was recorded as launched");
    }
    let mut launched_roles = BTreeSet::new();
    for l in &r.launched {
        if !launched_roles.insert(l.role.as_str()) {
            add(
                p,
                code::ARTIFACT_MISMATCH,
                oid(&l.role),
                "launched repeats a role",
            );
        }
        match r.build.artifacts.iter().find(|a| a.role == l.role) {
            None => add(
                p,
                code::ARTIFACT_NOT_FROM_BUILD,
                oid(&l.role),
                "launched role has no artifact in this build",
            ),
            Some(a) if a.sha256 != l.sha256 => add(
                p,
                code::ARTIFACT_NOT_FROM_BUILD,
                oid(&l.role),
                "launched digest differs from the built artifact's digest",
            ),
            Some(_) => {}
        }
    }
}

/// Rules 6 and 7 (except the results hash, checked with the results) plus host binding.
fn check_isolation_and_adapter(r: &Receipt, p: &mut Vec<Problem>) {
    let iso = &r.isolation;
    if iso.guard != "passed" {
        add(
            p,
            code::ISOLATION,
            None,
            format!("isolation guard is {:?}, not \"passed\"", iso.guard),
        );
    }
    if iso.qa_mode != "runtime" {
        add(
            p,
            code::ISOLATION,
            None,
            format!("qa_mode is {:?}, not \"runtime\"", iso.qa_mode),
        );
    }
    if iso.real_data_touched {
        add(p, code::ISOLATION, None, "real user data was touched");
    }
    if iso.data_dir.trim().is_empty() {
        add(p, code::ISOLATION, None, "no isolated data_dir recorded");
    }
    // Nothing launched is NO_LAUNCH (incomplete); the adapter never ran, so its exit code says nothing.
    if r.adapter.exit_code != 0 && !r.launched.is_empty() {
        add(
            p,
            code::ADAPTER_EXIT,
            None,
            format!("adapter exit code {}", r.adapter.exit_code),
        );
    }
    if r.adapter.protocol != PROTOCOL {
        add(
            p,
            code::PROTOCOL_VIOLATION,
            None,
            format!(
                "adapter protocol {}, expected {PROTOCOL}",
                r.adapter.protocol
            ),
        );
    }
    if r.selection.platform != r.host.os {
        add(
            p,
            code::HOST_MISMATCH,
            None,
            format!(
                "selection covers platform {:?} but the run was on {:?}",
                r.selection.platform, r.host.os
            ),
        );
    }
}

/// Rules 2 to 5 and 7 against the adapter's own records.
fn check_results(r: &Receipt, o: &ValidateOptions<'_>, counts: &Counts, p: &mut Vec<Problem>) {
    let Some(bytes) = o.results else {
        add(
            p,
            code::RESULTS_UNVERIFIED,
            None,
            "no results.jsonl supplied: the adapter's records cannot be re-checked",
        );
        return;
    };
    if sha256_hex(bytes) != r.adapter.results_sha256 {
        add(
            p,
            code::ADAPTER_EXIT,
            None,
            "adapter.results_sha256 does not match the results file read",
        );
    }
    let (records, errors) = parse_results(bytes);
    if !errors.is_empty() {
        for e in &errors {
            add(p, code::PROTOCOL_VIOLATION, None, e.clone());
        }
        return;
    }
    if records.is_empty() {
        add(
            p,
            code::PROTOCOL_VIOLATION,
            None,
            "results file holds no records",
        );
        return;
    }
    let rid: &str = &r.run_id;
    let nonce: &str = &r.nonce;
    let stale = records
        .iter()
        .filter(|rec| {
            let (a, b) = match rec {
                Record::Start(s) => (&s.run_id, &s.nonce),
                Record::Journey(j) => (&j.run_id, &j.nonce),
                Record::End(e) => (&e.run_id, &e.nonce),
            };
            a != rid || b != nonce
        })
        .count();
    if stale > 0 {
        add(
            p,
            code::STALE_RESULTS,
            None,
            format!("{stale} record(s) carry another run_id or nonce; the whole file is discarded"),
        );
        return;
    }

    let mut starts: Vec<&StartRecord> = vec![];
    let mut journeys: Vec<&JourneyRecord> = vec![];
    let mut end_count = 0usize;
    for (i, rec) in records.iter().enumerate() {
        match rec {
            Record::Start(s) => {
                starts.push(s);
                if i != 0 {
                    add(
                        p,
                        code::PROTOCOL_VIOLATION,
                        None,
                        "start is not the first record",
                    );
                }
            }
            Record::Journey(j) => {
                journeys.push(j);
                if i == 0 {
                    add(
                        p,
                        code::PROTOCOL_VIOLATION,
                        None,
                        "a journey record precedes start",
                    );
                }
            }
            Record::End(_) => {
                end_count += 1;
                if i != records.len() - 1 {
                    add(
                        p,
                        code::PROTOCOL_VIOLATION,
                        None,
                        "end is not the last record",
                    );
                }
            }
        }
    }
    if starts.len() != 1 {
        add(
            p,
            code::PROTOCOL_VIOLATION,
            None,
            format!("{} start record(s), expected 1", starts.len()),
        );
    }
    if end_count != 1 {
        add(
            p,
            code::PROTOCOL_VIOLATION,
            None,
            format!("{end_count} end record(s), expected 1"),
        );
    }
    if let Some(Record::End(e)) = records.last() {
        if e.selected != counts.selected || e.reported != counts.selected {
            add(
                p,
                code::PROTOCOL_VIOLATION,
                None,
                format!(
                    "end says selected {} reported {}, expected both {}",
                    e.selected, e.reported, counts.selected
                ),
            );
        }
        if e.reported != journeys.len() as u64 {
            add(
                p,
                code::PROTOCOL_VIOLATION,
                None,
                format!(
                    "end says reported {} but {} journey record(s) exist",
                    e.reported,
                    journeys.len()
                ),
            );
        }
    }

    // start: protocol, isolation, artifact digests.
    if let Some(s) = starts.first() {
        if s.protocol != PROTOCOL {
            add(
                p,
                code::PROTOCOL_VIOLATION,
                None,
                format!("start.protocol is {}", s.protocol),
            );
        }
        if s.isolation.guard != "passed"
            || s.isolation.qa_mode != "runtime"
            || s.isolation.data_dir.trim().is_empty()
        {
            add(
                p,
                code::ISOLATION,
                None,
                "start.isolation is not guard passed, qa_mode runtime",
            );
        }
        for a in &s.artifacts {
            if let Some(b) = r.build.artifacts.iter().find(|b| b.role == a.role) {
                if b.sha256 != a.sha256 {
                    add(
                        p,
                        code::ARTIFACT_MISMATCH,
                        oid(&a.role),
                        "adapter reported a digest that differs from the built artifact",
                    );
                }
            }
        }
        let exe_role = o
            .manifest
            .and_then(|(m, _)| m.adapter.iter().find(|a| a.name == r.adapter.name))
            .map(|a| a.executable.role.as_str())
            .unwrap_or("adapter");
        for l in &r.launched {
            match s.artifacts.iter().find(|a| a.role == l.role) {
                Some(a) if a.sha256 != l.sha256 => add(
                    p,
                    code::ARTIFACT_MISMATCH,
                    oid(&l.role),
                    "launched digest differs from the digest the adapter reported",
                ),
                Some(_) => {}
                None if l.role == exe_role => {}
                None => add(
                    p,
                    code::ARTIFACT_MISMATCH,
                    oid(&l.role),
                    "the adapter's start did not report the launched binary",
                ),
            }
        }
    }

    // journey records against the selection and against the receipt's journeys.
    let selset: BTreeSet<&str> = r.selection.ids.iter().map(String::as_str).collect();
    let mut by_id: BTreeMap<&str, Vec<&JourneyRecord>> = BTreeMap::new();
    for j in &journeys {
        by_id.entry(j.id.as_str()).or_default().push(j);
    }
    let mut file_dups = 0u64;
    for (id, recs) in &by_id {
        if !selset.contains(id) {
            add(
                p,
                code::UNSELECTED_RESULT,
                oid(id),
                "the adapter reported an unselected journey",
            );
        }
        if recs.len() > 1 {
            file_dups += (recs.len() - 1) as u64;
            add(
                p,
                code::DUPLICATE_RESULT,
                oid(id),
                format!(
                    "the adapter reported {} records for one journey",
                    recs.len()
                ),
            );
        }
        let j = recs[0];
        if j.status == JourneyStatus::Missing {
            add(
                p,
                code::PROTOCOL_VIOLATION,
                oid(id),
                "an adapter never reports status missing",
            );
        }
        if !is_rfc3339(&j.started_at) {
            add(
                p,
                code::PROTOCOL_VIOLATION,
                oid(id),
                "started_at is not RFC 3339",
            );
        }
        if j.status == JourneyStatus::Passed && !j.units.supports_pass() {
            add(
                p,
                code::EMPTY_JOURNEY,
                oid(id),
                "the adapter reported passed without a passing unit set",
            );
        }
    }
    if r.counts.duplicate < file_dups {
        add(
            p,
            code::COUNTS_MISMATCH,
            None,
            format!(
                "results hold {file_dups} duplicate record(s), receipt counts {}",
                r.counts.duplicate
            ),
        );
    }
    for id in &r.selection.ids {
        let file = by_id.get(id.as_str()).map(|v| v[0]);
        let rec = r.journeys.iter().find(|j| &j.id == id);
        match (file, rec) {
            (None, Some(j)) if j.status != JourneyStatus::Missing => add(
                p,
                code::RECEIPT_RESULTS_MISMATCH,
                oid(id),
                "the receipt has a result the adapter never reported",
            ),
            (Some(_), None) => add(
                p,
                code::RECEIPT_RESULTS_MISMATCH,
                oid(id),
                "the adapter reported a result the receipt lacks",
            ),
            (Some(f), Some(j))
                if f.status != j.status
                    || f.units != j.units
                    || f.evidence != j.evidence
                    || f.reason != j.reason
                    || f.started_at != j.started_at
                    || f.duration_ms != j.duration_ms =>
            {
                add(
                    p,
                    code::RECEIPT_RESULTS_MISMATCH,
                    oid(id),
                    "the receipt's journey differs from the adapter's record",
                );
            }
            _ => {}
        }
    }
}

fn check_manifest_binding(
    r: &Receipt,
    o: &ValidateOptions<'_>,
    p: &mut Vec<Problem>,
    unchecked: &mut Vec<String>,
) {
    let Some((m, sha)) = o.manifest else {
        let _ = &unchecked;
        add(
            p,
            code::RESULTS_UNVERIFIED,
            None,
            "no manifest supplied: the manifest binding (digest, app, journey ids, platforms, release_critical) cannot be checked",
        );
        return;
    };
    if sha != r.manifest.sha256 {
        add(
            p,
            code::SOURCE_MISMATCH,
            None,
            "the manifest differs from the one the receipt was run against (sha256)",
        );
    }
    if m.app != r.app {
        add(
            p,
            code::SOURCE_MISMATCH,
            None,
            format!("receipt app {:?} but manifest app {:?}", r.app, m.app),
        );
    }
    let mut needs_adapter = false;
    for id in &r.selection.ids {
        match m.journey.iter().find(|j| &j.id == id) {
            None => add(
                p,
                code::SOURCE_MISMATCH,
                oid(id),
                "selected journey is not in the manifest",
            ),
            Some(j) => {
                if !j.platforms.iter().any(|x| x == &r.selection.platform) {
                    add(
                        p,
                        code::SOURCE_MISMATCH,
                        oid(id),
                        "the journey does not declare the selected platform, so it is never selected there",
                    );
                }
                needs_adapter |= j.runner == "adapter";
                if let Some(rj) = r.journeys.iter().find(|x| &x.id == id) {
                    if rj.release_critical != j.release_critical {
                        add(
                            p,
                            code::SOURCE_MISMATCH,
                            oid(id),
                            "release_critical differs from the manifest",
                        );
                    }
                }
            }
        }
    }
    if needs_adapter && !m.adapter.iter().any(|a| a.name == r.adapter.name) {
        add(
            p,
            code::SOURCE_MISMATCH,
            None,
            format!(
                "adapter {:?} is not declared in the manifest",
                r.adapter.name
            ),
        );
    }
}

fn check_evidence(
    r: &Receipt,
    o: &ValidateOptions<'_>,
    p: &mut Vec<Problem>,
    unchecked: &mut Vec<String>,
) {
    let mut skipped = 0usize;
    for j in &r.journeys {
        for e in &j.evidence {
            if !is_safe_relative(&e.path) || !is_hex_lower(&e.sha256, 64) {
                add(
                    p,
                    code::EVIDENCE_MISMATCH,
                    oid(&j.id),
                    format!(
                        "evidence {:?} has an unsafe path or malformed sha256",
                        e.path
                    ),
                );
                continue;
            }
            match o.evidence_root {
                None => skipped += 1,
                Some(root) => match std::fs::read(root.join(&e.path)) {
                    Ok(b) if sha256_hex(&b) == e.sha256 => {}
                    Ok(_) => add(
                        p,
                        code::EVIDENCE_MISMATCH,
                        oid(&j.id),
                        format!("evidence {:?} does not match its recorded sha256", e.path),
                    ),
                    Err(err) => add(
                        p,
                        code::EVIDENCE_MISMATCH,
                        oid(&j.id),
                        format!("evidence {:?} cannot be read: {err}", e.path),
                    ),
                },
            }
        }
    }
    if skipped > 0 || (o.evidence_root.is_none() && !r.journeys.is_empty()) {
        let _ = &unchecked;
        add(
            p,
            code::RESULTS_UNVERIFIED,
            None,
            "no evidence root supplied: evidence hashes cannot be checked",
        );
    }
}

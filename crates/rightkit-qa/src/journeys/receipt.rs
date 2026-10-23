//! `rightkit.journeys.receipt/1` (D5): the receipt types and [`validate_receipt`].
//!
//! [`validate_receipt`] never believes the receipt's `verdict`. It recomputes counts and verdict
//! from the journeys, the provenance bindings, the isolation record, the adapter exit and (when
//! supplied) the adapter's own `results.jsonl`, the manifest and the evidence files. A receipt is
//! accepted only when [`Validation::passed`] is true: no problem found, recomputed verdict
//! `passed` and claimed verdict `passed`. Anything that could not be checked because an input was
//! not supplied is either a problem (`results_unverified`) or listed in `Validation::unchecked`
//! (manifest binding, evidence hashes) and never counted as a pass of that check.
//!
//! # Truthful receipt (ADR-0001 item 6, W2-F2)
//!
//! A receipt that declares `"runner": "rightkit-qa"` must also carry the sections that make it
//! checkable without trusting the runner: the build record binding (`build_record`), the step
//! timing (`timing`, derived from each journey's `steps`), per journey the engine starts, the
//! survivors, the tokens held and the world the scenario ran in (`world`: the actual launch
//! environment and paths, never an assertion), and, when a budget applies, the budget verdicts.
//! [`validate_receipt`] recomputes every one of them from the other fields and from the manifest;
//! a missing section is `truth_missing` (incomplete), a section that disagrees with its inputs is a
//! named failure. A receipt without a `runner` field predates this and is validated as before, but
//! any section it does carry is checked too. A receipt carrying the `explore` marker is refused:
//! an `explore` run records a scenario TOML ([`write_explore_scenario`]) and never a receipt
//! ([`write_receipt`] refuses it too).
use super::adapter::{JourneyRecord, Record, StartRecord};
use super::build_record::{BuildRecord, BUILD_RECORD_SCHEMA};
use super::manifest::Manifest;
use super::{
    code, is_hex_lower, is_rfc3339, is_safe_relative, Evidence, JourneyStatus, Problem, Units,
    PROTOCOL, RECEIPT_SCHEMA,
};
use crate::budget::{self, Day, Limits, Timing};
use crate::engine;
use crate::evidence::CheckStatus;
use crate::util::{err, sha256_hex, Error, Result};
use crate::worker::{EnvMarker, Survivor, Token, World};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

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
    /// Every step the scenario ran, with its wall time. The receipt's `timing` and budget verdicts
    /// are recomputed from these.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<Vec<StepSample>>,
    /// Engine processes started for this scenario, in total and per environment set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_starts: Option<EngineStarts>,
    /// Processes still alive after teardown, recorded before cleanup. Present even when empty; a
    /// passed journey with a survivor is refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub survivors: Option<Vec<Survivor>>,
    /// Machine-wide resource tokens the runner held for this journey. Must equal the tokens the
    /// manifest declares for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_held: Option<Vec<Token>>,
    /// The sandbox the scenario ran in, as the launch actually set it up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub world: Option<JourneyWorld>,
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
    /// Who produced the receipt: `"rightkit-qa"` for this runner. A receipt that names it must carry
    /// every truthful section; one with no `runner` is an older receipt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner: Option<String>,
    /// Version of the runner that wrote the receipt (`CARGO_PKG_VERSION`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_version: Option<String>,
    /// The build record the launched binaries came from; see [`BuildRecordRef`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_record: Option<BuildRecordRef>,
    /// Per step kind p50/p95/max and the ten slowest steps, recomputed from the journeys' `steps`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timing: Option<ReceiptTiming>,
    /// Budget limits applied and the verdicts they gave; `SLOW` is recorded, over budget after the
    /// warn window fails.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<ReceiptBudget>,
    /// The `explore` marker. An `explore` run never produces a receipt, so any receipt carrying the
    /// key is refused, whatever its value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explore: Option<bool>,
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
    } else if p
        .iter()
        .any(|x| code::is_incomplete(&x.code) || truth_code::is_incomplete(&x.code))
    {
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
        check_explore_marker(r, &mut p);
        return finish(r, p, Counts::default(), unchecked);
    }
    check_formats(r, &mut p);
    let counts = check_journeys(r, &mut p);
    check_provenance(r, o, &mut p);
    check_isolation_and_adapter(r, &mut p);
    check_results(r, o, &counts, &mut p);
    check_manifest_binding(r, o, &mut p, &mut unchecked);
    check_evidence(r, o, &mut p, &mut unchecked);
    check_truthful(r, o, &mut p, &mut unchecked);
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
    // The warn window is the manifest's `[budget] warn_until`, not the receipt's own claim: a runner
    // that stretched the window would hide an over-budget step as `slow`.
    if let (Some(decl), Some(b)) = (&m.budget, &r.budget) {
        if decl.warn_until != b.window_end {
            add(
                p,
                truth_code::BUDGET_MISMATCH,
                None,
                format!(
                    "budget.window_end {:?} is not the manifest's [budget] warn_until {:?}",
                    b.window_end, decl.warn_until
                ),
            );
        }
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

// =================================================================================================
// Truthful receipt (ADR-0001 item 6, W2-F2). Everything below is recomputed from other fields of
// the receipt, from the manifest and from the files named in it; a claimed field is never believed.
// =================================================================================================

/// The `runner` value of a receipt written by this crate.
pub const RUNNER: &str = "rightkit-qa";
/// First runner version whose receipts must carry the truthful sections. The `runner` field exists
/// only from this release on, so every receipt that names the runner is held to them; a receipt with
/// no `runner` is an older receipt and is validated as before.
pub const TRUTHFUL_SINCE: (u64, u64, u64) = (0, 8, 0);
/// Number of steps listed in `timing.slowest`.
pub const SLOWEST_STEPS: usize = 10;
/// Step kinds that start an engine again by design (ADR-0001 item 5: the explicit `cold_cli`,
/// `restart` and `kill` steps). Each one is an engine start the scenario declared.
pub const COLD_STEP_KINDS: [&str; 3] = ["cold_cli", "restart", "kill"];

/// Problem codes added by the truthful-receipt rules. The older rules keep their codes in [`code`];
/// the rules of this change are listed together. Rules that reuse an older code (`source_mismatch`,
/// `artifact_mismatch`, `artifact_not_from_build`, `build_record_stale`, `build_not_attested`,
/// `qa_marker_missing`, `isolation`, `host_mismatch`, `evidence_mismatch`) say so at the rule.
pub mod truth_code {
    /// A section a `rightkit-qa` receipt must carry is absent (verdict `incomplete`).
    pub const TRUTH_MISSING: &str = "truth_missing";
    /// A passed journey with a process that survived teardown.
    pub const SURVIVOR: &str = "survivor";
    /// More engine starts than the scenario's environment sets and declared cold launches allow, or
    /// per-environment starts that do not add up.
    pub const ENGINE_STARTS: &str = "engine_starts";
    /// The tokens held differ from the tokens the manifest declares for the journey.
    pub const TOKENS_MISMATCH: &str = "tokens_mismatch";
    /// `timing` differs from what the journeys' steps give.
    pub const TIMING_MISMATCH: &str = "timing_mismatch";
    /// The budget section is malformed or differs from what its limits and the steps give.
    pub const BUDGET_MISMATCH: &str = "budget_mismatch";
    /// A step or journey over its enforcement threshold after the warn window closed.
    pub const OVER_BUDGET: &str = "over_budget";
    /// The control token is not owner-only, was not enforced by the app, or is not the one launched.
    pub const CONTROL_AUTH: &str = "control_auth";
    /// The receipt carries the `explore` marker.
    pub const EXPLORE_RECEIPT: &str = "explore_receipt";

    /// Codes that mean "no usable result" (verdict `incomplete`).
    pub fn is_incomplete(code: &str) -> bool {
        code == TRUTH_MISSING
    }
}

/// The wall time of one executed step. `kind` is the step kind (`cli`, `http`, `engine_log`,
/// `restart`, ...), `step` its label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepSample {
    pub kind: String,
    pub step: String,
    pub ms: u64,
}

/// Nearest-rank percentiles of one step kind, in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KindStats {
    pub p50: u64,
    pub p95: u64,
    pub max: u64,
}

/// One of the slowest steps of the run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlowStep {
    pub step: String,
    pub ms: u64,
}

/// `timing` of the receipt: per step kind p50/p95/max over every journey's steps, and the
/// [`SLOWEST_STEPS`] slowest steps (longest first, ties by label then kind).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptTiming {
    pub per_kind: BTreeMap<String, KindStats>,
    pub slowest: Vec<SlowStep>,
}

/// Nearest rank: the smallest value with at least `pct` percent of the (sorted, non-empty) durations
/// at or below it. The same rule as [`budget::p95`].
fn nearest_rank(sorted: &[u64], pct: usize) -> u64 {
    let rank = (sorted.len() * pct).div_ceil(100).max(1);
    sorted[rank - 1]
}

impl ReceiptTiming {
    /// The timing of `samples`. The runner builds the receipt's `timing` with this and
    /// [`validate_receipt`] recomputes it with the same function, so they cannot drift apart.
    pub fn from_samples<'a>(samples: impl IntoIterator<Item = &'a StepSample>) -> Self {
        let mut by_kind: BTreeMap<&str, Vec<u64>> = BTreeMap::new();
        let mut all: Vec<&StepSample> = vec![];
        for s in samples {
            by_kind.entry(s.kind.as_str()).or_default().push(s.ms);
            all.push(s);
        }
        let per_kind = by_kind
            .into_iter()
            .map(|(kind, mut ms)| {
                ms.sort_unstable();
                (
                    kind.to_string(),
                    KindStats {
                        p50: nearest_rank(&ms, 50),
                        p95: nearest_rank(&ms, 95),
                        max: ms[ms.len() - 1],
                    },
                )
            })
            .collect();
        all.sort_by(|a, b| {
            b.ms.cmp(&a.ms)
                .then_with(|| a.step.cmp(&b.step))
                .then_with(|| a.kind.cmp(&b.kind))
        });
        let slowest = all
            .into_iter()
            .take(SLOWEST_STEPS)
            .map(|s| SlowStep {
                step: s.step.clone(),
                ms: s.ms,
            })
            .collect();
        ReceiptTiming { per_kind, slowest }
    }
}

/// Engine processes started for one scenario (ADR-0001 item 5, Sage C3 b): in total and per
/// environment set (a role's own sidecar). Each environment set may start one engine; every
/// `cold_cli`, `restart` or `kill` step the scenario ran allows one more.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineStarts {
    pub total: u32,
    pub by_env: BTreeMap<String, u32>,
    /// The per-kind counts the scenario's `engine-starts.json` recorded (summed over the journey's
    /// units). When present the allowance is recomputed from these with the rule in
    /// `engine::engine_start_allowance`; no `allowed` figure is read from the receipt. Absent on
    /// receipts written before the counts were carried, which then fall back to the step rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counts: Option<EngineStartCounts>,
}

/// The engine-start fields of one scenario's `engine-starts.json` (`engine::EngineStarts`), kept
/// so the validator can recompute the allowance. Unknown keys (`engine_starts`, `allowed`) are
/// ignored on purpose: the receipt never takes an allowance from the worker.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineStartCounts {
    #[serde(default)]
    pub sidecar_starts: u32,
    #[serde(default)]
    pub cold_launches: u32,
    #[serde(default)]
    pub scenario_sidecars: u32,
    #[serde(default)]
    pub daemon_engines: u32,
    #[serde(default)]
    pub spawned_engines: u32,
    #[serde(default)]
    pub env_sets: u32,
    #[serde(default)]
    pub declared_restarts: u32,
    #[serde(default)]
    pub declared_cold: u32,
    #[serde(default)]
    pub store_lock_reopens: u32,
    /// Cold launches nobody declared; any one is a violation by itself.
    #[serde(default)]
    pub undeclared: Vec<String>,
}

impl EngineStartCounts {
    /// Every start that happened, by the engine's own rule.
    pub fn total(&self) -> u32 {
        engine::engine_start_total(
            self.sidecar_starts,
            self.cold_launches,
            self.scenario_sidecars,
            self.daemon_engines,
            self.spawned_engines,
        )
    }

    /// Every start the scenario declared, by the engine's own rule (`engine::engine_start_allowance`).
    pub fn allowed(&self) -> u32 {
        engine::engine_start_allowance(
            self.env_sets,
            self.declared_restarts,
            self.declared_cold,
            self.store_lock_reopens,
            self.scenario_sidecars,
            self.daemon_engines,
        )
    }

    /// Add another unit's counts to these (a journey may have several units).
    pub fn add(&mut self, other: &EngineStartCounts) {
        self.sidecar_starts += other.sidecar_starts;
        self.cold_launches += other.cold_launches;
        self.scenario_sidecars += other.scenario_sidecars;
        self.daemon_engines += other.daemon_engines;
        self.spawned_engines += other.spawned_engines;
        self.env_sets += other.env_sets;
        self.declared_restarts += other.declared_restarts;
        self.declared_cold += other.declared_cold;
        self.store_lock_reopens += other.store_lock_reopens;
        self.undeclared.extend(other.undeclared.iter().cloned());
    }
}

impl From<&engine::EngineStarts> for EngineStartCounts {
    fn from(s: &engine::EngineStarts) -> Self {
        EngineStartCounts {
            sidecar_starts: s.sidecar_starts,
            cold_launches: s.cold_launches,
            scenario_sidecars: s.scenario_sidecars,
            daemon_engines: s.daemon_engines,
            spawned_engines: s.spawned_engines,
            env_sets: s.env_sets,
            declared_restarts: s.declared_restarts,
            declared_cold: s.declared_cold,
            store_lock_reopens: s.store_lock_reopens,
            undeclared: s.undeclared.clone(),
        }
    }
}

/// The engine-start problem for one journey, if any: the counts' own total and allowance when the
/// receipt carries them, otherwise the step-count fallback. `None` when the journey is within its
/// allowance. Pure, so the validator and its tests share it.
pub fn engine_start_problem(e: &EngineStarts, steps: &[StepSample]) -> Option<String> {
    if let Some(c) = &e.counts {
        let (total, allowed) = (c.total(), c.allowed());
        if u64::from(e.total) != u64::from(total) {
            return Some(format!(
                "engine_starts.total is {} but the recorded starts add up to {total}",
                e.total
            ));
        }
        if !c.undeclared.is_empty() {
            return Some(format!(
                "{total} engine starts include undeclared launch(es): {}",
                c.undeclared.join(" | ")
            ));
        }
        if total > allowed {
            return Some(format!(
                "{total} engine starts; allowed {allowed} ({} environment set(s) plus {} declared restart(s), {} declared cold launch(es), {} store-lock reopen(s), {} scenario sidecar(s), {} daemon(s))",
                c.env_sets, c.declared_restarts, c.declared_cold, c.store_lock_reopens, c.scenario_sidecars, c.daemon_engines
            ));
        }
        return None;
    }
    let allowed = e.allowed(steps);
    if u64::from(e.total) > allowed {
        return Some(format!(
            "{} engine starts; allowed {allowed} ({} environment set(s) plus {} declared cold launch(es); no engine-start counts recorded)",
            e.total,
            e.by_env.len(),
            allowed - e.by_env.len() as u64
        ));
    }
    None
}

impl EngineStarts {
    /// Starts per environment set; the total is their sum.
    pub fn new(by_env: BTreeMap<String, u32>) -> Self {
        let total = by_env.values().fold(0u32, |a, n| a.saturating_add(*n));
        EngineStarts {
            total,
            by_env,
            counts: None,
        }
    }

    /// A scenario that reports only a total (`WorkerOutcome::engine_starts`) has one environment
    /// set, named `default`. A scenario with several environment sets must report them separately,
    /// or its allowance is one set short and it fails closed.
    pub fn from_total(total: u32) -> Self {
        EngineStarts::new(BTreeMap::from([("default".to_string(), total)]))
    }

    /// Attach the recorded per-kind counts (see [`EngineStartCounts`]).
    pub fn with_counts(mut self, counts: EngineStartCounts) -> Self {
        self.counts = Some(counts);
        self
    }

    /// Engine starts the scenario may make: one per environment set plus one per declared cold
    /// launch among its `steps`.
    pub fn allowed(&self, steps: &[StepSample]) -> u64 {
        let extra = steps
            .iter()
            .filter(|s| COLD_STEP_KINDS.contains(&s.kind.as_str()))
            .count() as u64;
        self.by_env.len() as u64 + extra
    }
}

/// How the control token was handled for one launch (D-e).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlAuth {
    /// The token file the launch named; must be the world's `control_token_file`.
    pub token_file: String,
    /// The file was owner-only (read back with `rightkit_fs::private::verify_owner_only`).
    pub owner_only: bool,
    /// A token-less order was sent to the running app and refused (`Control::probe_token_required`).
    pub token_less_refused: bool,
}

/// The sandbox one scenario ran in, recorded from the launch the runner actually made: the world's
/// paths, how the app was told its data directory, and the isolation-relevant environment as set.
/// Every path is text so a receipt can be checked on another host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JourneyWorld {
    pub run_root: String,
    pub dir: String,
    pub data_dir: String,
    pub home: String,
    pub temp: String,
    pub control_socket: String,
    pub control_token_file: String,
    pub port_start: u16,
    pub port_end: u16,
    /// The world marker D-c puts in every process environment of the world.
    pub marker: EnvMarker,
    /// Environment variable through which the app was told its data directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_dir_env: Option<String>,
    /// Flag through which the app was told its data directory (`--data-dir <dir>` or `--data-dir=<dir>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_dir_arg: Option<String>,
    /// The arguments the app was started with (checked for `data_dir_arg`).
    #[serde(default)]
    pub args: Vec<String>,
    /// The launch environment as set, reduced by [`isolation_env`] (isolation keys, `RIGHTKIT_*`
    /// keys and the marker). Never a credential: the launch refuses those.
    pub env: BTreeMap<String, String>,
    /// How the control token was handled; present for every scenario that drove an app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_auth: Option<ControlAuth>,
}

/// What the runner knows about one launch beyond the world: input of [`JourneyWorld::from_world`].
#[derive(Debug, Clone, Default)]
pub struct LaunchFacts<'a> {
    pub data_dir_env: Option<&'a str>,
    pub data_dir_arg: Option<&'a str>,
    pub args: &'a [String],
    /// The full environment the child was given.
    pub env: &'a [(String, String)],
    pub control_auth: Option<ControlAuth>,
}

fn text(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

impl JourneyWorld {
    /// The record of `world` (as written into `scenario.json`) and the launch made in it.
    pub fn from_world(world: &World, run_root: &Path, facts: LaunchFacts<'_>) -> Self {
        JourneyWorld {
            run_root: text(run_root),
            dir: text(&world.dir),
            data_dir: text(&world.data_dir),
            home: text(&world.home),
            temp: text(&world.temp),
            control_socket: text(&world.control_socket),
            control_token_file: text(&world.control_token_file),
            port_start: world.port_start,
            port_end: world.port_end,
            marker: world.env_marker.clone(),
            data_dir_env: facts.data_dir_env.map(str::to_string),
            data_dir_arg: facts.data_dir_arg.map(str::to_string),
            args: facts.args.to_vec(),
            env: isolation_env(facts.env, &world.env_marker.name),
            control_auth: facts.control_auth,
        }
    }
}

/// An environment key that names a directory the app keeps state in: the same rule the launch
/// guard applies (`userdata::engine_dirs_violation`).
fn is_isolation_key(key: &str) -> bool {
    let up = key.to_ascii_uppercase();
    ["DATA", "HOME", "PROFILE", "CONFIG"]
        .iter()
        .any(|w| up.contains(w))
        || matches!(
            up.as_str(),
            "APPDATA" | "LOCALAPPDATA" | "TMPDIR" | "TEMP" | "TMP"
        )
}

/// The part of a launch environment a receipt records: isolation keys, `RIGHTKIT_*` keys and the
/// world marker. The last value of a repeated key wins, as with `Command::env`.
pub fn isolation_env(env: &[(String, String)], marker_name: &str) -> BTreeMap<String, String> {
    env.iter()
        .filter(|(k, _)| k == marker_name || k.starts_with("RIGHTKIT_") || is_isolation_key(k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// The build record the launched binaries came from, summarized for the receipt. The summary is
/// derived from `build-record.json` by [`BuildRecordRef::from_record`]; [`validate_receipt`]
/// re-reads the file (when given the evidence root) and refuses a summary it does not reproduce.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildRecordRef {
    /// `build-record.json`, relative to the evidence root.
    pub path: String,
    /// SHA-256 of the file's exact bytes.
    pub sha256: String,
    pub schema: String,
    pub run_id: String,
    pub app: String,
    pub host_os: String,
    pub host_arch: String,
    pub manifest_sha256: String,
    pub command: Vec<String>,
    pub source_head: String,
    /// The tree before the build.
    pub source_tree: String,
    /// The tree after the build.
    pub source_after_tree: String,
    pub source_dirty: bool,
    pub request_id: String,
    pub request_id_source: String,
    pub exit_code: i32,
    pub build_finished: bool,
    pub attested: bool,
    pub artifacts: Vec<BuildRecordArtifact>,
}

/// One binary of the build record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildRecordArtifact {
    pub role: String,
    pub sha256: String,
    pub size: u64,
    /// The QA control marker was found in the stored copy at build time.
    pub qa_marker: bool,
    /// The role must carry the marker to be launched.
    pub marker_required: bool,
}

impl BuildRecordRef {
    /// Summarize `rec`, which was read from `path` and hashed to `record_sha256`.
    pub fn from_record(rec: &BuildRecord, path: &str, record_sha256: &str) -> Self {
        BuildRecordRef {
            path: path.to_string(),
            sha256: record_sha256.to_string(),
            schema: rec.schema.clone(),
            run_id: rec.run_id.clone(),
            app: rec.app.clone(),
            host_os: rec.host_os.clone(),
            host_arch: rec.host_arch.clone(),
            manifest_sha256: rec.manifest.sha256.clone(),
            command: rec.command.clone(),
            source_head: rec.source.head.clone(),
            source_tree: rec.source.tree.clone(),
            source_after_tree: rec.source_after.tree.clone(),
            source_dirty: rec.source.dirty,
            request_id: rec.broker.request_id.clone(),
            request_id_source: rec.broker.request_id_source.clone(),
            exit_code: rec.broker.exit_code,
            build_finished: rec.broker.build_finished,
            attested: rec.broker.attested,
            artifacts: rec
                .artifacts
                .iter()
                .map(|a| BuildRecordArtifact {
                    role: a.role.clone(),
                    sha256: a.sha256.clone(),
                    size: a.size,
                    qa_marker: a.qa_marker,
                    marker_required: a.marker.required(),
                })
                .collect(),
        }
    }
}

/// A budget threshold pair (milliseconds): over `warn_ms` warns inside the window, over
/// `enforce_ms` fails after it. See [`budget::Limits`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetLimits {
    pub warn_ms: u64,
    pub enforce_ms: u64,
}

impl BudgetLimits {
    fn limits(self) -> Limits {
        Limits {
            warn_ms: self.warn_ms,
            enforce_ms: self.enforce_ms,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetScope {
    Step,
    Journey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetOutcome {
    /// Over the declared budget inside the warn window: recorded, the run still passes.
    Slow,
    /// Over the enforcement threshold after the warn window: a failure.
    OverBudget,
}

/// One timing that was not within budget. Timings within budget are not listed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BudgetEntry {
    pub scope: BudgetScope,
    pub journey: String,
    /// The step kind, or the journey tier for a journey.
    pub kind: String,
    /// The step label, or the journey id.
    pub label: String,
    pub ms: u64,
    pub outcome: BudgetOutcome,
}

/// `budget` of the receipt: the warn window, the limits that were applied and the verdicts they
/// gave. [`validate_receipt`] recomputes the verdicts from the limits, the journeys' steps and
/// durations and the day the run ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptBudget {
    /// Last day (`YYYY-MM-DD`, inclusive) on which exceeding a budget only warns.
    pub window_end: String,
    /// Limits per step kind on this host.
    #[serde(default)]
    pub step_limits: BTreeMap<String, BudgetLimits>,
    /// The per-journey limit for the selection's tier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journey_limit: Option<BudgetLimits>,
    pub verdicts: Vec<BudgetEntry>,
}

/// The day a run ended: the date part of `ended_at`. The verdict of a timing is judged as of this day.
fn run_day(ended_at: &str) -> Result<Day> {
    let date = ended_at
        .get(..10)
        .ok_or_else(|| Error(format!("ended_at {ended_at:?} has no date")))?;
    Day::parse(date)
}

/// Every timing of `journeys` that is not within budget, judged as of `now` against `window_end`.
fn budget_entries(
    window_end: Day,
    now: Day,
    step_limits: &BTreeMap<String, BudgetLimits>,
    journey_limit: Option<&BudgetLimits>,
    tier: &str,
    journeys: &[ReceiptJourney],
) -> Vec<BudgetEntry> {
    let outcome = |l: &BudgetLimits, label: &str, ms: u64| match budget::verdict(
        l.limits(),
        Timing { label, ms },
        now,
        window_end,
    ) {
        budget::Verdict::Ok => None,
        budget::Verdict::Slow { .. } => Some(BudgetOutcome::Slow),
        budget::Verdict::OverBudget { .. } => Some(BudgetOutcome::OverBudget),
    };
    let mut out = vec![];
    for j in journeys {
        for s in j.steps.iter().flatten() {
            if let Some(o) = step_limits
                .get(&s.kind)
                .and_then(|l| outcome(l, &s.step, s.ms))
            {
                out.push(BudgetEntry {
                    scope: BudgetScope::Step,
                    journey: j.id.clone(),
                    kind: s.kind.clone(),
                    label: s.step.clone(),
                    ms: s.ms,
                    outcome: o,
                });
            }
        }
        if let Some(o) = journey_limit.and_then(|l| outcome(l, &j.id, j.duration_ms)) {
            out.push(BudgetEntry {
                scope: BudgetScope::Journey,
                journey: j.id.clone(),
                kind: tier.to_string(),
                label: j.id.clone(),
                ms: j.duration_ms,
                outcome: o,
            });
        }
    }
    out.sort();
    out
}

impl ReceiptBudget {
    /// The budget section for `journeys` that ended at `ended_at`. The runner builds it with this
    /// and [`validate_receipt`] recomputes it with the same function.
    pub fn build(
        window_end: &str,
        step_limits: BTreeMap<String, BudgetLimits>,
        journey_limit: Option<BudgetLimits>,
        tier: &str,
        journeys: &[ReceiptJourney],
        ended_at: &str,
    ) -> Result<Self> {
        let end = Day::parse(window_end)?;
        let now = run_day(ended_at)?;
        let verdicts = budget_entries(
            end,
            now,
            &step_limits,
            journey_limit.as_ref(),
            tier,
            journeys,
        );
        Ok(ReceiptBudget {
            window_end: window_end.to_string(),
            step_limits,
            journey_limit,
            verdicts,
        })
    }
}

/// The journey status a check status becomes. Only `Passed` passes; `Errored` stays `errored`, and
/// `NotRun` (wire name `missing`) is `Missing`, which a selected journey cannot be and still pass.
pub fn journey_status_of(s: CheckStatus) -> JourneyStatus {
    match s {
        CheckStatus::Passed => JourneyStatus::Passed,
        CheckStatus::Failed => JourneyStatus::Failed,
        CheckStatus::Skipped => JourneyStatus::Skipped,
        CheckStatus::Errored => JourneyStatus::Errored,
        CheckStatus::NotRun => JourneyStatus::Missing,
    }
}

// ------------------------------------------------------------------------------------ checking

fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let core = s.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let v = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(v)
}

/// The receipt names this runner at or after [`TRUTHFUL_SINCE`], or at a version that cannot be
/// read (fail closed): the truthful sections are required.
fn truthful_required(r: &Receipt) -> bool {
    r.runner.as_deref() == Some(RUNNER)
        && r.runner_version
            .as_deref()
            .and_then(parse_version)
            .is_none_or(|v| v >= TRUTHFUL_SINCE)
}

fn check_explore_marker(r: &Receipt, p: &mut Vec<Problem>) {
    if r.explore.is_some() {
        add(
            p,
            truth_code::EXPLORE_RECEIPT,
            None,
            "the receipt carries an explore marker; an explore run records a scenario and never a receipt",
        );
    }
}

fn check_truthful(
    r: &Receipt,
    o: &ValidateOptions<'_>,
    p: &mut Vec<Problem>,
    unchecked: &mut Vec<String>,
) {
    check_explore_marker(r, p);
    let required = truthful_required(r);
    if required {
        check_sections_present(r, o, p);
    }
    check_build_record(r, o, p);
    check_timing(r, p);
    let window_bound = o.manifest.is_some_and(|(m, _)| m.budget.is_some());
    check_budget(r, p, unchecked, required, window_bound);
    check_journey_truth(r, o, p);
    check_worlds(r, o, p, required);
}

/// Every section a `rightkit-qa` receipt must carry. A journey that never ran (`missing`) has none.
/// A journey the manifest runs through an adapter (`runner = "adapter"`) executes inside the
/// adapter's own process: the runner cannot see its steps or its engine starts, so those two are
/// not required of it (survivors, tokens and the world still are).
fn check_sections_present(r: &Receipt, o: &ValidateOptions<'_>, p: &mut Vec<Problem>) {
    let mut miss = |id: Option<&str>, what: &str| {
        add(
            p,
            truth_code::TRUTH_MISSING,
            id,
            format!("a {RUNNER} receipt must carry {what}"),
        )
    };
    if r.runner_version
        .as_deref()
        .and_then(parse_version)
        .is_none()
    {
        miss(None, "a runner_version (x.y.z)");
    }
    if r.build_record.is_none() {
        miss(None, "build_record");
    }
    if r.timing.is_none() {
        miss(None, "timing");
    }
    for j in r
        .journeys
        .iter()
        .filter(|j| j.status != JourneyStatus::Missing)
    {
        let id = Some(j.id.as_str());
        let adapter_run = o
            .manifest
            .and_then(|(m, _)| m.journey.iter().find(|x| x.id == j.id))
            .is_some_and(|x| x.runner == "adapter");
        if j.steps.is_none() && !adapter_run {
            miss(id, "steps");
        }
        if j.engine_starts.is_none() && !adapter_run {
            miss(id, "engine_starts");
        }
        if j.survivors.is_none() {
            miss(id, "survivors (an empty list when none)");
        }
        if j.tokens_held.is_none() {
            miss(id, "tokens_held (an empty list when none)");
        }
        if j.world.is_none() {
            miss(id, "world (the launch the scenario ran in)");
        }
    }
}

/// The build binding: the record the launched binaries came from must be attested, match the
/// receipt's source, manifest, command, host and artifacts, and (given the evidence root) be the
/// file the summary says it is.
fn check_build_record(r: &Receipt, o: &ValidateOptions<'_>, p: &mut Vec<Problem>) {
    let Some(b) = &r.build_record else { return };
    if b.schema != BUILD_RECORD_SCHEMA {
        add(
            p,
            code::SCHEMA,
            None,
            format!(
                "build_record.schema is {:?}, expected {BUILD_RECORD_SCHEMA:?}",
                b.schema
            ),
        );
    }
    let hex40 = |s: &str| is_hex_lower(s, 40);
    let hex64 = |s: &str| is_hex_lower(s, 64);
    if !hex64(&b.sha256)
        || !hex64(&b.manifest_sha256)
        || !hex40(&b.source_head)
        || !hex40(&b.source_tree)
        || !hex40(&b.source_after_tree)
        || b.artifacts
            .iter()
            .any(|a| a.role.trim().is_empty() || !hex64(&a.sha256))
    {
        add(
            p,
            code::SCHEMA,
            None,
            "malformed build_record (a digest, tree or artifact role)",
        );
    }
    if !b.attested
        || b.exit_code != 0
        || !b.build_finished
        || b.request_id.trim().is_empty()
        || b.request_id_source.trim().is_empty()
    {
        add(
            p,
            code::BUILD_NOT_ATTESTED,
            None,
            "the build record does not attest an exit-0, build-finished broker build with a request id",
        );
    }
    if b.source_tree != r.source.tree
        || b.source_after_tree != r.source.tree
        || b.source_head != r.source.head
        || b.source_dirty != r.source.dirty
        || b.source_tree != r.build.source_tree
        || b.source_after_tree != r.build.tree_after_build
    {
        add(
            p,
            code::SOURCE_MISMATCH,
            None,
            "the build record's source (head, tree before and after, dirty) is not the receipt's source",
        );
    }
    if b.manifest_sha256 != r.manifest.sha256 {
        add(
            p,
            code::BUILD_RECORD_STALE,
            None,
            "the manifest changed since the build: the record was built against another journeys.toml",
        );
    }
    if b.command != r.build.command {
        add(
            p,
            code::BUILD_RECORD_STALE,
            None,
            format!(
                "the receipt's build command {:?} is not the record's {:?}",
                r.build.command, b.command
            ),
        );
    }
    if b.app != r.app {
        add(
            p,
            code::SOURCE_MISMATCH,
            None,
            format!(
                "build record is for app {:?}, the receipt for {:?}",
                b.app, r.app
            ),
        );
    }
    if b.host_os != r.host.os || b.host_arch != r.host.arch {
        add(
            p,
            code::HOST_MISMATCH,
            None,
            format!(
                "the binaries were built for {}/{} but the receipt's host is {}/{}",
                b.host_os, b.host_arch, r.host.os, r.host.arch
            ),
        );
    }
    for a in &r.build.artifacts {
        match b.artifacts.iter().find(|x| x.role == a.role) {
            None => add(
                p,
                code::ARTIFACT_NOT_FROM_BUILD,
                oid(&a.role),
                "the receipt's build lists an artifact the build record does not hold",
            ),
            Some(x) if x.sha256 != a.sha256 || x.size != a.size => add(
                p,
                code::ARTIFACT_MISMATCH,
                oid(&a.role),
                "the receipt's build artifact differs from the build record's (sha256 or size)",
            ),
            Some(_) => {}
        }
    }
    for x in &b.artifacts {
        if !r.build.artifacts.iter().any(|a| a.role == x.role) {
            add(
                p,
                code::ARTIFACT_MISMATCH,
                oid(&x.role),
                "the build record holds an artifact the receipt's build lacks",
            );
        }
    }
    for l in &r.launched {
        match b.artifacts.iter().find(|x| x.role == l.role) {
            None => add(
                p,
                code::ARTIFACT_NOT_FROM_BUILD,
                oid(&l.role),
                "launched role is not in the build record",
            ),
            Some(x) if x.sha256 != l.sha256 => add(
                p,
                code::ARTIFACT_NOT_FROM_BUILD,
                oid(&l.role),
                "launched digest differs from the build record's artifact digest",
            ),
            Some(x) if x.marker_required && !x.qa_marker => add(
                p,
                code::QA_MARKER_MISSING,
                oid(&l.role),
                "launched binary lacks the QA control marker its role requires",
            ),
            Some(_) => {}
        }
    }
    let Some(root) = o.evidence_root else { return };
    if !is_safe_relative(&b.path) {
        add(
            p,
            code::EVIDENCE_MISMATCH,
            None,
            format!("build_record.path {:?} is unsafe", b.path),
        );
        return;
    }
    let bytes = match std::fs::read(root.join(&b.path)) {
        Ok(x) => x,
        Err(e) => {
            add(
                p,
                code::EVIDENCE_MISMATCH,
                None,
                format!("the build record {:?} cannot be read: {e}", b.path),
            );
            return;
        }
    };
    if sha256_hex(&bytes) != b.sha256 {
        add(
            p,
            code::EVIDENCE_MISMATCH,
            None,
            format!(
                "the build record {:?} does not match build_record.sha256",
                b.path
            ),
        );
        return;
    }
    let rec = serde_json::from_slice::<BuildRecord>(&bytes)
        .map_err(|e| e.to_string())
        .and_then(|rec| rec.validate().map(|_| rec).map_err(|e| e.0));
    match rec {
        Err(e) => add(
            p,
            code::BUILD_NOT_ATTESTED,
            None,
            format!("the build record file is not a valid attested record: {e}"),
        ),
        Ok(rec) => {
            if BuildRecordRef::from_record(&rec, &b.path, &b.sha256) != *b {
                add(
                    p,
                    code::BUILD_RECORD_STALE,
                    None,
                    "build_record differs from what the build record file says",
                );
            }
        }
    }
}

fn timing_difference(claimed: &ReceiptTiming, want: &ReceiptTiming) -> String {
    for (kind, w) in &want.per_kind {
        match claimed.per_kind.get(kind) {
            None => return format!("step kind {kind:?} is missing from timing.per_kind"),
            Some(c) if c != w => {
                return format!(
                    "timing.per_kind[{kind:?}] claims p50 {} p95 {} max {}, the steps give p50 {} p95 {} max {}",
                    c.p50, c.p95, c.max, w.p50, w.p95, w.max
                )
            }
            Some(_) => {}
        }
    }
    if let Some(kind) = claimed
        .per_kind
        .keys()
        .find(|k| !want.per_kind.contains_key(*k))
    {
        return format!("timing.per_kind lists step kind {kind:?} that no step has");
    }
    "timing.slowest is not the slowest steps of the journeys".to_string()
}

fn check_timing(r: &Receipt, p: &mut Vec<Problem>) {
    let Some(t) = &r.timing else { return };
    let want =
        ReceiptTiming::from_samples(r.journeys.iter().flat_map(|j| j.steps.iter().flatten()));
    if *t != want {
        add(
            p,
            truth_code::TIMING_MISMATCH,
            None,
            timing_difference(t, &want),
        );
    }
}

fn check_budget(
    r: &Receipt,
    p: &mut Vec<Problem>,
    unchecked: &mut Vec<String>,
    required: bool,
    window_bound: bool,
) {
    let Some(b) = &r.budget else {
        if required {
            unchecked.push(
                "budget: the receipt carries no budget section and the manifest's [budget] table cannot be read here, so a skipped budget is not detected"
                    .to_string(),
            );
        }
        return;
    };
    let end = match Day::parse(&b.window_end) {
        Ok(d) => d,
        Err(e) => {
            add(
                p,
                truth_code::BUDGET_MISMATCH,
                None,
                format!("budget.window_end: {}", e.0),
            );
            return;
        }
    };
    let now = match run_day(&r.ended_at) {
        Ok(d) => d,
        Err(e) => {
            add(
                p,
                truth_code::BUDGET_MISMATCH,
                None,
                format!("budget cannot be judged: {}", e.0),
            );
            return;
        }
    };
    let zero = b
        .step_limits
        .values()
        .chain(b.journey_limit.iter())
        .any(|l| l.warn_ms == 0 || l.enforce_ms == 0);
    if zero {
        add(
            p,
            truth_code::BUDGET_MISMATCH,
            None,
            "a budget limit is zero",
        );
        return;
    }
    let want = budget_entries(
        end,
        now,
        &b.step_limits,
        b.journey_limit.as_ref(),
        &r.selection.tier,
        &r.journeys,
    );
    let mut claimed = b.verdicts.clone();
    claimed.sort();
    if claimed != want {
        add(
            p,
            truth_code::BUDGET_MISMATCH,
            None,
            format!(
                "budget.verdicts lists {} entr{} but the limits and the journeys' timings give {}",
                claimed.len(),
                if claimed.len() == 1 { "y" } else { "ies" },
                want.len()
            ),
        );
    }
    // The failures come from the recomputed entries, never from the claimed ones.
    for e in want
        .iter()
        .filter(|e| e.outcome == BudgetOutcome::OverBudget)
    {
        let limit = match e.scope {
            BudgetScope::Step => b.step_limits.get(&e.kind),
            BudgetScope::Journey => b.journey_limit.as_ref(),
        };
        add(
            p,
            truth_code::OVER_BUDGET,
            oid(&e.journey),
            match limit {
                Some(l) => budget::Verdict::OverBudget {
                    step: e.label.clone(),
                    ms: e.ms,
                    budget: l.enforce_ms,
                }
                .to_string(),
                None => format!("OVER BUDGET {}: {} ms", e.label, e.ms),
            },
        );
    }
    if !window_bound {
        unchecked.push(
            "budget.window_end is the receipt's own claim: no manifest with a [budget] table was supplied, so the window is not bound to journeys.toml"
                .to_string(),
        );
    }
}

fn token_name(t: Token) -> String {
    serde_json::to_value(t)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// Per journey: survivors, engine starts and tokens held.
fn check_journey_truth(r: &Receipt, o: &ValidateOptions<'_>, p: &mut Vec<Problem>) {
    for j in &r.journeys {
        let id = Some(j.id.as_str());
        if let Some(sv) = &j.survivors {
            if !sv.is_empty() && j.status == JourneyStatus::Passed {
                add(
                    p,
                    truth_code::SURVIVOR,
                    id,
                    format!(
                        "journey is passed but {} process(es) survived teardown ({}); any survivor fails the scenario",
                        sv.len(),
                        sv.iter()
                            .map(|s| format!("{} {}", s.pid, s.name))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                );
            }
        }
        if let Some(e) = &j.engine_starts {
            let sum = e.by_env.values().map(|n| u64::from(*n)).sum::<u64>();
            if sum != u64::from(e.total) {
                add(
                    p,
                    truth_code::ENGINE_STARTS,
                    id,
                    format!(
                        "engine_starts.total is {} but the per-environment starts add up to {sum}",
                        e.total
                    ),
                );
            }
            let steps: &[StepSample] = j.steps.as_deref().unwrap_or(&[]);
            if let Some(problem) = engine_start_problem(e, steps) {
                add(p, truth_code::ENGINE_STARTS, id, problem);
            }
        }
        if let Some(held) = &j.tokens_held {
            let mut seen = BTreeSet::new();
            for t in held {
                if !seen.insert(*t) {
                    add(
                        p,
                        truth_code::TOKENS_MISMATCH,
                        id,
                        format!("token {} is held twice", token_name(*t)),
                    );
                }
            }
            let held_names: BTreeSet<String> = seen.iter().map(|t| token_name(*t)).collect();
            let declared = o
                .manifest
                .and_then(|(m, _)| m.journey.iter().find(|x| x.id == j.id))
                .map(|x| x.tokens.iter().cloned().collect::<BTreeSet<String>>());
            if let Some(declared) = declared {
                for t in held_names.difference(&declared) {
                    add(
                        p,
                        truth_code::TOKENS_MISMATCH,
                        id,
                        format!("token {t} was held but the manifest does not declare it for this journey"),
                    );
                }
                for t in declared.difference(&held_names) {
                    add(
                        p,
                        truth_code::TOKENS_MISMATCH,
                        id,
                        format!("the manifest declares token {t} for this journey but the runner did not hold it"),
                    );
                }
            }
        }
    }
}

// ------------------------------------------------------------------------------- paths in text

fn looks_windows(s: &str) -> bool {
    s.contains('\\') || s.as_bytes().get(1) == Some(&b':')
}

/// The components of an absolute path written as text (either separator, a drive or `\\?\`
/// prefix), case-folded when `fold`. `None` for a relative path or one with a `.` or `..`.
fn components(s: &str, fold: bool) -> Option<Vec<String>> {
    let unified = s.replace('\\', "/");
    let t = unified.strip_prefix("//?/").unwrap_or(&unified);
    let b = t.as_bytes();
    let absolute = t.starts_with('/') || (b.len() >= 3 && b[1] == b':' && b[2] == b'/');
    if !absolute {
        return None;
    }
    let mut out = vec![];
    for c in t.split('/') {
        if c.is_empty() {
            continue;
        }
        if c == "." || c == ".." {
            return None;
        }
        out.push(if fold {
            c.to_lowercase()
        } else {
            c.to_string()
        });
    }
    Some(out)
}

/// `path` lies strictly under `root`. Pure text; nothing is read from disk.
fn path_under(root: &str, path: &str) -> bool {
    let fold = looks_windows(root) || looks_windows(path);
    match (components(root, fold), components(path, fold)) {
        (Some(r), Some(p)) => !r.is_empty() && p.len() > r.len() && p[..r.len()] == r[..],
        _ => false,
    }
}

fn same_path(a: &str, b: &str) -> bool {
    let fold = looks_windows(a) || looks_windows(b);
    match (components(a, fold), components(b, fold)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// Value of the last `flag <value>` / `flag=<value>` in `args`.
fn arg_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let mut found = None;
    for (i, a) in args.iter().enumerate() {
        if a == flag {
            found = args.get(i + 1).map(String::as_str);
        } else if let Some(v) = a.strip_prefix(flag).and_then(|r| r.strip_prefix('=')) {
            found = Some(v);
        }
    }
    found
}

/// Isolation from the launch the runner actually made: every world path under the run root, the
/// data directory the app was told is the one recorded, the isolation-relevant environment points
/// into the run root, no two scenarios share a world, and the control token is owner-only and
/// enforced.
fn check_worlds(r: &Receipt, o: &ValidateOptions<'_>, p: &mut Vec<Problem>, required: bool) {
    let mut markers: BTreeSet<&str> = BTreeSet::new();
    let mut data_dirs: Vec<(&str, &str)> = vec![];
    let mut roots: Vec<&str> = vec![];
    for j in &r.journeys {
        let id = Some(j.id.as_str());
        let manifest_surface = o
            .manifest
            .and_then(|(m, _)| m.journey.iter().find(|x| x.id == j.id))
            .map(|x| x.surface.as_str());
        let Some(w) = &j.world else {
            continue;
        };
        if !roots.iter().any(|x| same_path(x, &w.run_root)) {
            roots.push(w.run_root.as_str());
        }
        if components(&w.run_root, looks_windows(&w.run_root)).is_none_or(|c| c.is_empty()) {
            add(
                p,
                code::ISOLATION,
                id,
                format!("world.run_root {:?} is not an absolute path", w.run_root),
            );
            continue;
        }
        for (label, path) in [
            ("dir", &w.dir),
            ("data_dir", &w.data_dir),
            ("home", &w.home),
            ("temp", &w.temp),
            ("control_socket", &w.control_socket),
            ("control_token_file", &w.control_token_file),
        ] {
            if !path_under(&w.run_root, path) {
                add(
                    p,
                    code::ISOLATION,
                    id,
                    format!(
                        "world.{label} {path:?} is not under the run root {:?}",
                        w.run_root
                    ),
                );
            }
        }
        if w.port_start < crate::worker::MIN_WORLD_PORT || w.port_start > w.port_end {
            add(
                p,
                code::ISOLATION,
                id,
                format!(
                    "world port range {}-{} is invalid",
                    w.port_start, w.port_end
                ),
            );
        }
        if w.marker.name.is_empty() || w.marker.value.trim().is_empty() {
            add(p, code::ISOLATION, id, "world marker is empty");
        } else {
            if w.env.get(&w.marker.name) != Some(&w.marker.value) {
                add(
                    p,
                    code::ISOLATION,
                    id,
                    format!(
                        "the world marker {} is not in the launch environment the receipt records",
                        w.marker.name
                    ),
                );
            }
            if !markers.insert(w.marker.value.as_str()) {
                add(
                    p,
                    code::ISOLATION,
                    id,
                    "two scenarios carry the same world marker: they shared a world",
                );
            }
        }
        if let Some((_, other)) = data_dirs.iter().find(|(d, _)| same_path(d, &w.data_dir)) {
            add(
                p,
                code::ISOLATION,
                id,
                format!("scenario shares its data directory with journey {other}"),
            );
        }
        data_dirs.push((w.data_dir.as_str(), j.id.as_str()));
        let mut told = false;
        if let Some(name) = &w.data_dir_env {
            told = true;
            match w.env.get(name) {
                Some(v) if same_path(v, &w.data_dir) => {}
                Some(v) => add(
                    p,
                    code::ISOLATION,
                    id,
                    format!("the app was told its data directory through {name}={v:?}, not the recorded {:?}", w.data_dir),
                ),
                None => add(
                    p,
                    code::ISOLATION,
                    id,
                    format!("the launch environment has no {name}, the variable the data directory went through"),
                ),
            }
        }
        if let Some(flag) = &w.data_dir_arg {
            told = true;
            match arg_value(&w.args, flag) {
                Some(v) if same_path(v, &w.data_dir) => {}
                Some(v) => add(
                    p,
                    code::ISOLATION,
                    id,
                    format!("the app was started with {flag} {v:?}, not the recorded data directory {:?}", w.data_dir),
                ),
                None => add(
                    p,
                    code::ISOLATION,
                    id,
                    format!("the app's arguments do not carry {flag}"),
                ),
            }
        }
        if !told {
            add(
                p,
                code::ISOLATION,
                id,
                "no record of how the app was told its data directory (data_dir_env or data_dir_arg)",
            );
        }
        for (k, v) in &w.env {
            let names_a_dir = is_isolation_key(k)
                || k == "RIGHTKIT_SUITE_ROOT"
                || k == crate::control::token::ENV_AUTH_FILE;
            if names_a_dir
                && components(v, looks_windows(v)).is_some()
                && !path_under(&w.run_root, v)
            {
                add(
                    p,
                    code::ISOLATION,
                    id,
                    format!(
                        "launch environment {k}={v:?} points outside the run root {:?}",
                        w.run_root
                    ),
                );
            }
        }
        match &w.control_auth {
            Some(a) => {
                if !same_path(&a.token_file, &w.control_token_file) {
                    add(
                        p,
                        truth_code::CONTROL_AUTH,
                        id,
                        "the control token file is not the world's control_token_file",
                    );
                }
                if !a.owner_only {
                    add(
                        p,
                        truth_code::CONTROL_AUTH,
                        id,
                        "the control token file was not owner-only",
                    );
                }
                if !a.token_less_refused {
                    add(
                        p,
                        truth_code::CONTROL_AUTH,
                        id,
                        "a token-less control order was not shown to be refused: the app does not enforce the per-run token (it predates the check and must re-pin rightkit-qa)",
                    );
                }
                match w.env.get(crate::control::token::ENV_AUTH_FILE) {
                    Some(v) if same_path(v, &a.token_file) => {}
                    _ => add(
                        p,
                        truth_code::CONTROL_AUTH,
                        id,
                        format!(
                            "the launch environment does not name the token file in {}",
                            crate::control::token::ENV_AUTH_FILE
                        ),
                    ),
                }
            }
            None => {
                if required
                    && j.status != JourneyStatus::Missing
                    && manifest_surface == Some("native-ui")
                {
                    add(
                        p,
                        truth_code::CONTROL_AUTH,
                        id,
                        "a native-ui journey drives the app's control server: its world must record the control token handling",
                    );
                }
            }
        }
    }
    if roots.len() > 1 {
        add(
            p,
            code::ISOLATION,
            None,
            "the journeys record different run roots",
        );
    }
    if let Some(root) = roots.first() {
        if !r.isolation.data_dir.trim().is_empty() && !path_under(root, &r.isolation.data_dir) {
            add(
                p,
                code::ISOLATION,
                None,
                format!(
                    "isolation.data_dir {:?} is not under the run root {root:?} the launches record",
                    r.isolation.data_dir
                ),
            );
        }
    }
}

// ------------------------------------------------------------------------ writing and explore

fn write_whole(path: &Path, bytes: &[u8]) -> Result<()> {
    rightkit_fs::atomic_write(path, bytes).map_err(|e| {
        Error(format!(
            "cannot write {}: {}",
            path.display(),
            std::io::Error::from(e)
        ))
    })
}

/// Write `receipt` as JSON, atomically. A receipt carrying the `explore` marker is refused: an
/// `explore` run records a scenario ([`write_explore_scenario`]) and never a receipt.
pub fn write_receipt(path: &Path, receipt: &Receipt) -> Result<()> {
    if receipt.explore.is_some() {
        return err("an explore run never writes a receipt; it records a scenario TOML instead");
    }
    let mut bytes = serde_json::to_vec_pretty(receipt)?;
    bytes.push(b'\n');
    write_whole(path, &bytes)
}

/// One executed step of an `explore` run: the step kind, its label and its parameters as the step
/// was run. `params` holds the step's own keys (everything but `kind` and `label`).
#[derive(Debug, Clone, PartialEq)]
pub struct ExploreStep {
    pub kind: String,
    pub label: Option<String>,
    pub params: BTreeMap<String, serde_json::Value>,
}

fn json_to_toml(v: &serde_json::Value, at: &str) -> Result<toml::Value> {
    use serde_json::Value as J;
    Ok(match v {
        J::Null => {
            return err(format!(
                "{at}: TOML has no null; the step cannot be recorded"
            ))
        }
        J::Bool(b) => toml::Value::Boolean(*b),
        J::Number(n) => {
            if let Some(i) = n.as_i64() {
                toml::Value::Integer(i)
            } else if let Some(f) = n.as_f64().filter(|f| n.is_f64() && f.is_finite()) {
                toml::Value::Float(f)
            } else {
                return err(format!(
                    "{at}: number {n} does not fit a TOML integer or float"
                ));
            }
        }
        J::String(s) => toml::Value::String(s.clone()),
        J::Array(a) => toml::Value::Array(
            a.iter()
                .enumerate()
                .map(|(i, x)| json_to_toml(x, &format!("{at}[{i}]")))
                .collect::<Result<Vec<_>>>()?,
        ),
        J::Object(m) => {
            let mut t = toml::Table::new();
            for (k, x) in m {
                t.insert(k.clone(), json_to_toml(x, &format!("{at}.{k}"))?);
            }
            toml::Value::Table(t)
        }
    })
}

/// The scenario TOML an `explore` run records from its executed steps. `tokens` are the resource
/// tokens the run held (written as the scenario's `tokens`, so the committed scenario declares
/// them). The result is not evidence: it must be reviewed and committed, and only a run of the
/// committed file produces a receipt.
pub fn explore_scenario_toml(
    name: &str,
    target: &str,
    tokens: &[String],
    steps: &[ExploreStep],
) -> Result<String> {
    let name_ok = !name.is_empty()
        && name.len() <= 64
        && name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if !name_ok {
        return err(format!(
            "explore scenario name {name:?} must be 1-64 characters of a-z, 0-9, '-' and '_'"
        ));
    }
    if !["engine", "ui", "browser"].contains(&target) {
        return err(format!(
            "explore target {target:?} must be engine, ui or browser"
        ));
    }
    if steps.is_empty() {
        return err("an explore run that executed no step records nothing");
    }
    // Written by hand: scalars first, then one `[[step]]` table per executed step, so no key can
    // land inside the wrong table. Values go through `toml::Value`'s inline form.
    let value = |v: toml::Value| v.to_string();
    let key = |k: &str| {
        if !k.is_empty()
            && k.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            k.to_string()
        } else {
            value(toml::Value::String(k.to_string()))
        }
    };
    let mut out = String::from(
        "# Recorded by an explore run. Not evidence: review it and commit it; only a run of a committed scenario file produces a receipt.\n",
    );
    out.push_str(&format!(
        "name = {}\ntarget = {}\ntier = \"fast\"\n",
        value(toml::Value::String(name.to_string())),
        value(toml::Value::String(target.to_string())),
    ));
    if !tokens.is_empty() {
        let list = tokens
            .iter()
            .map(|t| value(toml::Value::String(t.clone())))
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!("tokens = [{list}]\n"));
    }
    for (i, st) in steps.iter().enumerate() {
        if st.kind.trim().is_empty() {
            return err(format!("explore step {i} has no kind"));
        }
        out.push_str("\n[[step]]\n");
        out.push_str(&format!(
            "kind = {}\n",
            value(toml::Value::String(st.kind.clone()))
        ));
        if let Some(l) = &st.label {
            out.push_str(&format!(
                "label = {}\n",
                value(toml::Value::String(l.clone()))
            ));
        }
        for (k, v) in &st.params {
            if k == "kind" || k == "label" {
                return err(format!(
                    "explore step {i}: parameter {k:?} collides with the step's own key"
                ));
            }
            out.push_str(&format!(
                "{} = {}\n",
                key(k),
                value(json_to_toml(v, &format!("step {i} {k}"))?)
            ));
        }
    }
    Ok(out)
}

/// Write `<dir>/<name>.scenario.toml` from an `explore` run's executed steps and check that the
/// scenario loader reads it back with the same steps. An existing file is never overwritten.
/// This is the only output of an `explore` run; it writes no receipt.
pub fn write_explore_scenario(
    dir: &Path,
    name: &str,
    target: &str,
    tokens: &[String],
    steps: &[ExploreStep],
) -> Result<PathBuf> {
    let toml_text = explore_scenario_toml(name, target, tokens, steps)?;
    let path = dir.join(format!("{name}.scenario.toml"));
    if path.exists() {
        return err(format!(
            "{} already exists; an explore run does not overwrite a scenario",
            path.display()
        ));
    }
    std::fs::create_dir_all(dir)?;
    write_whole(&path, toml_text.as_bytes())?;
    let loaded = crate::scenario::Scenario::load(&path).and_then(|s| {
        if s.steps.len() == steps.len()
            && s.steps
                .iter()
                .zip(steps)
                .all(|(a, b)| a.kind == b.kind && a.label == b.label)
        {
            Ok(())
        } else {
            err("the recorded scenario does not read back with the executed steps")
        }
    });
    if let Err(e) = loaded {
        let _ = std::fs::remove_file(&path);
        return err(format!("the recorded scenario is not loadable: {}", e.0));
    }
    Ok(path)
}

#[cfg(test)]
mod engine_start_tests {
    use super::*;

    /// The `engine-starts.json` of PostRight PR-076 (store backup and restore), compact.
    const PR_076: &str = r##"{"allowed":23,"by_env_set":{"#1 default":23},"cold_launches":17,"cold_reasons":{"store-lock":17},"crashes":[],"daemon_engines":0,"declared_cold":17,"declared_restarts":0,"engine_starts":23,"env_sets":1,"plugin_declared":[],"plugin_declared_cold":0,"scenario_sidecars":0,"sidecar_starts":6,"spawned_engines":0,"store_lock_reopens":5,"undeclared":[]}"##;

    fn journey_of(counts: EngineStartCounts, total: u32) -> EngineStarts {
        EngineStarts {
            total,
            by_env: BTreeMap::from([("#1 default".to_string(), total)]),
            counts: Some(counts),
        }
    }

    #[test]
    fn the_pr_076_shape_validates() {
        let c: EngineStartCounts = serde_json::from_str(PR_076).unwrap();
        assert_eq!((c.total(), c.allowed()), (23, 23));
        assert_eq!(engine_start_problem(&journey_of(c, 23), &[]), None);
    }

    #[test]
    fn an_inflated_allowed_does_not_buy_an_extra_start() {
        // The worker says allowed 99 and records one more sidecar start: 24 starts, 23 allowed.
        let text = PR_076
            .replace("\"allowed\":23", "\"allowed\":99")
            .replace("\"sidecar_starts\":6", "\"sidecar_starts\":7");
        let c: EngineStartCounts = serde_json::from_str(&text).unwrap();
        let p = engine_start_problem(&journey_of(c, 24), &[]).expect("24 starts over 23 allowed");
        assert!(p.starts_with("24 engine starts; allowed 23"), "{p}");
    }

    #[test]
    fn an_undeclared_launch_is_refused_even_when_the_counts_fit() {
        let text = PR_076.replace("\"undeclared\":[]", "\"undeclared\":[\"step 2: cold\"]");
        let c: EngineStartCounts = serde_json::from_str(&text).unwrap();
        let p = engine_start_problem(&journey_of(c, 23), &[]).expect("undeclared launch");
        assert!(p.contains("undeclared"), "{p}");
    }

    #[test]
    fn a_receipt_without_counts_keeps_the_step_rule() {
        assert!(engine_start_problem(&EngineStarts::from_total(2), &[]).is_some());
        assert_eq!(
            engine_start_problem(&EngineStarts::from_total(1), &[]),
            None
        );
    }
}

//! `rightkit-qa journeys run`: select, record source identity, build in the broker (steps 1 to 3,
//! [`prepare`]), then launch, collect and write the receipt (steps 4 and 5, [`run_journeys`]).
//!
//! * **Launch and collect (step 4).** [`run_with`] re-measures the checkout against the build record,
//!   then runs every selected scenario in its own supervised worker ([`supervise::run_worker`]) in
//!   the pool: one world per scenario, the manifest's tokens held, a control token file written per
//!   world, every binary of the record re-hashed before each launch and again after the run. A
//!   `runner = "adapter"` journey runs through the Harness bridge over the recorded libtest binary.
//!   A Windows process outside the interactive session is refused, never given a pass.
//! * **Receipt (step 5).** The receipt is built from what the run observed: the worker's step
//!   samples, engine starts per environment set, survivors and launch facts, the runner's own
//!   record of tokens and worlds, the build record and the manifest's `[budget]`. The verdict is
//!   recomputed by [`validate_receipt`], the file is validated again from disk, and the run is a pass
//!   only if that validation passes. An `explore` run records a scenario and writes no receipt.
//!
//! Steps 1 to 3 in detail:
//!
//! * **Select (step 1).** [`select_journeys`] filters the manifest by id, platform and tier before
//!   anything is built or created. An id that does not declare the platform is an error, never a
//!   quiet drop: skipping is not a way to meet a platform requirement. The required ids per
//!   platform are frozen in `required-journeys.json` next to `selection.json`.
//! * **Source identity and build (steps 2 and 3).** [`prepare`] measures the checkout, runs the
//!   adapter's `build` argv through `rustright cargo` (validated compile-only, see
//!   [`build_record::validate_build_command`]), measures the checkout again and writes the record
//!   ([`build_record::record_build`]). The runner owns no target directory: the copies in the run
//!   store are all it keeps.
//! * **Qualifying mode.** A qualifying run (`journeys run`, or `run --qualifying`, or
//!   `RIGHTKIT_QA_QUALIFYING=1`) refuses `--build`, `--build-ui` and `--kill-orphans`
//!   ([`refuse_flags_in_qualifying`], [`refuse_options_in_qualifying`]): the broker is the only
//!   compile path, and survivors fail a scenario instead of being killed quietly.
//! * **Run store cap (Sage C6).** The run store (`<managed root>/runs/<app>`) has a byte cap
//!   ([`DEFAULT_RUN_STORE_CAP_BYTES`], overridden by `--run-store-cap-bytes` or
//!   `RIGHTKIT_QA_RUN_STORE_CAP_BYTES`; the RK owner sets the number). [`cleanup_after_run`] drops the
//!   finished run's binary copies when asked, applies the ordinary run retention, then removes the
//!   binary copies of the oldest finished runs until the store fits. Evidence is never removed by
//!   the cap; a store still over the cap refuses the NEXT run at start.
use super::adapter::{EndRecord, Isolation, Record, SelectedJourney, Selection, StartRecord};
use super::bridge::{self, BridgeEntry, BridgeEnv, BridgeOptions};
use super::build_record::{
    self, ArtifactSpec, BuildPlan, BuildRecord, CurrentInputs, MarkerPolicy, RawBuild,
    RecordRequest, BUILD_RECORD_FILE, STORE_DIR,
};
use super::manifest::{self, Journey, Manifest, MANIFEST_FILE, PLATFORMS, TIERS};
use super::receipt::{
    self, validate_receipt, AdapterRun, BuildRecordRef, Counts, EngineStartCounts, EngineStarts,
    FileRef, Host, JourneyWorld, LaunchFacts, Launched, Receipt, ReceiptIsolation, ReceiptJourney,
    ReceiptKind, ReceiptSelection, ReceiptTiming, ValidateOptions, Verdict,
};
use super::{
    code, ArtifactDigest, Evidence, EvidenceKind, JourneyStatus, Problem, Units, PROTOCOL,
    RECEIPT_SCHEMA,
};
use crate::budget::{KindBudget, Limits};
use crate::config::QaConfig;
use crate::control;
use crate::evidence::CheckStatus;
use crate::interactive;
use crate::lock::LockOptions;
use crate::pool::{self, HomeTemplate, Job, Pool, SpecInputs, WorldPlan};
use crate::process::Tracker;
use crate::retention;
use crate::scenario::Scenario;
use crate::storage::{self, ManagedRun, RunOutcome};
use crate::suite::SuiteOptions;
use crate::supervise;
use crate::util::{
    canonical_plain, err, new_id, now_iso, now_ms, plain_path, sha256_hex, Error, Result,
};
use crate::worker::{
    self, Deadlines, ScenarioSpec, Survivor, SurvivorOwner, Token, WorkerLaunch, WorkerOutcome,
    World,
};
use crate::workspace;

use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Env var that makes `rightkit-qa run` a qualifying run (`1`, `true`, `yes`, `on`).
pub const QUALIFYING_ENV: &str = "RIGHTKIT_QA_QUALIFYING";
/// Flags a qualifying run refuses: the broker build lane is the only compile path, and orphans
/// are never killed quietly.
pub const QUALIFYING_REFUSED_FLAGS: [&str; 3] = ["--build", "--build-ui", "--kill-orphans"];
/// Env var that sets the run store cap in bytes (the `--run-store-cap-bytes` flag wins over it).
pub const RUN_STORE_CAP_ENV: &str = "RIGHTKIT_QA_RUN_STORE_CAP_BYTES";
/// Run store cap when nothing sets one: bytes under `<managed root>/runs/<app>`. A named default
/// the RK owner is expected to replace after measuring (a build record's binaries are tens of MB
/// per role; evidence is small).
pub const DEFAULT_RUN_STORE_CAP_BYTES: u64 = 8_000_000_000;
/// Lease the run marker claims while steps 1 to 3 run.
pub const RUN_LEASE: Duration = Duration::from_secs(3 * 3600);
pub const SELECTION_FILE: &str = "selection.json";
pub const REQUIRED_FILE: &str = "required-journeys.json";
pub const REQUIRED_SCHEMA: &str = "rightkit.journeys.required/1";
/// Written into a run directory when steps 1 to 3 fail, so the failure outlives the terminal.
pub const PREPARE_ERROR_FILE: &str = "prepare-error.txt";
const CLEANUP_LOCK_WAIT: Duration = Duration::from_secs(10);

// ------------------------------------------------------------------------- qualifying mode

/// True when the run is qualifying: `--qualifying` among `args`, or `RIGHTKIT_QA_QUALIFYING` truthy.
pub fn is_qualifying(args: &[String], env: &dyn Fn(&str) -> Option<String>) -> bool {
    args.iter().any(|a| a == "--qualifying")
        || env(QUALIFYING_ENV).is_some_and(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
}

fn refusal_reason(flag: &str) -> &'static str {
    match flag {
        "--kill-orphans" => {
            "survivors fail the scenario in a qualifying run; they are never killed quietly"
        }
        _ => {
            "the broker (`rustright cargo`, via `rightkit-qa journeys run`) is the only compile path"
        }
    }
}

/// Refuse `--build`, `--build-ui` and `--kill-orphans` (also as `--flag=value`) among `args`.
pub fn refuse_flags_in_qualifying(args: &[String]) -> Result<()> {
    for a in args {
        for flag in QUALIFYING_REFUSED_FLAGS {
            if a == flag || a.starts_with(&format!("{flag}=")) {
                return err(format!(
                    "{flag} is refused in a qualifying run: {}",
                    refusal_reason(flag)
                ));
            }
        }
    }
    Ok(())
}

/// The same refusal for options a library caller built: `build_engine` is `--build`, `build_ui` is
/// `--build-ui`, `kill_orphans` is `--kill-orphans`. A scenario's own `kill_orphans = true` is not
/// visible here (the suite reads it per scenario).
pub fn refuse_options_in_qualifying(opts: &SuiteOptions) -> Result<()> {
    for (set, flag) in [
        (opts.build_engine, "--build"),
        (opts.build_ui, "--build-ui"),
        (opts.kill_orphans, "--kill-orphans"),
    ] {
        if set {
            return err(format!(
                "{flag} is refused in a qualifying run: {}",
                refusal_reason(flag)
            ));
        }
    }
    Ok(())
}

// ------------------------------------------------------------------------ step 1: select

/// What the caller asked to run: explicit ids (empty means every journey that applies), the
/// platform (default: this host) and a comma list of tiers (default `fast`).
#[derive(Debug, Clone, Default)]
pub struct SelectionRequest {
    pub ids: Vec<String>,
    pub platform: Option<String>,
    pub tier: Option<String>,
}

/// The journeys a run will cover, fixed before anything is built.
#[derive(Debug, Clone)]
pub struct Selected {
    pub platform: String,
    /// The tiers selected, sorted and comma-joined (the receipt's `selection.tier`).
    pub tier: String,
    pub journeys: Vec<Journey>,
}

/// This host's platform word as the manifest spells it.
pub fn host_platform() -> Result<&'static str> {
    match std::env::consts::OS {
        "macos" => Ok("macos"),
        "windows" => Ok("windows"),
        "linux" => Ok("linux"),
        other => err(format!("journeys cannot run on {other}")),
    }
}

/// Filter `m` by id, platform and tier. The platform must be `host`: a journey for another
/// platform cannot run here and a receipt for it would be a lie. Every explicit id must exist,
/// declare the platform and belong to a selected tier; zero journeys is an error.
pub fn select_journeys(m: &Manifest, req: &SelectionRequest, host: &str) -> Result<Selected> {
    let platform = req.platform.clone().unwrap_or_else(|| host.to_string());
    if !PLATFORMS.contains(&platform.as_str()) {
        return err(format!(
            "unknown platform {platform:?} (macos, windows, linux)"
        ));
    }
    if platform != host {
        return err(format!(
            "{}: platform {platform:?} is not this host ({host}); a run covers the platform it runs on",
            code::HOST_MISMATCH
        ));
    }
    let tier_text = req.tier.clone().unwrap_or_else(|| "fast".to_string());
    let mut tiers: BTreeSet<String> = BTreeSet::new();
    for t in tier_text.split(',') {
        let t = t.trim();
        if !TIERS.contains(&t) {
            return err(format!("unknown tier {t:?} (fast, full, paid)"));
        }
        tiers.insert(t.to_string());
    }
    let mut seen = BTreeSet::new();
    for id in &req.ids {
        if !seen.insert(id.as_str()) {
            return err(format!("journey {id} is listed twice"));
        }
    }
    let declares = |j: &Journey| j.platforms.contains(&platform);
    let journeys: Vec<Journey> = if req.ids.is_empty() {
        m.journey
            .iter()
            .filter(|j| declares(j) && tiers.contains(&j.tier))
            .cloned()
            .collect()
    } else {
        let mut out = Vec::new();
        for id in &req.ids {
            let Some(j) = manifest::find(m, id) else {
                return err(format!("journey {id} is not in {MANIFEST_FILE}"));
            };
            if !declares(j) {
                return err(format!(
                    "journey {id} does not declare platform {platform}; skipping is not a way to meet a platform requirement"
                ));
            }
            if !tiers.contains(&j.tier) {
                return err(format!(
                    "journey {id} is tier {} but the selected tier is {}",
                    j.tier,
                    tiers.iter().cloned().collect::<Vec<_>>().join(",")
                ));
            }
            out.push(j.clone());
        }
        out
    };
    if journeys.is_empty() {
        return err(format!(
            "zero journeys selected for platform {platform}, tier {}: zero is never a pass",
            tiers.iter().cloned().collect::<Vec<_>>().join(",")
        ));
    }
    Ok(Selected {
        platform,
        tier: tiers.into_iter().collect::<Vec<_>>().join(","),
        journeys,
    })
}

/// Every journey id the manifest requires on each platform, in manifest order. Taken from the
/// whole manifest (tier and id filters do not shrink it) and frozen before the build.
pub fn required_by_platform(m: &Manifest) -> BTreeMap<String, Vec<String>> {
    PLATFORMS
        .iter()
        .map(|p| {
            (
                p.to_string(),
                m.journey
                    .iter()
                    .filter(|j| j.platforms.iter().any(|x| x.as_str() == *p))
                    .map(|j| j.id.clone())
                    .collect(),
            )
        })
        .collect()
}

fn named(o: &Option<String>) -> Option<&str> {
    o.as_deref().filter(|s| !s.trim().is_empty())
}

/// The one broker build a selection needs. One run builds once: either the manifest's `[build]`
/// section (for `runner = "scenario"` journeys) or exactly one adapter's `build` (for
/// `runner = "adapter"` journeys), never both. An artifact named only by `path` is refused: it is
/// not produced by this build, so the record could not bind it.
pub fn plan_build(m: &Manifest, selected: &Selected, app_root: &Path) -> Result<BuildPlan> {
    let scenario: Vec<&str> = selected
        .journeys
        .iter()
        .filter(|j| j.runner != "adapter")
        .map(|j| j.id.as_str())
        .collect();
    let adapter_count = selected.journeys.len() - scenario.len();
    if !scenario.is_empty() && adapter_count > 0 {
        return err("one run builds once: adapter and scenario journeys cannot share a selection");
    }
    if !scenario.is_empty() {
        let b = m.build.as_ref().ok_or_else(|| {
            Error(format!(
                "journeys {} use runner \"scenario\" but journeys.toml has no [build] section",
                scenario.join(", ")
            ))
        })?;
        let mut specs = Vec::new();
        for a in &b.artifacts {
            let (target, package) = (named(&a.cargo_target), named(&a.cargo_package));
            if target.is_none() && package.is_none() {
                return err(format!(
                    "{}: [build] role {:?} names no cargo_target or cargo_package; a prebuilt path is not produced by this build and cannot be bound",
                    code::ARTIFACT_NOT_FROM_BUILD,
                    a.role
                ));
            }
            specs.push(ArtifactSpec {
                role: a.role.clone(),
                cargo_target: target.map(str::to_string),
                cargo_package: package.map(str::to_string),
                marker: if a.qa_marker {
                    MarkerPolicy::Required
                } else {
                    MarkerPolicy::NotApplicable
                },
            });
        }
        return BuildPlan::new(&m.app, &b.command, app_root.to_path_buf(), specs);
    }
    let names: BTreeSet<&str> = selected
        .journeys
        .iter()
        .filter_map(|j| j.adapter.as_deref())
        .collect();
    if names.len() != 1 {
        return err(format!(
            "one run builds for exactly one adapter, the selected journeys name {}: {}",
            names.len(),
            names.iter().copied().collect::<Vec<_>>().join(", ")
        ));
    }
    let name = names.into_iter().next().unwrap_or_default();
    let decl = m
        .adapter
        .iter()
        .find(|a| a.name == name)
        .ok_or_else(|| Error(format!("adapter {name:?} is not declared")))?;
    let mut specs = Vec::new();
    for r in std::iter::once(&decl.executable).chain(decl.artifacts.iter()) {
        let (target, package) = (named(&r.cargo_target), named(&r.cargo_package));
        if target.is_none() && package.is_none() {
            return err(format!(
                "{}: adapter {name:?} role {:?} names no cargo_target or cargo_package; a prebuilt path is not produced by this build and cannot be bound",
                code::ARTIFACT_NOT_FROM_BUILD,
                r.role
            ));
        }
        specs.push(ArtifactSpec {
            role: r.role.clone(),
            cargo_target: target.map(str::to_string),
            cargo_package: package.map(str::to_string),
            marker: MarkerPolicy::for_role(&r.role),
        });
    }
    BuildPlan::new(name, &decl.build, app_root.to_path_buf(), specs)
}

// ---------------------------------------------------------------------------- run store cap

/// The run store cap in bytes (`<managed root>/runs/<app>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreCap(pub u64);

impl StoreCap {
    /// The `--run-store-cap-bytes` value, else `RIGHTKIT_QA_RUN_STORE_CAP_BYTES`, else
    /// [`DEFAULT_RUN_STORE_CAP_BYTES`]. A cap that is not a positive integer is refused (zero would
    /// remove every binary copy as soon as it is made).
    pub fn resolve(cli: Option<&str>, env: &dyn Fn(&str) -> Option<String>) -> Result<StoreCap> {
        let text = cli
            .map(str::to_string)
            .or_else(|| env(RUN_STORE_CAP_ENV).filter(|v| !v.trim().is_empty()));
        let Some(text) = text else {
            return Ok(StoreCap(DEFAULT_RUN_STORE_CAP_BYTES));
        };
        match text.trim().replace('_', "").parse::<u64>() {
            Ok(n) if n > 0 => Ok(StoreCap(n)),
            _ => err(format!(
                "run store cap {text:?} must be a positive number of bytes"
            )),
        }
    }
}

/// What a cleanup pass did.
#[derive(Debug, Default, Clone)]
pub struct CleanupReport {
    /// Binary store directories removed, with the bytes each held.
    pub removed: Vec<(PathBuf, u64)>,
    /// Whole finished runs the ordinary retention rule removed.
    pub pruned_runs: usize,
    /// Things that could not be done; a cleanup never fails the run.
    pub errors: Vec<String>,
    /// The store size at the last measurement and the cap it was measured against.
    pub total_bytes: u64,
    pub cap_bytes: u64,
    /// Bytes still over the cap after everything removable was removed.
    pub over_cap_bytes: u64,
}

impl CleanupReport {
    pub fn summary(&self) -> String {
        format!(
            "removed {} binary store(s) ({} bytes) and {} run(s); store {} of {} bytes{}",
            self.removed.len(),
            self.removed.iter().map(|(_, b)| *b).sum::<u64>(),
            self.pruned_runs,
            self.total_bytes,
            self.cap_bytes,
            if self.over_cap_bytes > 0 {
                format!(", {} bytes OVER the cap", self.over_cap_bytes)
            } else {
                String::new()
            }
        )
    }

    fn merge(&mut self, other: CleanupReport) {
        self.removed.extend(other.removed);
        self.pruned_runs += other.pruned_runs;
        self.errors.extend(other.errors);
        if other.cap_bytes > 0 {
            self.total_bytes = other.total_bytes;
            self.cap_bytes = other.cap_bytes;
            self.over_cap_bytes = other.over_cap_bytes;
        }
    }
}

/// Remove `<run_root>/build/bin` (the binary copies) when `run_root/build` and it are plain
/// directories, not links.
fn remove_store_bin(run_root: &Path, report: &mut CleanupReport) {
    let build = run_root.join("build");
    let bin = run_root.join(STORE_DIR);
    if !storage::safe_dir(&build) || !storage::safe_dir(&bin) {
        return;
    }
    let bytes = storage::directory_bytes(&bin).unwrap_or(0);
    match std::fs::remove_dir_all(&bin) {
        Ok(()) => report.removed.push((bin, bytes)),
        Err(e) => report
            .errors
            .push(format!("cannot remove {}: {e}", bin.display())),
    }
}

/// Bring `<managed root>/runs/<app>` under `cap` by removing the binary copies of the oldest
/// finished runs (never `current`, never a run that is still active, never evidence). A store that
/// is already under the cap is not touched. Holds the app's retention lock for the pass and skips
/// when another pass holds it.
pub fn cleanup_store(
    managed_root: &Path,
    app: &str,
    cap: StoreCap,
    current: Option<&str>,
) -> CleanupReport {
    let mut report = CleanupReport {
        cap_bytes: cap.0,
        ..Default::default()
    };
    let app_dir = managed_root.join("runs").join(app);
    if !storage::safe_dir(&app_dir) {
        return report;
    }
    let _guard = match storage::try_lock(&app_dir, CLEANUP_LOCK_WAIT) {
        Ok(Some(g)) => g,
        Ok(None) => {
            report
                .errors
                .push("another retention pass holds the lock; store cleanup skipped".into());
            return report;
        }
        Err(e) => {
            report
                .errors
                .push(format!("cannot lock the run store: {e}"));
            return report;
        }
    };
    let Some(total) = storage::directory_bytes(&app_dir) else {
        report
            .errors
            .push(format!("cannot measure {}", app_dir.display()));
        return report;
    };
    report.total_bytes = total;
    let mut now_total = total;
    if now_total > cap.0 {
        let now = now_ms();
        let mut finished: Vec<(u128, PathBuf)> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&app_dir) {
            for entry in rd.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if current == Some(name.as_str()) {
                    continue;
                }
                let dir = entry.path();
                if !storage::safe_dir(&dir) {
                    continue;
                }
                let Some(marker) = storage::read_marker(&dir.join(storage::RUN_MARKER)) else {
                    continue;
                };
                if marker.run_id != name
                    || marker.app != app
                    || storage::marker_active(&marker, now)
                {
                    continue;
                }
                finished.push((marker.completed_at.unwrap_or(marker.created_at), dir));
            }
        }
        finished.sort();
        for (_, dir) in finished {
            if now_total <= cap.0 {
                break;
            }
            let before = report.removed.len();
            remove_store_bin(&dir, &mut report);
            if report.removed.len() > before {
                now_total = now_total.saturating_sub(report.removed[before].1);
            }
        }
    }
    report.over_cap_bytes = now_total.saturating_sub(cap.0);
    report.total_bytes = now_total;
    report
}

/// The cleanup that runs after a run ends: drop the run's own binary copies when
/// `drop_binaries`, apply the ordinary run retention (`retention::prune_runs_after_job`), then
/// [`cleanup_store`] against `cap`. Call it after the run is finished
/// ([`storage::finish`]), because retention leaves an unfinished run alone. Never fails.
pub fn cleanup_after_run(run: &ManagedRun, cap: StoreCap, drop_binaries: bool) -> CleanupReport {
    let mut report = CleanupReport::default();
    let Some(root) = retention::managed_root_of_run(&run.root) else {
        report.errors.push(format!(
            "{} is not under <managed root>/runs/<app>/<run id>",
            run.root.display()
        ));
        return report;
    };
    if drop_binaries {
        remove_store_bin(&run.root, &mut report);
    }
    report.pruned_runs = retention::prune_runs_after_job(&root, &run.marker.app)
        .removed
        .len();
    report.merge(cleanup_store(
        &root,
        &run.marker.app,
        cap,
        Some(&run.marker.run_id),
    ));
    report
}

/// Close a run in which only steps 1 to 3 ran: finish it WITHOUT recording an outcome, because
/// it made no claim (retention keeps a run with no outcome, so the record stays inspectable), and
/// clean up. Its binary copies stay until the store cap needs the room.
pub fn close_prepared(run: &ManagedRun, cap: StoreCap) -> CleanupReport {
    let mut report = CleanupReport::default();
    if let Err(e) = storage::finish(run) {
        report.errors.push(format!("cannot finish the run: {e}"));
    }
    report.merge(cleanup_after_run(run, cap, false));
    report
}

/// Record how the run ended, finish it and clean up after it.
pub fn finish_run(
    run: &ManagedRun,
    outcome: RunOutcome,
    cap: StoreCap,
    drop_binaries: bool,
) -> CleanupReport {
    let mut report = CleanupReport::default();
    if let Err(e) = storage::set_run_outcome(&run.root, outcome) {
        report
            .errors
            .push(format!("cannot record the run outcome: {e}"));
    }
    if let Err(e) = storage::finish(run) {
        report.errors.push(format!("cannot finish the run: {e}"));
    }
    report.merge(cleanup_after_run(run, cap, drop_binaries));
    report
}

// ------------------------------------------------------------------- steps 2 and 3: prepare

pub struct PrepareOptions {
    pub app_root: PathBuf,
    pub ids: Vec<String>,
    pub platform: Option<String>,
    pub tier: Option<String>,
    /// The managed root; `None` means `RIGHTKIT_MANAGED_ROOT` or the platform default.
    pub managed_root: Option<PathBuf>,
    pub store_cap: StoreCap,
}

/// What steps 1 to 3 produced. `run` is left open (not finished): launch and collect continue it.
pub struct Prepared {
    pub run: ManagedRun,
    pub nonce: String,
    pub selection: Selection,
    pub selection_path: PathBuf,
    pub required_path: PathBuf,
    pub record: BuildRecord,
    pub record_path: PathBuf,
    pub plan: BuildPlan,
    pub manifest_sha256: String,
}

impl Prepared {
    /// A few lines for the terminal.
    pub fn describe(&self) -> String {
        let r = &self.record;
        format!(
            "run {} ({})\nselected {} journey(s) on {}: {}\nbuild request {} (exit {}, attested)\nsource tree {}{}\nbuild record {}\n{}",
            self.run.marker.run_id,
            self.run.root.display(),
            self.selection.journeys.len(),
            self.selection.platform,
            self.selection.ids().join(", "),
            r.broker.request_id,
            r.broker.exit_code,
            r.source.tree,
            if r.source.dirty { " (dirty)" } else { "" },
            self.record_path.display(),
            r.artifacts
                .iter()
                .map(|a| format!("  {} {} {}", a.role, a.sha256, a.store_path))
                .collect::<Vec<_>>()
                .join("\n")
        )
    }
}

fn new_nonce() -> Result<String> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).map_err(|e| Error(format!("random source unavailable: {e}")))?;
    Ok(rightkit_fs::digest::hex(&b))
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes).map_err(|e| Error(format!("cannot write {}: {e}", path.display())))
}

/// Steps 1 to 3 with the real broker: [`build_record::spawn_broker`].
pub fn prepare(opts: &PrepareOptions) -> Result<Prepared> {
    prepare_with(opts, &build_record::spawn_broker)
}

/// Steps 1 to 3 with `build` standing in for the broker spawn (fixtures pass a fake). Selection and
/// planning happen before anything is created or built. A failure after the run directory exists
/// marks the run `failed`, writes `prepare-error.txt` into it and returns the error.
pub fn prepare_with(
    opts: &PrepareOptions,
    build: &dyn Fn(&BuildPlan) -> Result<RawBuild>,
) -> Result<Prepared> {
    let app_root = canonical_plain(&opts.app_root)
        .map_err(|e| Error(format!("cannot resolve {}: {e}", opts.app_root.display())))?;
    // Step 1: select, entirely before any side effect.
    let checked = manifest::check(&app_root)?;
    for warning in manifest::require_ok_for_run(&checked)? {
        eprintln!("[qa] guardrail (warning until enforced): {warning}");
    }
    let selected = select_journeys(
        &checked.manifest,
        &SelectionRequest {
            ids: opts.ids.clone(),
            platform: opts.platform.clone(),
            tier: opts.tier.clone(),
        },
        host_platform()?,
    )?;
    // Zero selected is an error, never a pass: a scenario journey whose entry names no scenario
    // file is refused here, before anything is created or built.
    for j in selected.journeys.iter().filter(|j| j.runner != "adapter") {
        if scenario_files_for(&app_root, &j.entry).is_empty() {
            return err(format!(
                "journey {}: entry {:?} matches no scenario file; zero units is an error, never a pass",
                j.id, j.entry
            ));
        }
    }
    // Every scenario file a run reads is bound to a committed source: an ignored, untracked or
    // staged-only file is invisible to the dirty check, so it is refused here before anything builds.
    for j in selected.journeys.iter().filter(|j| j.runner != "adapter") {
        for f in scenario_files_for(&app_root, &j.entry) {
            if !build_record::committed_at_head(&f)? {
                return err(format!(
                    "journey {}: scenario file {} is not committed at HEAD (ignored, untracked or staged only); commit it, a file a run reads must be a committed source",
                    j.id,
                    f.display()
                ));
            }
        }
    }
    let required = required_by_platform(&checked.manifest);
    let plan = plan_build(&checked.manifest, &selected, &app_root)?;
    let app = checked.manifest.app.clone();
    let managed_root = storage::managed_root(opts.managed_root.as_deref());
    // The run store is bounded: make room first, and refuse to add to a store that cannot be made to fit.
    let pre = cleanup_store(&managed_root, &app, opts.store_cap, None);
    if pre.over_cap_bytes > 0 {
        return err(format!(
            "the run store for {app} holds {} bytes, {} over the {}-byte cap, after removing old binary copies; remove old runs (`rightkit-qa prune-runs`) or raise {RUN_STORE_CAP_ENV}",
            pre.total_bytes, pre.over_cap_bytes, pre.cap_bytes
        ));
    }
    let run_id = new_id();
    let run = storage::create(Some(&managed_root), &app, Some(&run_id), RUN_LEASE)?;
    match prepare_in(
        &run,
        &app_root,
        &checked.manifest,
        &checked.manifest_sha256,
        &selected,
        &required,
        plan,
        build,
    ) {
        Ok(p) => Ok(p),
        Err(e) => {
            let _ = std::fs::write(run.root.join(PREPARE_ERROR_FILE), format!("{e}\n"));
            let _ = finish_run(&run, RunOutcome::Failed, opts.store_cap, false);
            Err(e)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn prepare_in(
    run: &ManagedRun,
    app_root: &Path,
    m: &Manifest,
    manifest_sha256: &str,
    selected: &Selected,
    required: &BTreeMap<String, Vec<String>>,
    plan: BuildPlan,
    build: &dyn Fn(&BuildPlan) -> Result<RawBuild>,
) -> Result<Prepared> {
    let nonce = new_nonce()?;
    let selection = Selection {
        protocol: PROTOCOL,
        run_id: run.marker.run_id.clone(),
        nonce: nonce.clone(),
        app: m.app.clone(),
        platform: selected.platform.clone(),
        tier: selected.tier.clone(),
        manifest_sha256: manifest_sha256.to_string(),
        journeys: selected
            .journeys
            .iter()
            .map(|j| SelectedJourney {
                id: j.id.clone(),
                entry: j.entry.clone(),
            })
            .collect(),
    };
    let bytes = serde_json::to_vec_pretty(&selection)?;
    Selection::parse(&bytes)?;
    let selection_path = run.root.join(SELECTION_FILE);
    write_new(&selection_path, &bytes)?;
    let required_path = run.root.join(REQUIRED_FILE);
    write_new(
        &required_path,
        &serde_json::to_vec_pretty(&serde_json::json!({
            "schema": REQUIRED_SCHEMA,
            "app": m.app,
            "manifest_sha256": manifest_sha256,
            "platforms": required,
        }))?,
    )?;

    // Step 2: source identity (and the other inputs the record is bound to), before the build.
    let before = CurrentInputs::gather(app_root, &plan, manifest_sha256)?;
    // Step 3: the broker build, then the same measurements again.
    let started_at = now_iso();
    let raw = build(&plan)?;
    let ended_at = now_iso();
    let after = CurrentInputs::gather(app_root, &plan, manifest_sha256)?;
    let record = build_record::record_build(&RecordRequest {
        run_id: &run.marker.run_id,
        app: &m.app,
        manifest_path: MANIFEST_FILE,
        plan: &plan,
        before: &before,
        after: &after,
        started_at,
        ended_at,
        raw: &raw,
        run_dir: &run.root,
    })?;
    let record_path = run.root.join(BUILD_RECORD_FILE);
    record.write(&record_path)?;
    Ok(Prepared {
        run: run.clone(),
        nonce,
        selection,
        selection_path,
        required_path,
        record,
        record_path,
        plan,
        manifest_sha256: manifest_sha256.to_string(),
    })
}

// ------------------------------------------------------------- steps 4 and 5: launch, collect, receipt

/// `results.jsonl`: the runner writes it as the adapter-protocol records of the run, so the receipt
/// can be re-checked against it (`rightkit-qa journeys validate-receipt` reads it beside the receipt).
pub const RESULTS_FILE: &str = "results.jsonl";
/// The receipt, written into the run directory (the evidence root).
pub const RECEIPT_FILE: &str = "receipt.json";
/// Written into a run directory when launch or collect fails after the build record exists.
pub const RUN_ERROR_FILE: &str = "run-error.txt";
/// The runner's own judgement of one scenario, kept beside the worker's raw `outcome.json`.
pub const SETTLED_FILE: &str = "settled.json";
/// Environment variable that carries the run token to bridged test binaries (Sage C8: world marker
/// and run token together mean "launched by the runner", not from inside `cargo test`).
pub const RUN_TOKEN_ENV: &str = "RIGHTKIT_QA_RUN_TOKEN";
/// The adapter name a scenario-runner receipt records for itself.
pub const ADAPTER_NAME: &str = "rightkit-qa";
/// A step or scenario is killed at this multiple of its declared budget: the budget is a speed
/// signal, the deadline is for a hang.
pub const HARD_DEADLINE_FACTOR: u64 = 10;
pub const DEFAULT_STEP_DEADLINE_MS: u64 = 120_000;
pub const DEFAULT_SCENARIO_DEADLINE_MS: u64 = 600_000;
pub const MIN_STEP_DEADLINE_MS: u64 = 10_000;
const MAC_UI_SERIAL_REASON: &str =
    "macOS UI scenarios share one fixed QA app bundle per app until the per-scenario worker stages its own";
const BRIDGE_SERIAL_REASON: &str = "bridged Harness binaries serialize on the run lock";
const BRIDGE_LOCK_WAIT: Duration = Duration::from_secs(6 * 3600);

/// How `journeys run` ends.
pub enum RunMode {
    /// Select, build, launch, collect, receipt.
    Qualifying,
    /// Record a scenario from executed steps. No run, no build, no receipt.
    Explore(ExploreRecording),
}

/// The executed steps of an `explore` session, to be written as a scenario TOML.
pub struct ExploreRecording {
    pub dir: PathBuf,
    pub name: String,
    pub target: String,
    pub tokens: Vec<String>,
    pub steps: Vec<receipt::ExploreStep>,
}

pub struct JourneyRunOptions {
    pub prepare: PrepareOptions,
    /// The `rightkit-qa` executable started as `worker <world dir>` (normally `current_exe()`).
    pub exe: PathBuf,
    pub mode: RunMode,
}

/// The effects of a run, so fixtures stand in for each of them.
pub struct Hooks<'a> {
    /// The broker build (steps 2-3), as for [`prepare_with`].
    pub broker: &'a dyn Fn(&BuildPlan) -> Result<RawBuild>,
    /// Run one scenario in a supervised worker: [`supervise::run_worker`] with the runner's exe.
    pub worker: &'a (dyn Fn(&ScenarioSpec) -> Result<WorkerOutcome> + Sync),
    /// Start one bridged process under the given tracker: [`bridge::owned_exec`].
    pub bridge_exec: &'a (dyn Fn(&Tracker, &bridge::Invocation) -> Result<bridge::RawRun> + Sync),
    /// `Some(reason)` when this process is outside the interactive desktop session.
    pub session: &'a dyn Fn() -> Option<String>,
}

/// The result of `journeys run`.
#[derive(Debug, Default)]
pub struct RunReport {
    pub run_id: String,
    pub run_dir: PathBuf,
    pub receipt_path: Option<PathBuf>,
    pub results_path: Option<PathBuf>,
    /// The scenario an explore run recorded.
    pub explored: Option<PathBuf>,
    pub verdict: Option<Verdict>,
    /// The receipt, read back from disk, validated as a pass.
    pub passed: bool,
    pub problems: Vec<Problem>,
    pub counts: Option<Counts>,
    /// Timings that were over budget inside the warn window.
    pub slow: Vec<String>,
    pub cleanup: CleanupReport,
}

impl RunReport {
    pub fn exit_code(&self) -> i32 {
        if self.passed || self.explored.is_some() {
            0
        } else {
            1
        }
    }

    pub fn describe(&self) -> String {
        if let Some(p) = &self.explored {
            return format!(
                "explore: recorded {} (not evidence: review it, commit it, then run it)",
                p.display()
            );
        }
        let mut out = format!("run {} ({})\n", self.run_id, self.run_dir.display());
        if let Some(c) = &self.counts {
            out.push_str(&format!(
                "selected {} executed {} passed {} failed {} skipped {} errored {} missing {}\n",
                c.selected, c.executed, c.passed, c.failed, c.skipped, c.errored, c.missing
            ));
        }
        for p in &self.problems {
            out.push_str(&format!(
                "- {}{}: {}\n",
                p.code,
                p.id.as_deref()
                    .map(|i| format!(" [{i}]"))
                    .unwrap_or_default(),
                p.detail
            ));
        }
        for s in &self.slow {
            out.push_str(&format!("slow (warning inside the window): {s}\n"));
        }
        if let Some(p) = &self.receipt_path {
            out.push_str(&format!("receipt {}\n", p.display()));
        }
        out.push_str(&format!(
            "receipt validates as {}: {}",
            self.verdict.map(|v| v.as_str()).unwrap_or("unknown"),
            if self.passed { "ACCEPTED" } else { "REFUSED" }
        ));
        out
    }
}

/// `journeys run` with the real broker, supervisor and bridge executor.
pub fn run_journeys(opts: &JourneyRunOptions) -> Result<RunReport> {
    let exe = opts.exe.clone();
    run_with(
        opts,
        &Hooks {
            broker: &build_record::spawn_broker,
            worker: &|spec| supervise::run_worker(spec, &exe),
            bridge_exec: &|tracker, inv| bridge::owned_exec(tracker.clone())(inv),
            session: &|| interactive::non_interactive_reason(&interactive::session_facts()),
        },
    )
}

// ---- small helpers

fn join_msg(m: &mut String, add: &str) {
    if !m.is_empty() {
        m.push_str("; ");
    }
    m.push_str(add);
}

fn path_keys(p: &Path) -> Vec<String> {
    plain_path(p)
        .components()
        .map(|c| {
            let s = c.as_os_str().to_string_lossy().into_owned();
            if cfg!(windows) {
                s.to_lowercase()
            } else {
                s
            }
        })
        .collect()
}

/// `path` lies strictly under `root` (pure text).
fn under(root: &Path, path: &Path) -> bool {
    let (r, p) = (path_keys(root), path_keys(path));
    path.is_absolute() && p.len() > r.len() && p[..r.len()] == r[..]
}

fn same_path(a: &Path, b: &Path) -> bool {
    path_keys(a) == path_keys(b)
}

fn last_env<'a>(env: &'a [(String, String)], name: &str) -> Option<&'a str> {
    env.iter()
        .rev()
        .find(|(k, _)| {
            if cfg!(windows) {
                k.eq_ignore_ascii_case(name)
            } else {
                k == name
            }
        })
        .map(|(_, v)| v.as_str())
}

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

fn wild_match(p: &[u8], t: &[u8]) -> bool {
    match p.split_first() {
        None => t.is_empty(),
        Some((b'*', rest)) => (0..=t.len()).any(|i| wild_match(rest, &t[i..])),
        Some((b'?', rest)) => !t.is_empty() && wild_match(rest, &t[1..]),
        Some((c, rest)) => t.first() == Some(c) && wild_match(rest, &t[1..]),
    }
}

fn files_matching(dir: &Path, pattern: &str) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut out: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| wild_match(pattern.as_bytes(), n.to_string_lossy().as_bytes()))
        })
        .collect();
    out.sort();
    out
}

/// The scenario files a journey entry names, sorted: a literal file, a directory (its
/// `*.scenario.toml` files) or a glob in the last component. Empty when nothing matches; the caller
/// makes that an `errored` journey, never a quiet pass.
pub fn scenario_files_for(app_root: &Path, entry: &str) -> Vec<PathBuf> {
    let glob = |s: &str| s.contains(['*', '?']);
    if !glob(entry) {
        let p = app_root.join(entry);
        if p.is_file() {
            return vec![p];
        }
        if p.is_dir() {
            return files_matching(&p, "*.scenario.toml");
        }
        return vec![];
    }
    let (dir, pattern) = match entry.rsplit_once(['/', '\\']) {
        Some((d, p)) => (d, p),
        None => ("", entry),
    };
    if glob(dir) {
        return vec![];
    }
    files_matching(&app_root.join(dir), pattern)
}

/// Hard deadlines from the tier's `[budget]` or the defaults. A scenario's own `timeout_s` wins for
/// the scenario deadline.
pub fn deadlines_for(timeout_s: Option<u64>, kind: Option<&KindBudget>) -> Deadlines {
    let scenario_ms = timeout_s
        .map(|s| s.saturating_mul(1000))
        .or_else(|| {
            kind.and_then(|k| k.journey_ms)
                .map(|j| j.saturating_mul(HARD_DEADLINE_FACTOR))
        })
        .unwrap_or(DEFAULT_SCENARIO_DEADLINE_MS)
        .max(1);
    let step_ms = kind
        .and_then(|k| k.step_ms)
        .map(|s| {
            s.saturating_mul(HARD_DEADLINE_FACTOR)
                .max(MIN_STEP_DEADLINE_MS)
        })
        .unwrap_or(DEFAULT_STEP_DEADLINE_MS)
        .min(scenario_ms);
    Deadlines {
        step_ms,
        scenario_ms,
        teardown_ms: pool::DEFAULT_TEARDOWN_MS,
    }
}

/// What the runner can check about one launch against the world it ran in. Every message is a
/// reason the scenario is not recorded as isolated.
pub fn launch_problems(world: &World, run_root: &Path, l: &WorkerLaunch) -> Vec<String> {
    let mut p = vec![];
    if !under(&world.dir, &l.data_dir) {
        p.push(format!(
            "the app was told the data directory {}, which is not under its world {}",
            l.data_dir.display(),
            world.dir.display()
        ));
    }
    let mut told = false;
    if let Some(name) = &l.data_dir_env {
        told = true;
        match last_env(&l.env, name) {
            Some(v) if same_path(Path::new(v), &l.data_dir) => {}
            Some(v) => p.push(format!(
                "{name}={v:?} is not the data directory {} the launch recorded",
                l.data_dir.display()
            )),
            None => p.push(format!("the launch environment has no {name}")),
        }
    }
    if let Some(flag) = &l.data_dir_arg {
        told = true;
        match arg_value(&l.args, flag) {
            Some(v) if same_path(Path::new(v), &l.data_dir) => {}
            _ => p.push(format!(
                "the launch arguments do not carry {flag} <data dir>"
            )),
        }
    }
    if !told {
        p.push("no record of how the app was told its data directory".into());
    }
    if last_env(&l.env, &world.env_marker.name) != Some(world.env_marker.value.as_str()) {
        p.push(format!(
            "the world marker {} is not in the launch environment",
            world.env_marker.name
        ));
    }
    for (k, v) in &l.env {
        let dir_like = is_isolation_key(k) || k == control::token::ENV_AUTH_FILE;
        if dir_like && Path::new(v).is_absolute() && !under(run_root, Path::new(v)) {
            p.push(format!(
                "launch environment {k}={v:?} points outside the run root"
            ));
        }
    }
    if l.token_less_refused.is_some() {
        match last_env(&l.env, control::token::ENV_AUTH_FILE) {
            Some(v) if same_path(Path::new(v), &world.control_token_file) => {}
            _ => p.push(format!(
                "the launch environment does not name the world's token file in {}",
                control::token::ENV_AUTH_FILE
            )),
        }
    }
    p
}

/// One journey from its units `(name, status, reason)`, 1:1 and without softening: no unit is
/// `errored`; any errored unit is `errored`, else any failed is `failed`, else any skipped or not
/// run is `skipped`, else `passed`. Counts: errored counts as failed, not run as skipped.
pub fn aggregate(
    id: &str,
    units: &[(String, JourneyStatus, String)],
    started_at: String,
    duration_ms: u64,
    evidence: Vec<Evidence>,
    empty_reason: &str,
) -> super::adapter::JourneyResult {
    let mut u = Units {
        total: units.len() as u64,
        ..Units::default()
    };
    let (mut errored, mut failed, mut skipped): (Option<String>, Option<String>, Option<String>) =
        (None, None, None);
    for (name, st, why) in units {
        let why = if why.trim().is_empty() {
            "no reason given"
        } else {
            why.as_str()
        };
        let text = format!("{name}: {why}");
        match st {
            JourneyStatus::Passed => u.passed += 1,
            JourneyStatus::Errored => {
                u.failed += 1;
                errored.get_or_insert(text);
            }
            JourneyStatus::Failed => {
                u.failed += 1;
                failed.get_or_insert(text);
            }
            JourneyStatus::Skipped | JourneyStatus::Missing => {
                u.skipped += 1;
                skipped.get_or_insert(text);
            }
        }
    }
    let (status, reason) = if u.total == 0 {
        (JourneyStatus::Errored, Some(empty_reason.to_string()))
    } else if let Some(t) = errored {
        (JourneyStatus::Errored, Some(t))
    } else if let Some(t) = failed {
        (JourneyStatus::Failed, Some(t))
    } else if let Some(t) = skipped {
        (JourneyStatus::Skipped, Some(t))
    } else {
        (JourneyStatus::Passed, None)
    };
    super::adapter::JourneyResult {
        id: id.to_string(),
        status,
        started_at,
        duration_ms,
        units: u,
        evidence,
        reason,
    }
}

/// Hash `path` (a file under `run_root`) as run evidence; `None` when it is absent or outside.
fn evidence_of(run_root: &Path, path: &Path, kind: EvidenceKind) -> Option<Evidence> {
    let bytes = std::fs::read(path).ok()?;
    let rel = plain_path(path)
        .strip_prefix(plain_path(run_root))
        .ok()?
        .to_path_buf();
    let rel = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    Some(Evidence {
        path: rel,
        sha256: sha256_hex(&bytes),
        kind,
    })
}

/// A judged unit (one scenario, or one bridged journey): what the runner decided and why.
struct UnitReport {
    name: String,
    status: CheckStatus,
    message: String,
    started_at: String,
    duration_ms: u64,
    outcome: Option<WorkerOutcome>,
    world: Option<World>,
    /// Build record roles whose binaries this unit launched.
    roles: Vec<String>,
    /// Why the unit's launch is not recorded as isolated.
    isolation_problems: Vec<String>,
    evidence: Vec<Evidence>,
}

impl UnitReport {
    fn new(name: &str, world: Option<World>) -> Self {
        UnitReport {
            name: name.to_string(),
            status: CheckStatus::Errored,
            message: String::new(),
            started_at: now_iso(),
            duration_ms: 0,
            outcome: None,
            world,
            roles: vec![],
            isolation_problems: vec![],
            evidence: vec![],
        }
    }

    fn errored(mut self, why: impl Into<String>) -> Self {
        self.status = CheckStatus::Errored;
        self.message = why.into();
        self
    }

    /// A pass the runner cannot stand behind becomes `errored`; any other status stands.
    fn demote(&mut self, why: &str) {
        if self.status == CheckStatus::Passed {
            self.status = CheckStatus::Errored;
        }
        join_msg(&mut self.message, why);
    }
}

/// What the pool hands back for one scenario.
struct UnitRun {
    started_at: String,
    duration_ms: u64,
    spec: ScenarioSpec,
    outcome: Result<WorkerOutcome>,
}

/// The binaries of the build record: `(role, absolute store path)`.
fn record_roles(rec: &BuildRecord, run_root: &Path) -> Vec<(String, PathBuf)> {
    rec.artifacts
        .iter()
        .map(|a| (a.role.clone(), run_root.join(&a.store_path)))
        .collect()
}

/// Judge one scenario outcome. The worker's verdict is advice: a pass stands only with launch
/// facts that check out against the world, binaries that are the build record's, and no survivor.
fn judge_unit(
    name: &str,
    ran: std::result::Result<UnitRun, String>,
    world: Option<World>,
    roles: &[(String, PathBuf)],
    run_root: &Path,
) -> UnitReport {
    let mut rep = UnitReport::new(name, world.clone());
    let ran = match ran {
        Err(why) => return rep.errored(format!("scenario could not run: {why}")),
        Ok(r) => r,
    };
    rep.started_at = ran.started_at.clone();
    rep.duration_ms = ran.duration_ms;
    let mut outcome = match ran.outcome {
        Err(e) => return rep.errored(e.0),
        Ok(o) => o,
    };
    if outcome.status == CheckStatus::Passed && !outcome.survivors.is_empty() {
        outcome.status = CheckStatus::Errored;
        join_msg(
            &mut outcome.message,
            &format!(
                "{} process(es) survived teardown; any survivor fails the scenario",
                outcome.survivors.len()
            ),
        );
    }
    if let Err(e) = outcome.validate_for(&ran.spec) {
        return rep.errored(format!("the worker's outcome was refused: {}", e.0));
    }
    rep.status = outcome.status;
    rep.message = outcome.message.clone();
    match (&outcome.launch, world.as_ref()) {
        (None, _) => {
            if outcome.status == CheckStatus::Passed {
                rep.demote("the worker reported no launch: a scenario that launched nothing proves nothing about the build, and its isolation cannot be recorded");
            }
        }
        (Some(l), w) => {
            for b in &l.binaries {
                match roles.iter().find(|(_, p)| same_path(p, b)) {
                    Some((role, _)) => {
                        if !rep.roles.contains(role) {
                            rep.roles.push(role.clone());
                        }
                    }
                    None => rep.demote(&format!(
                        "the scenario launched {}, which is not a binary of the build record",
                        b.display()
                    )),
                }
            }
            if l.binaries.is_empty() {
                rep.demote("the scenario launched no binary of the build record");
            }
            if let Some(w) = w {
                rep.isolation_problems = launch_problems(w, run_root, l);
                if !rep.isolation_problems.is_empty() {
                    let why = rep.isolation_problems.join("; ");
                    rep.demote(&format!("isolation not recorded: {why}"));
                }
            }
        }
    }
    rep.outcome = Some(outcome);
    rep
}

/// Re-hash every launched binary after the run, and re-measure the source: a binary swapped, or a
/// tracked file edited, while scenarios ran invalidates what ran.
fn post_run_problems(
    rec: &BuildRecord,
    run_dir: &Path,
    cur: &CurrentInputs,
    reps: &mut [UnitReport],
) -> Vec<String> {
    let mut out = vec![];
    let stale = rec.stale_against(cur);
    if !stale.is_empty() {
        let why = stale
            .iter()
            .map(|p| format!("{}: {}", p.code, p.detail))
            .collect::<Vec<_>>()
            .join("; ");
        out.push(format!("the source changed during the run: {why}"));
        for r in reps.iter_mut() {
            r.demote(&format!("the source changed during the run: {why}"));
        }
        return out;
    }
    let roles: BTreeSet<String> = reps.iter().flat_map(|r| r.roles.iter().cloned()).collect();
    for role in roles {
        if let Err(e) = rec.verify_launch(run_dir, &role, cur) {
            let why = format!("binary {role} changed during the run: {e}");
            out.push(why.clone());
            for r in reps.iter_mut().filter(|r| r.roles.contains(&role)) {
                r.demote(&why);
            }
        }
    }
    out
}

/// A journey ready for the receipt and the results file.
struct JourneyOut {
    result: super::adapter::JourneyResult,
    journey: ReceiptJourney,
}

fn tokens_of(j: &Journey) -> Vec<Token> {
    pool::parse_tokens(&j.tokens).unwrap_or_default()
}

/// Per host step durations by kind, from this app's earlier passing receipts in the run store.
fn host_history(
    managed_root: &Path,
    app: &str,
    current: &str,
    os: &str,
    arch: &str,
) -> BTreeMap<String, Vec<u64>> {
    let mut out: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    let dir = managed_root.join("runs").join(app);
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return out;
    };
    for e in rd.flatten() {
        if e.file_name().to_string_lossy() == current {
            continue;
        }
        let path = e.path().join(RECEIPT_FILE);
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if !meta.is_file() || meta.len() > 16 * 1024 * 1024 {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(r) = receipt::parse_receipt(&bytes) else {
            continue;
        };
        if r.runner.as_deref() != Some(receipt::RUNNER)
            || r.verdict != Verdict::Passed
            || r.host.os != os
            || r.host.arch != arch
        {
            continue;
        }
        for j in &r.journeys {
            for s in j.steps.iter().flatten() {
                out.entry(s.kind.clone()).or_default().push(s.ms);
            }
        }
    }
    out
}

/// The `[budget]` section of the receipt: the manifest's window, declared limits for the selected
/// tiers (the loosest when several are selected, since the receipt carries one set) and 2x this
/// host's p95 once enough samples exist.
fn budget_for(
    m: &Manifest,
    tiers: &BTreeSet<String>,
    journeys: &[ReceiptJourney],
    history: &BTreeMap<String, Vec<u64>>,
    tier_text: &str,
    ended_at: &str,
) -> Result<Option<receipt::ReceiptBudget>> {
    let Some(decl) = &m.budget else {
        return Ok(None);
    };
    let kinds: Vec<&KindBudget> = tiers.iter().filter_map(|t| decl.kind(t)).collect();
    let step_ms = kinds.iter().filter_map(|k| k.step_ms).max();
    let journey_ms = kinds.iter().filter_map(|k| k.journey_ms).max();
    let mut step_limits = BTreeMap::new();
    if let Some(declared) = step_ms {
        let seen: BTreeSet<&str> = journeys
            .iter()
            .flat_map(|j| j.steps.iter().flatten())
            .map(|s| s.kind.as_str())
            .collect();
        for kind in seen {
            let l = Limits::for_step(
                declared,
                history.get(kind).map(Vec::as_slice).unwrap_or(&[]),
            );
            step_limits.insert(
                kind.to_string(),
                receipt::BudgetLimits {
                    warn_ms: l.warn_ms,
                    enforce_ms: l.enforce_ms,
                },
            );
        }
    }
    let journey_limit = journey_ms.map(|ms| receipt::BudgetLimits {
        warn_ms: ms,
        enforce_ms: ms,
    });
    receipt::ReceiptBudget::build(
        &decl.warn_until,
        step_limits,
        journey_limit,
        tier_text,
        journeys,
        ended_at,
    )
    .map(Some)
}

/// What a run's judged journeys add up to, before the receipt is written.
struct Gathered {
    journeys: Vec<JourneyOut>,
    launched: BTreeMap<String, crate::binary::LaunchCheck>,
    isolated: bool,
    adapter_name: String,
    adapter_argv: Vec<String>,
    adapter_exit: i64,
    isolation_dir: PathBuf,
    problems: Vec<String>,
}

fn status_of(s: CheckStatus) -> JourneyStatus {
    receipt::journey_status_of(s)
}

// ---- scenario runner

#[allow(clippy::too_many_arguments)]
/// One unit's engine-start counts, read from its scenario's `engine-starts.json` artifact (written at
/// teardown from `engine::EngineStarts`). `None` when the artifact is absent or unreadable. Only the
/// count fields are taken; the file's own `allowed` is ignored.
fn engine_start_counts_of(o: &WorkerOutcome) -> Option<EngineStartCounts> {
    let a = o
        .artifacts
        .iter()
        .find(|a| a.name.ends_with("engine-starts.json"))?;
    serde_json::from_str(&std::fs::read_to_string(&a.path).ok()?).ok()
}

#[allow(clippy::too_many_arguments)]
fn gather_scenarios(
    prepared: &Prepared,
    m: &Manifest,
    journeys: &[Journey],
    app_root: &Path,
    cfg: &QaConfig,
    cur: &CurrentInputs,
    hooks: &Hooks<'_>,
    managed_root: &Path,
) -> Result<Gathered> {
    let run = &prepared.run;
    let record = &prepared.record;
    let platform = prepared.selection.platform.clone();
    // The base workspace the worlds are cut from (as the suite does): its home is the run root of
    // every world, its binding file is what each world home is built from.
    let ws = workspace::create(
        managed_root,
        Some(&format!("{}-w", run.marker.run_id)),
        &m.app,
    )?;
    let template = HomeTemplate::capture(&ws.home)?;
    let roles = record_roles(record, &run.root);

    struct Unit {
        journey: usize,
        name: String,
        sha256: String,
        scenario: std::result::Result<Scenario, String>,
    }
    let mut units: Vec<Unit> = vec![];
    let mut journey_error: Vec<Option<String>> = vec![None; journeys.len()];
    let mut grants: Vec<Vec<Token>> = vec![];
    for (ji, j) in journeys.iter().enumerate() {
        grants.push(match pool::parse_tokens(&j.tokens) {
            Ok(g) => g,
            Err(e) => {
                journey_error[ji] = Some(e.0);
                vec![]
            }
        });
        if journey_error[ji].is_some() {
            continue;
        }
        let files = scenario_files_for(app_root, &j.entry);
        if files.is_empty() {
            journey_error[ji] = Some(format!(
                "entry {:?} matched no scenario file: zero units is never a pass",
                j.entry
            ));
            continue;
        }
        for f in files {
            let fallback = f
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let sha = std::fs::read(&f)
                .map(|b| sha256_hex(&b))
                .unwrap_or_default();
            // A file not committed at HEAD is an errored unit: it never runs and never passes.
            let committed = matches!(build_record::committed_at_head(&f), Ok(true));
            let scenario = if !committed {
                Err(format!(
                    "scenario file {} is not committed at HEAD: refused, never counted passed",
                    f.display()
                ))
            } else {
                Scenario::load(&f).map_err(|e| e.0)
            }
            .and_then(|s| {
                if let Some(t) = s.grants.iter().find(|t| !grants[ji].contains(t)) {
                    Err(format!(
                        "the scenario declares token {} that journey {} does not",
                        pool::token_name(*t),
                        j.id
                    ))
                } else {
                    Ok(s)
                }
            });
            units.push(Unit {
                journey: ji,
                name: scenario
                    .as_ref()
                    .map(|s| s.name.clone())
                    .unwrap_or(fallback),
                sha256: sha,
                scenario,
            });
        }
    }
    let runnable: Vec<usize> = (0..units.len())
        .filter(|i| units[*i].scenario.is_ok())
        .collect();
    let jobs: Vec<Job> = runnable
        .iter()
        .map(|i| Job {
            id: units[*i].name.clone(),
            tokens: grants[units[*i].journey].clone(),
        })
        .collect();
    let host = &cfg.host;
    let serial = (platform == "macos"
        && runnable
            .iter()
            .any(|i| units[*i].scenario.as_ref().is_ok_and(|s| s.target == "ui")))
    .then_some(MAC_UI_SERIAL_REASON);
    let size = pool::host_pool_size(host, jobs.len(), serial);
    println!("[qa] pool: {} worker(s) ({})", size.workers, size.reason);
    let plan = WorldPlan::new(run.marker.run_id.clone(), ws.home.clone(), host, jobs.len())?;

    let binaries: serde_json::Map<String, Value> = roles
        .iter()
        .map(|(r, p)| (r.clone(), Value::String(p.to_string_lossy().into_owned())))
        .collect();
    let engine = binaries.get("engine").cloned().unwrap_or(Value::Null);
    let ui = binaries
        .get("app")
        .or_else(|| binaries.get("ui"))
        .cloned()
        .unwrap_or(Value::Null);
    // The app's step plugin (`[plugin]` in journeys.toml): its role's recorded binary, the args and
    // the hello timeout. Refused by the manifest check when the role is not a build artifact.
    let plugin_json = match &m.plugin {
        None => Value::Null,
        Some(pd) => json!({
            "binary": binaries.get(&pd.role).cloned().unwrap_or(Value::Null),
            "args": pd.args,
            "env": [],
            "helloTimeoutMs": pd.hello_timeout_ms.unwrap_or(10_000),
        }),
    };
    let run_worker_hook = hooks.worker;
    let ws_ref = &ws;
    let template_ref = &template;
    let units_ref = &units;
    let runnable_ref = &runnable;
    let journeys_ref = journeys;
    let grants_ref = &grants;
    let m_ref = m;
    let results = Pool::new(size.workers).run(&jobs, &plan, |k, _job, world| {
        let started_at = now_iso();
        let t0 = Instant::now();
        let u = &units_ref[runnable_ref[k]];
        let j = &journeys_ref[u.journey];
        let g = &grants_ref[u.journey];
        let Ok(sc) = &u.scenario else {
            return Err("internal: a unit that did not load was scheduled".to_string());
        };
        let tier_budget = m_ref.budget.as_ref().and_then(|b| b.kind(&j.tier));
        let wsw = pool::world_workspace(ws_ref, template_ref, world, g)
            .map_err(|e| format!("the scenario world could not be built: {}", e.0))?;
        control::token::write_file(&world.control_token_file).map_err(|e| e.0)?;
        let runtime = json!({
            "config": cfg.config_path.to_string_lossy(),
            "tier": j.tier,
            "kind": sc.kind.map(|k| k.as_str()).unwrap_or("functional"),
            "killOrphans": false,
            "engine": engine,
            "ui": ui,
            "scenarioSha256": u.sha256,
            "plugin": plugin_json.clone(),
            "workspace": {
                "root": wsw.root.to_string_lossy(),
                "evidenceDir": wsw.evidence_dir.to_string_lossy(),
                "captureDir": wsw.capture_dir.to_string_lossy(),
                "env": wsw.env,
            },
        });
        let mut spec = pool::build_spec(
            &plan,
            world,
            SpecInputs {
                app: &m_ref.app,
                platform: &platform,
                scenario_id: &sc.name,
                scenario: json!({
                    "name": sc.name,
                    "file": sc.file.to_string_lossy(),
                    "target": sc.target,
                    "journey": j.id,
                    "binaries": Value::Object(binaries.clone()),
                    "runtime": runtime,
                }),
                tokens: g,
                scenario_ms: 0,
            },
        );
        spec.deadlines = deadlines_for(sc.timeout_s, tier_budget);
        let mut refusal = None;
        for art in &record.artifacts {
            if let Err(e) = record.verify_launch(&run.root, &art.role, cur) {
                refusal = Some(format!("refused before launch: {e}"));
                break;
            }
        }
        let outcome = match refusal {
            Some(r) => Err(Error(r)),
            None => run_worker_hook(&spec),
        };
        Ok(UnitRun {
            started_at,
            duration_ms: u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX),
            spec,
            outcome,
        })
    })?;
    // `Ok(Err(text))` is a unit that could not be set up: an `errored` scenario, never a pass.
    let mut reports: Vec<Option<UnitReport>> = (0..units.len()).map(|_| None).collect();
    for (k, res) in results.into_iter().enumerate() {
        let i = runnable[k];
        let world = plan.world(k).ok();
        let flat = match res {
            Err(why) => Err(why),
            Ok(Err(why)) => Err(why),
            Ok(Ok(r)) => Ok(r),
        };
        reports[i] = Some(judge_unit(&units[i].name, flat, world, &roles, &ws.home));
    }
    let mut reps: Vec<UnitReport> = reports
        .into_iter()
        .enumerate()
        .map(|(i, r)| {
            r.unwrap_or_else(|| {
                UnitReport::new(&units[i].name, None).errored(
                    units[i]
                        .scenario
                        .as_ref()
                        .err()
                        .cloned()
                        .unwrap_or_else(|| "the scenario never ran".into()),
                )
            })
        })
        .collect();
    // Measured again now: a tracked file edited, or a binary swapped, while scenarios ran.
    let cur_end = CurrentInputs::gather(app_root, &prepared.plan, &prepared.manifest_sha256)?;
    let mut problems = post_run_problems(record, &run.root, &cur_end, &mut reps);

    // Evidence: the runner's judgement beside the worker's raw files, copied into the run directory
    // (the world directories are disposed with the base workspace).
    for (i, r) in reps.iter_mut().enumerate() {
        let dir = run.root.join("units").join(format!("{:03}", i + 1));
        let _ = std::fs::create_dir_all(&dir);
        let judged = json!({
            "scenario": r.name, "journey": journeys[units[i].journey].id,
            "status": r.status, "message": r.message,
            "isolationProblems": r.isolation_problems, "roles": r.roles,
        });
        let settled = dir.join(SETTLED_FILE);
        if std::fs::write(
            &settled,
            serde_json::to_vec_pretty(&judged).unwrap_or_default(),
        )
        .is_ok()
        {
            r.evidence
                .extend(evidence_of(&run.root, &settled, EvidenceKind::Report));
        }
        if let Some(w) = &r.world {
            for (file, kind) in [
                (worker::OUTCOME_FILE, EvidenceKind::Report),
                (crate::worker_main::LOG_FILE, EvidenceKind::Log),
            ] {
                let to = dir.join(file);
                if std::fs::copy(w.dir.join(file), &to).is_ok() {
                    r.evidence.extend(evidence_of(&run.root, &to, kind));
                }
            }
        }
    }

    let mut out = vec![];
    let mut isolated = true;
    let mut any_error = false;
    for (ji, j) in journeys.iter().enumerate() {
        let mine: Vec<usize> = (0..units.len())
            .filter(|i| units[*i].journey == ji)
            .collect();
        let multi = mine.len() > 1;
        let label = |u: &str, s: &str| {
            if multi {
                format!("{u}/{s}")
            } else {
                s.to_string()
            }
        };
        let unit_rows: Vec<(String, JourneyStatus, String)> = mine
            .iter()
            .map(|i| {
                (
                    reps[*i].name.clone(),
                    status_of(reps[*i].status),
                    reps[*i].message.clone(),
                )
            })
            .collect();
        let started = mine
            .iter()
            .map(|i| reps[*i].started_at.clone())
            .min()
            .unwrap_or_else(now_iso);
        let duration: u64 = mine.iter().map(|i| reps[*i].duration_ms).sum();
        let evidence: Vec<Evidence> = mine
            .iter()
            .flat_map(|i| reps[*i].evidence.clone())
            .collect();
        let empty = journey_error[ji]
            .clone()
            .unwrap_or_else(|| "no scenario".into());
        let result = aggregate(&j.id, &unit_rows, started, duration, evidence, &empty);
        let mut steps = vec![];
        let mut by_env: BTreeMap<String, u32> = BTreeMap::new();
        let mut survivors: Vec<Survivor> = vec![];
        // The journey's engine-start counts: the sum of its units' `engine-starts.json` fields. One unit
        // that started engines without a readable file makes the whole journey count-less (fail closed).
        let mut engine_counts: Option<EngineStartCounts> = Some(EngineStartCounts::default());
        for i in &mine {
            let r = &reps[*i];
            if r.status == CheckStatus::Errored {
                any_error = true;
            }
            isolated &= r.isolation_problems.is_empty();
            if let Some(o) = &r.outcome {
                if o.engine_starts > 0 {
                    match engine_start_counts_of(o) {
                        Some(c) => {
                            if let Some(acc) = engine_counts.as_mut() {
                                acc.add(&c);
                            }
                        }
                        None => engine_counts = None,
                    }
                }
                for s in &o.steps {
                    steps.push(receipt::StepSample {
                        kind: s.kind.clone(),
                        step: label(&r.name, &s.label),
                        ms: s.ms,
                    });
                }
                if o.engine_starts_by_env.is_empty() {
                    if o.engine_starts > 0 {
                        *by_env.entry(label(&r.name, "default")).or_default() += o.engine_starts;
                    }
                } else {
                    for (k, n) in &o.engine_starts_by_env {
                        *by_env.entry(label(&r.name, k)).or_default() += n;
                    }
                }
                survivors.extend(o.survivors.iter().cloned());
            }
        }
        let rep_for_world = mine
            .iter()
            .map(|i| &reps[*i])
            .find(|r| r.outcome.as_ref().is_some_and(|o| o.launch.is_some()) && r.world.is_some())
            .or_else(|| mine.iter().map(|i| &reps[*i]).find(|r| r.world.is_some()));
        let world = rep_for_world.and_then(|r| {
            let w = r.world.as_ref()?;
            let launch = r.outcome.as_ref().and_then(|o| o.launch.as_ref());
            let (env, args, den, dar, probe): (Vec<(String, String)>, Vec<String>, _, _, _) =
                match launch {
                    Some(l) => (
                        l.env.clone(),
                        l.args.clone(),
                        l.data_dir_env.clone(),
                        l.data_dir_arg.clone(),
                        l.token_less_refused,
                    ),
                    None => (pool::world_env(w, &grants[ji]), vec![], None, None, None),
                };
            // N2: the file the app was actually given (the launch environment's one value, which the
            // suite wrote from the running app), not the world's file by assumption.
            let control_auth = probe.map(|refused| {
                let given = last_env(&env, control::token::ENV_AUTH_FILE)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| w.control_token_file.clone());
                receipt::ControlAuth {
                    token_file: given.to_string_lossy().into_owned(),
                    owner_only: rightkit_fs::private::verify_owner_only(&given).is_ok(),
                    token_less_refused: refused,
                }
            });
            let mut jw = JourneyWorld::from_world(
                w,
                ws.home.as_path(),
                LaunchFacts {
                    data_dir_env: den.as_deref(),
                    data_dir_arg: dar.as_deref(),
                    args: &args,
                    env: &env,
                    control_auth,
                },
            );
            // The directory the app was actually told, not the world's data root.
            if let Some(l) = launch {
                jw.data_dir = l.data_dir.to_string_lossy().into_owned();
            }
            Some(jw)
        });
        out.push(JourneyOut {
            journey: ReceiptJourney {
                id: j.id.clone(),
                status: result.status,
                release_critical: j.release_critical,
                started_at: result.started_at.clone(),
                duration_ms: result.duration_ms,
                units: result.units,
                evidence: result.evidence.clone(),
                reason: result.reason.clone(),
                steps: Some(steps),
                engine_starts: Some({
                    let mut e = EngineStarts::new(by_env);
                    e.counts = engine_counts;
                    e
                }),
                survivors: Some(survivors),
                tokens_held: Some(grants[ji].clone()),
                world,
            },
            result,
        });
    }
    let mut launched = BTreeMap::new();
    for role in reps.iter().flat_map(|r| r.roles.iter()) {
        if launched.contains_key(role) {
            continue;
        }
        if let Ok(c) = record.verify_launch(&run.root, role, &cur_end) {
            launched.insert(role.clone(), c);
        }
    }
    // The base workspace is finished here; its worlds were copied into the evidence above.
    let _ = ws.set_outcome(if any_error {
        RunOutcome::Failed
    } else {
        RunOutcome::Passed
    });
    let _ = ws.finish();
    if !any_error {
        let _ = workspace::dispose(&ws, true, 5, 200);
    }
    problems.dedup();
    Ok(Gathered {
        journeys: out,
        launched,
        isolated,
        adapter_name: ADAPTER_NAME.to_string(),
        adapter_argv: vec![],
        adapter_exit: i64::from(any_error),
        isolation_dir: ws.data_dir.clone(),

        problems,
    })
}

// ---- bridge runner

struct BridgeRun {
    started_at: String,
    duration_ms: u64,
    check: std::result::Result<crate::binary::LaunchCheck, String>,
    results: Result<Vec<super::adapter::JourneyResult>>,
    survivors: Vec<Survivor>,
}

/// The environment a bridged binary is given: the world's variables, the profile directories moved
/// into the world, and the data directory the receipt records.
fn bridge_env(
    world: &World,
    tokens: &[Token],
    run_token: &str,
) -> (BridgeEnv, Vec<(String, String)>) {
    let text = |p: &Path| p.to_string_lossy().into_owned();
    let mut extra: Vec<(String, String)> = pool::world_env(world, tokens)
        .into_iter()
        .filter(|(k, _)| *k != world.env_marker.name)
        .collect();
    let cache = world.home.join("cache");
    for (k, v) in [
        ("RIGHTKIT_QA_DATA_DIR", text(&world.data_dir)),
        ("HOME", text(&world.home)),
        ("USERPROFILE", text(&world.home)),
        ("APPDATA", text(&world.data_dir)),
        ("LOCALAPPDATA", text(&world.data_dir)),
        ("XDG_DATA_HOME", text(&world.data_dir)),
        ("XDG_CONFIG_HOME", text(&world.data_dir)),
        ("XDG_CACHE_HOME", text(&cache)),
        ("TMPDIR", text(&world.temp)),
        ("TMP", text(&world.temp)),
        ("TEMP", text(&world.temp)),
    ] {
        extra.push((k.to_string(), v));
    }
    let _ = std::fs::create_dir_all(&cache);
    // What the receipt records: the world marker (each bridged process carries `<marker>-<n>`),
    // the run token's presence is not recorded, only its name.
    let mut recorded = extra.clone();
    recorded.push((
        world.env_marker.name.clone(),
        world.env_marker.value.clone(),
    ));
    (
        BridgeEnv {
            world_marker_name: world.env_marker.name.clone(),
            world_marker_value: world.env_marker.value.clone(),
            run_token_name: RUN_TOKEN_ENV.to_string(),
            run_token_value: run_token.to_string(),
            extra,
        },
        recorded,
    )
}

#[allow(clippy::too_many_arguments)]
fn gather_bridge(
    prepared: &Prepared,
    m: &Manifest,
    journeys: &[Journey],
    app_root: &Path,
    cfg: Option<&QaConfig>,
    cur: &CurrentInputs,
    hooks: &Hooks<'_>,
    managed_root: &Path,
) -> Result<Gathered> {
    let run = &prepared.run;
    let record = &prepared.record;
    let host = cfg.map(|c| c.host.clone()).unwrap_or_default();
    // Which bridge entry and which recorded binary each journey runs.
    struct Plan {
        entry: Option<BridgeEntry>,
        decl: Option<manifest::BridgeDecl>,
        role: Option<String>,
        adapter: String,
        error: Option<String>,
    }
    let mut plans: Vec<Plan> = vec![];
    for j in journeys {
        let adapter = j.adapter.clone().unwrap_or_default();
        let mut p = Plan {
            entry: None,
            decl: None,
            role: None,
            adapter: adapter.clone(),
            error: None,
        };
        let target = m
            .adapter
            .iter()
            .find(|a| a.name == adapter)
            .and_then(|a| a.executable.cargo_target.clone());
        let decl = target
            .as_deref()
            .and_then(|t| m.bridge.iter().find(|b| b.binary == t))
            .cloned();
        match (decl, target) {
            (Some(d), Some(t)) => match record.artifacts.iter().find(|a| a.target == t) {
                Some(a) => {
                    p.role = Some(a.role.clone());
                    p.entry = Some(BridgeEntry {
                        app: d.app.clone(),
                        binary: run.root.join(&a.store_path),
                        removal_date: d.removal_date.clone(),
                        ignored_required: d.ignored_required.iter().cloned().collect(),
                        cwd: Some(app_root.to_path_buf()),
                    });
                    p.decl = Some(d);
                }
                None => p.error = Some(format!("the build record holds no binary for the bridge target {t:?}")),
            },
            _ => {
                p.error = Some(format!(
                    "adapter {adapter:?} has no [[bridge]] entry: `journeys run` runs an adapter journey only through the Harness bridge in this release"
                ))
            }
        }
        plans.push(p);
    }
    let shared: Vec<(usize, String)> = plans
        .iter()
        .enumerate()
        .filter_map(|(i, a)| {
            let d = a.decl.as_ref()?;
            plans
                .iter()
                .enumerate()
                .any(|(k, b)| k != i && b.decl.as_ref().is_some_and(|x| x.app == d.app))
                .then(|| (i, d.app.clone()))
        })
        .collect();
    for (i, app) in shared {
        plans[i].error = Some(format!(
            "two selected journeys name the bridge entry for {app:?}; a bridge cannot split its tests between journeys"
        ));
    }
    let runnable: Vec<usize> = (0..journeys.len())
        .filter(|i| plans[*i].error.is_none())
        .collect();
    let grants: Vec<Vec<Token>> = journeys.iter().map(tokens_of).collect();
    let jobs: Vec<Job> = runnable
        .iter()
        .map(|i| Job {
            id: journeys[*i].id.clone(),
            tokens: grants[*i].clone(),
        })
        .collect();
    let size = pool::host_pool_size(&host, jobs.len(), Some(BRIDGE_SERIAL_REASON));
    let worlds_root = prepared.run.root.join("bridge");
    std::fs::create_dir_all(&worlds_root)?;
    let plan = WorldPlan::new(
        run.marker.run_id.clone(),
        worlds_root.clone(),
        &host,
        jobs.len(),
    )?;
    let run_token = control::token::mint()?;
    let exec = hooks.bridge_exec;
    let concurrency = host.workers.unwrap_or(1).max(1);
    let lock_dir = managed_root.join("locks").join(format!("bridge-{}", m.app));
    let plans_ref = &plans;
    let grants_ref = &grants;
    let runnable_ref = &runnable;
    let results = Pool::new(size.workers).run(&jobs, &plan, |k, _job, world| {
        let i = runnable_ref[k];
        let p = &plans_ref[i];
        let started_at = now_iso();
        let t0 = Instant::now();
        let finish = |check, results, survivors| BridgeRun {
            started_at: started_at.clone(),
            duration_ms: u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX),
            check,
            results,
            survivors,
        };
        let (Some(entry), Some(role), Some(decl)) = (&p.entry, &p.role, &p.decl) else {
            return finish(
                Err("internal: no bridge plan".into()),
                err("no bridge plan"),
                vec![],
            );
        };
        let check = record
            .verify_launch(&run.root, role, cur)
            .map_err(|e| format!("refused before launch: {e}"));
        if let Err(e) = &check {
            return finish(check.clone(), err(e.clone()), vec![]);
        }
        let (env, _) = bridge_env(world, &grants_ref[i], &run_token);
        let tracker = Tracker::new();
        let seen: std::sync::Mutex<Vec<Survivor>> = std::sync::Mutex::new(vec![]);
        let wrapped = |inv: &bridge::Invocation| -> Result<bridge::RawRun> {
            let raw = exec(&tracker, inv)?;
            if raw.stderr.contains("outlived") {
                let line = raw
                    .stderr
                    .lines()
                    .find(|l| l.contains("outlived"))
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if let Ok(mut g) = seen.lock() {
                    g.push(Survivor {
                        pid: 0,
                        name: inv.label.clone(),
                        owner: SurvivorOwner::RegisteredRoot,
                        detail: line,
                    });
                }
            }
            Ok(raw)
        };
        let opts = BridgeOptions {
            concurrency,
            selected: if decl.tests.is_empty() {
                None
            } else {
                Some(decl.tests.clone())
            },
            run_lock: Some((
                lock_dir.clone(),
                LockOptions {
                    timeout: BRIDGE_LOCK_WAIT,
                    ..LockOptions::default()
                },
            )),
            ..BridgeOptions::default()
        };
        let results = bridge::run_entry(entry, &env, &opts, &wrapped);
        let mut survivors = seen.into_inner().unwrap_or_default();
        for (pid, label) in tracker.alive() {
            survivors.push(Survivor {
                pid,
                name: label,
                owner: SurvivorOwner::RegisteredRoot,
                detail: String::new(),
            });
        }
        tracker.kill_all();
        finish(check, results, survivors)
    })?;

    let cur_end = CurrentInputs::gather(app_root, &prepared.plan, &prepared.manifest_sha256)?;
    let mut out = vec![];
    let mut launched = BTreeMap::new();
    let mut isolated = true;
    let mut any_error = false;
    let mut argv = vec![];
    let mut adapter_name = String::new();
    let mut pooled: Vec<Option<std::result::Result<BridgeRun, String>>> =
        (0..journeys.len()).map(|_| None).collect();
    for (k, res) in results.into_iter().enumerate() {
        pooled[runnable[k]] = Some(res);
    }
    for (i, j) in journeys.iter().enumerate() {
        let p = &plans[i];
        if adapter_name.is_empty() {
            adapter_name = p.adapter.clone();
        }
        let world = runnable
            .iter()
            .position(|r| *r == i)
            .and_then(|k| plan.world(k).ok());
        #[allow(clippy::type_complexity)]
        let (mut rows, mut started, mut duration, mut survivors, mut evidence): (
            Vec<(String, JourneyStatus, String)>,
            String,
            u64,
            Vec<Survivor>,
            Vec<Evidence>,
        ) = (vec![], now_iso(), 0, vec![], vec![]);
        let mut empty = p
            .error
            .clone()
            .unwrap_or_else(|| "the bridge ran no test".into());
        match pooled[i].take() {
            None => {}
            Some(Err(why)) => empty = format!("the bridge job could not run: {why}"),
            Some(Ok(b)) => {
                started = b.started_at.clone();
                duration = b.duration_ms;
                survivors = b.survivors.clone();
                match (&b.check, &b.results) {
                    (Err(e), _) => empty = e.clone(),
                    (_, Err(e)) => empty = e.0.clone(),
                    (Ok(c), Ok(rs)) => {
                        let again = match &p.role {
                            Some(role) => record
                                .verify_launch(&run.root, role, &cur_end)
                                .err()
                                .map(|e| (role.clone(), e)),
                            None => None,
                        };
                        if let Some((role, e)) = again {
                            empty = format!("binary {role} changed during the run: {e}");
                        } else {
                            if let Some(role) = &p.role {
                                launched.insert(role.clone(), c.clone());
                            }
                            argv = vec![c.path.to_string_lossy().into_owned()];
                            rows = rs
                                .iter()
                                .map(|r| {
                                    (r.id.clone(), r.status, r.reason.clone().unwrap_or_default())
                                })
                                .collect();
                        }
                        if !rows.is_empty() && world.is_some() {
                            let body: Vec<Value> = rs
                                .iter()
                                .map(|r| json!({"test": r.id, "status": r.status, "duration_ms": r.duration_ms, "reason": r.reason}))
                                .collect();
                            let to = run.root.join(format!("bridge-{}.json", j.id));
                            if std::fs::write(
                                &to,
                                serde_json::to_vec_pretty(&body).unwrap_or_default(),
                            )
                            .is_ok()
                            {
                                evidence.extend(evidence_of(&run.root, &to, EvidenceKind::Report));
                            }
                        }
                    }
                }
            }
        }
        let result = aggregate(&j.id, &rows, started, duration, evidence, &empty);
        if result.status == JourneyStatus::Errored {
            any_error = true;
        }
        let jworld = world.as_ref().map(|w| {
            let (_, recorded) = bridge_env(w, &grants[i], "recorded");
            let recorded: Vec<(String, String)> = recorded
                .into_iter()
                .filter(|(k, _)| k != RUN_TOKEN_ENV)
                .collect();
            JourneyWorld::from_world(
                w,
                worlds_root.as_path(),
                LaunchFacts {
                    data_dir_env: Some("RIGHTKIT_QA_DATA_DIR"),
                    data_dir_arg: None,
                    args: &[],
                    env: &recorded,
                    control_auth: None,
                },
            )
        });
        if jworld.is_none() {
            isolated = false;
        }
        out.push(JourneyOut {
            journey: ReceiptJourney {
                id: j.id.clone(),
                status: result.status,
                release_critical: j.release_critical,
                started_at: result.started_at.clone(),
                duration_ms: result.duration_ms,
                units: result.units,
                evidence: result.evidence.clone(),
                reason: result.reason.clone(),
                steps: None,
                engine_starts: None,
                survivors: Some(survivors),
                tokens_held: Some(grants[i].clone()),
                world: jworld,
            },
            result,
        });
    }
    Ok(Gathered {
        journeys: out,
        launched,
        isolated,
        adapter_name,
        adapter_argv: argv,
        adapter_exit: i64::from(any_error),
        isolation_dir: plan.worlds_dir(),

        problems: vec![],
    })
}

// ---- the receipt

fn results_text(
    prepared: &Prepared,
    g: &Gathered,
    started_at: &str,
    ended_at: &str,
) -> Result<String> {
    let run_id = prepared.run.marker.run_id.clone();
    let nonce = prepared.nonce.clone();
    let artifacts: Vec<ArtifactDigest> = g
        .launched
        .iter()
        .map(|(role, c)| ArtifactDigest {
            role: role.clone(),
            path: c.path.to_string_lossy().into_owned(),
            sha256: c.sha256.clone(),
        })
        .collect();
    let mut recs = vec![Record::Start(StartRecord {
        protocol: PROTOCOL,
        run_id: run_id.clone(),
        nonce: nonce.clone(),
        adapter: g.adapter_name.clone(),
        adapter_version: env!("CARGO_PKG_VERSION").to_string(),
        started_at: started_at.to_string(),
        artifacts,
        isolation: Isolation {
            data_dir: g.isolation_dir.to_string_lossy().into_owned(),
            guard: if g.isolated { "passed" } else { "failed" }.into(),
            qa_mode: "runtime".into(),
        },
    })];
    for j in &g.journeys {
        let r = &j.result;
        recs.push(Record::Journey(super::adapter::JourneyRecord {
            run_id: run_id.clone(),
            nonce: nonce.clone(),
            id: r.id.clone(),
            status: r.status,
            started_at: r.started_at.clone(),
            duration_ms: r.duration_ms,
            units: r.units,
            evidence: r.evidence.clone(),
            reason: r.reason.clone(),
        }));
    }
    recs.push(Record::End(EndRecord {
        run_id,
        nonce,
        selected: g.journeys.len() as u64,
        reported: g.journeys.len() as u64,
        ended_at: ended_at.to_string(),
    }));
    let mut text = String::new();
    for r in &recs {
        text.push_str(&serde_json::to_string(r)?);
        text.push('\n');
    }
    Ok(text)
}

/// Build the receipt from what the run observed: every truthful section from the build record, the
/// outcomes the supervisor collected and the manifest, never from a claim.
#[allow(clippy::too_many_arguments)]
fn build_receipt(
    prepared: &Prepared,
    m: &Manifest,
    g: &Gathered,
    started_at: &str,
    ended_at: &str,
    results: &[u8],
    mapping: Option<FileRef>,
    record_sha: &str,
    journeys: &[Journey],
    managed_root: &Path,
) -> Result<Receipt> {
    let rec = &prepared.record;
    let run_dir = &prepared.run.root;
    let rjs: Vec<ReceiptJourney> = g.journeys.iter().map(|j| j.journey.clone()).collect();
    let tiers: BTreeSet<String> = journeys.iter().map(|j| j.tier.clone()).collect();
    let history = host_history(
        managed_root,
        &prepared.record.app,
        &prepared.run.marker.run_id,
        &rec.host_os,
        &rec.host_arch,
    );
    let tier_text = prepared.selection.tier.clone();
    let budget = budget_for(m, &tiers, &rjs, &history, &tier_text, ended_at)?;
    let samples: Vec<receipt::StepSample> = rjs
        .iter()
        .flat_map(|j| j.steps.iter().flatten().cloned())
        .collect();
    let launched: Vec<Launched> = rec
        .artifacts
        .iter()
        .filter_map(|a| g.launched.get(&a.role).map(|c| (a, c)))
        .map(|(a, c)| Launched {
            role: a.role.clone(),
            path: c.path.to_string_lossy().into_owned(),
            size: c.size,
            sha256: c.sha256.clone(),
        })
        .collect();
    let mut r = Receipt {
        schema: RECEIPT_SCHEMA.to_string(),
        run_id: prepared.run.marker.run_id.clone(),
        nonce: prepared.nonce.clone(),
        app: m.app.clone(),
        kind: ReceiptKind::Functional,
        verdict: Verdict::Passed,
        started_at: started_at.to_string(),
        ended_at: ended_at.to_string(),
        host: Host {
            os: rec.host_os.clone(),
            arch: rec.host_arch.clone(),
            session: "interactive".into(),
        },
        source: rec.source.clone(),
        manifest: FileRef {
            path: MANIFEST_FILE.into(),
            sha256: prepared.manifest_sha256.clone(),
        },
        mapping,
        build: rec.to_receipt_build(run_dir),
        launched,
        isolation: ReceiptIsolation {
            data_dir: g.isolation_dir.to_string_lossy().into_owned(),
            guard: if g.isolated { "passed" } else { "failed" }.into(),
            qa_mode: "runtime".into(),
            real_data_touched: false,
        },
        adapter: AdapterRun {
            name: g.adapter_name.clone(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            protocol: PROTOCOL,
            argv: g.adapter_argv.clone(),
            exit_code: g.adapter_exit,
            results_sha256: sha256_hex(results),
        },
        selection: ReceiptSelection {
            ids: prepared.selection.ids(),
            platform: prepared.selection.platform.clone(),
            tier: tier_text,
        },
        journeys: rjs,
        counts: Counts::default(),
        problems: vec![],
        runner: Some(receipt::RUNNER.to_string()),
        runner_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        build_record: Some(BuildRecordRef::from_record(
            rec,
            BUILD_RECORD_FILE,
            record_sha,
        )),
        timing: Some(ReceiptTiming::from_samples(samples.iter())),
        budget,
        explore: None,
    };
    r.counts = Counts::default();
    Ok(r)
}

/// Set the receipt's counts, verdict and problems from the validator's recomputation, never from
/// the runner's own opinion: counts first, then the verdict from the problems that are not the
/// verdict's own mismatch.
fn settle_receipt(r: &mut Receipt, o: &ValidateOptions<'_>) {
    r.verdict = Verdict::Passed;
    r.problems.clear();
    let first = validate_receipt(r, o);
    r.counts = first.counts;
    let second = validate_receipt(r, o);
    r.verdict = second.verdict;
    r.problems = second
        .problems
        .into_iter()
        .filter(|p| p.code != code::VERDICT_MISMATCH)
        .collect();
}

fn finish_with_error(prepared: &Prepared, cap: StoreCap, e: &Error) -> CleanupReport {
    let _ = std::fs::write(prepared.run.root.join(RUN_ERROR_FILE), format!("{e}\n"));
    finish_run(&prepared.run, RunOutcome::Failed, cap, false)
}

/// Steps 1 to 5 of `journeys run` with `hooks` standing in for the effects. See the module docs.
pub fn run_with(opts: &JourneyRunOptions, hooks: &Hooks<'_>) -> Result<RunReport> {
    if let RunMode::Explore(rec) = &opts.mode {
        // An explore run records a scenario. It builds nothing, launches nothing, creates no run
        // and writes no receipt.
        let path = receipt::write_explore_scenario(
            &rec.dir,
            &rec.name,
            &rec.target,
            &rec.tokens,
            &rec.steps,
        )?;
        return Ok(RunReport {
            explored: Some(path),
            ..RunReport::default()
        });
    }
    if let Some(reason) = (hooks.session)() {
        return err(format!(
            "journeys run needs the interactive desktop session, and this process is not in it ({reason}); a hidden app cannot get a ready window here and a pass from here would be a lie. Start it through `rightkit-qa-interactive -- rightkit-qa journeys run ...`"
        ));
    }
    let started_at = now_iso();
    let prepared = prepare_with(&opts.prepare, hooks.broker)?;
    let cap = opts.prepare.store_cap;
    match collect(opts, hooks, &prepared, &started_at) {
        Ok(report) => Ok(report),
        Err(e) => {
            let _ = finish_with_error(&prepared, cap, &e);
            Err(e)
        }
    }
}

fn collect(
    opts: &JourneyRunOptions,
    hooks: &Hooks<'_>,
    prepared: &Prepared,
    started_at: &str,
) -> Result<RunReport> {
    let app_root = canonical_plain(&opts.prepare.app_root).map_err(|e| {
        Error(format!(
            "cannot resolve {}: {e}",
            opts.prepare.app_root.display()
        ))
    })?;
    let managed_root = storage::managed_root(opts.prepare.managed_root.as_deref());
    let run = &prepared.run;
    let record = &prepared.record;
    let bytes = std::fs::read(app_root.join(MANIFEST_FILE))
        .map_err(|e| Error(format!("cannot read {MANIFEST_FILE}: {e}")))?;
    let (m, sha) = manifest::parse_manifest(&bytes)?;
    if sha != prepared.manifest_sha256 {
        return err(format!(
            "{MANIFEST_FILE} changed during the run (sha256 {sha}, the run was built against {})",
            prepared.manifest_sha256
        ));
    }
    let mut journeys = vec![];
    for id in prepared.selection.ids() {
        match manifest::find(&m, &id) {
            Some(j) => journeys.push(j.clone()),
            None => return err(format!("selected journey {id} is not in {MANIFEST_FILE}")),
        }
    }
    let cfg_path = app_root.join("rightkit-qa.toml");
    let cfg = if cfg_path.is_file() {
        Some(QaConfig::load(&cfg_path)?)
    } else {
        None
    };
    // The launch gate: the record must still describe the checkout before anything is started.
    let cur = CurrentInputs::gather(&app_root, &prepared.plan, &prepared.manifest_sha256)?;
    let stale = record.stale_against(&cur);
    if !stale.is_empty() {
        return err(stale
            .iter()
            .map(|p| format!("{}: {}", p.code, p.detail))
            .collect::<Vec<_>>()
            .join("; "));
    }
    let adapter_run = journeys.iter().any(|j| j.runner == "adapter");
    let g = if adapter_run {
        gather_bridge(
            prepared,
            &m,
            &journeys,
            &app_root,
            cfg.as_ref(),
            &cur,
            hooks,
            &managed_root,
        )?
    } else {
        let Some(cfg) = cfg.as_ref() else {
            return err("journeys run needs rightkit-qa.toml in the app root: the scenario workers read their engine and data-directory settings from it");
        };
        gather_scenarios(
            prepared,
            &m,
            &journeys,
            &app_root,
            cfg,
            &cur,
            hooks,
            &managed_root,
        )?
    };
    for p in &g.problems {
        eprintln!("[qa] {p}");
    }
    let ended_at = now_iso();
    let results = results_text(prepared, &g, started_at, &ended_at)?;
    let results_path = run.root.join(RESULTS_FILE);
    std::fs::write(&results_path, results.as_bytes())
        .map_err(|e| Error(format!("cannot write {}: {e}", results_path.display())))?;
    let mapping = std::fs::read(app_root.join(manifest::MAPPING_FILE))
        .ok()
        .map(|b| FileRef {
            path: manifest::MAPPING_FILE.into(),
            sha256: sha256_hex(&b),
        });
    let record_sha = sha256_hex(&std::fs::read(&prepared.record_path).map_err(|e| {
        Error(format!(
            "cannot read {}: {e}",
            prepared.record_path.display()
        ))
    })?);
    let mut receipt = build_receipt(
        prepared,
        &m,
        &g,
        started_at,
        &ended_at,
        results.as_bytes(),
        mapping,
        &record_sha,
        &journeys,
        &managed_root,
    )?;
    let tree = receipt.source.tree.clone();
    let vo = ValidateOptions {
        expected_tree: Some(tree.as_str()),
        manifest: Some((&m, prepared.manifest_sha256.as_str())),
        results: Some(results.as_bytes()),
        evidence_root: Some(run.root.as_path()),
        allow_dirty: false,
    };
    settle_receipt(&mut receipt, &vo);
    let receipt_path = run.root.join(RECEIPT_FILE);
    receipt::write_receipt(&receipt_path, &receipt)?;
    // Validate what is on disk, not what is in memory.
    let on_disk = receipt::parse_receipt(
        &std::fs::read(&receipt_path)
            .map_err(|e| Error(format!("cannot read {}: {e}", receipt_path.display())))?,
    )?;
    let on_disk_results = std::fs::read(&results_path)
        .map_err(|e| Error(format!("cannot read {}: {e}", results_path.display())))?;
    let v = validate_receipt(
        &on_disk,
        &ValidateOptions {
            expected_tree: Some(tree.as_str()),
            manifest: Some((&m, prepared.manifest_sha256.as_str())),
            results: Some(on_disk_results.as_slice()),
            evidence_root: Some(run.root.as_path()),
            allow_dirty: false,
        },
    );
    let passed = v.passed();
    let slow: Vec<String> = on_disk
        .budget
        .iter()
        .flat_map(|b| b.verdicts.iter())
        .filter(|e| e.outcome == receipt::BudgetOutcome::Slow)
        .map(|e| format!("{} {} ms ({})", e.label, e.ms, e.journey))
        .collect();
    let cleanup = finish_run(
        run,
        if passed {
            RunOutcome::Passed
        } else {
            RunOutcome::Failed
        },
        opts.prepare.store_cap,
        passed,
    );
    for e in &cleanup.errors {
        eprintln!("[qa] cleanup: {e}");
    }
    Ok(RunReport {
        run_id: run.marker.run_id.clone(),
        run_dir: run.root.clone(),
        receipt_path: Some(receipt_path),
        results_path: Some(results_path),
        explored: None,
        verdict: Some(v.verdict),
        passed,
        problems: v.problems,
        counts: Some(v.counts),
        slow,
        cleanup,
    })
}

// ---------------------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binary::{LaunchRefusal, QA_CONTROL_MARKER};
    use crate::journeys::build_record::RecordRefusal;

    const REQ: &str = "096d6843-aaaa-4bbb-8ccc-0123456789ab";

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rkqa-run-{tag}-{}", new_id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn argv(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    const MANIFEST: &str = r#"
version = 1
app = "fixtureapp"

[[adapter]]
name = "fx"
build = ["rustright", "cargo", "test", "-p", "fx", "--no-run", "--features", "qa", "--message-format=json"]
executable = { role = "adapter", cargo_target = "journeys_adapter" }
artifacts = [{ role = "app", cargo_package = "fxapp" }]

[[adapter]]
name = "other"
build = ["rustright", "cargo", "build", "-p", "other"]
executable = { role = "adapter", cargo_target = "other" }

[[adapter]]
name = "prebuilt"
build = ["rustright", "cargo", "build", "-p", "p"]
executable = { role = "adapter", path = "bin/adapter" }

[[journey]]
id = "J-001"
title = "all platforms, fast"
platforms = ["macos", "windows", "linux"]
surface = "engine"
runner = "adapter"
adapter = "fx"
entry = "tests/j1.scenario.toml"
tier = "fast"
data = "generated"

[[journey]]
id = "J-002"
title = "windows, full"
platforms = ["windows"]
surface = "engine"
runner = "adapter"
adapter = "fx"
entry = "tests/j1.scenario.toml"
tier = "full"
data = "generated"

[[journey]]
id = "J-003"
title = "macos, fast"
platforms = ["macos"]
surface = "engine"
runner = "adapter"
adapter = "fx"
entry = "tests/j1.scenario.toml"
tier = "fast"
data = "generated"

[[journey]]
id = "J-004"
title = "scenario runner"
platforms = ["macos", "windows", "linux"]
surface = "engine"
runner = "scenario"
entry = "tests/j1.scenario.toml"
tier = "fast"
data = "generated"

[[journey]]
id = "J-005"
title = "other adapter"
platforms = ["macos", "windows", "linux"]
surface = "engine"
runner = "adapter"
adapter = "other"
entry = "tests/j1.scenario.toml"
tier = "fast"
data = "generated"

[[journey]]
id = "J-006"
title = "prebuilt adapter"
platforms = ["macos", "windows", "linux"]
surface = "engine"
runner = "adapter"
adapter = "prebuilt"
entry = "tests/j1.scenario.toml"
tier = "fast"
data = "generated"
"#;

    fn manifest() -> Manifest {
        manifest::parse_manifest(MANIFEST.as_bytes()).unwrap().0
    }

    fn ids(s: &Selected) -> Vec<&str> {
        s.journeys.iter().map(|j| j.id.as_str()).collect()
    }

    fn select(
        ids: &[&str],
        platform: Option<&str>,
        tier: Option<&str>,
        host: &str,
    ) -> Result<Selected> {
        select_journeys(
            &manifest(),
            &SelectionRequest {
                ids: ids.iter().map(|s| s.to_string()).collect(),
                platform: platform.map(str::to_string),
                tier: tier.map(str::to_string),
            },
            host,
        )
    }

    #[test]
    fn journeys_are_selected_per_platform_and_tier_up_front() {
        // Default tier is fast: the windows-only full journey is not selected.
        let s = select(&[], None, None, "windows").unwrap();
        assert_eq!(ids(&s), ["J-001", "J-004", "J-005", "J-006"]);
        assert_eq!((s.platform.as_str(), s.tier.as_str()), ("windows", "fast"));
        let s = select(&[], None, Some("full,fast"), "windows").unwrap();
        assert_eq!(ids(&s), ["J-001", "J-002", "J-004", "J-005", "J-006"]);
        assert_eq!(s.tier, "fast,full");
        // macOS gets its own journey and not the windows one.
        let s = select(&[], None, None, "macos").unwrap();
        assert!(ids(&s).contains(&"J-003") && !ids(&s).contains(&"J-002"));
        // Explicit ids keep the order asked for.
        assert_eq!(
            ids(&select(&["J-005", "J-001"], None, None, "linux").unwrap()),
            ["J-005", "J-001"]
        );
    }

    #[test]
    fn selection_never_drops_a_journey_quietly() {
        let e = select(&["J-003"], None, None, "windows").unwrap_err();
        assert!(e.0.contains("does not declare platform windows"), "{}", e.0);
        assert!(select(&["J-002"], None, None, "windows")
            .unwrap_err()
            .0
            .contains("is tier full"));
        assert!(select(&["J-999"], None, None, "windows")
            .unwrap_err()
            .0
            .contains("not in journeys.toml"));
        assert!(select(&["J-001", "J-001"], None, None, "windows")
            .unwrap_err()
            .0
            .contains("twice"));
        assert!(select(&[], Some("macos"), None, "windows")
            .unwrap_err()
            .0
            .starts_with("host_mismatch"));
        assert!(select(&[], Some("beos"), None, "windows").is_err());
        assert!(select(&[], None, Some("slow"), "windows").is_err());
        // Zero selected is an error.
        let zero = select(&[], None, Some("paid"), "linux").unwrap_err();
        assert!(zero.0.contains("zero journeys selected"), "{}", zero.0);
    }

    #[test]
    fn required_ids_are_fixed_per_platform_from_the_whole_manifest() {
        let r = required_by_platform(&manifest());
        assert_eq!(r["windows"], ["J-001", "J-002", "J-004", "J-005", "J-006"]);
        assert_eq!(r["macos"], ["J-001", "J-003", "J-004", "J-005", "J-006"]);
        assert_eq!(r["linux"], ["J-001", "J-004", "J-005", "J-006"]);
    }

    #[test]
    fn the_build_plan_comes_from_one_adapter_and_refuses_what_it_cannot_bind() {
        let m = manifest();
        let s = select(&["J-001"], None, None, "windows").unwrap();
        let plan = plan_build(&m, &s, Path::new("/app")).unwrap();
        assert_eq!(plan.name, "fx");
        assert_eq!(plan.artifacts.len(), 2);
        assert_eq!(plan.artifacts[0].role, "adapter");
        assert_eq!(plan.artifacts[0].marker, MarkerPolicy::NotApplicable);
        assert_eq!(plan.artifacts[1].role, "app");
        assert_eq!(plan.artifacts[1].marker, MarkerPolicy::Required);
        assert_eq!(plan.artifacts[1].cargo_package.as_deref(), Some("fxapp"));
        // A scenario-runner journey has no build declaration in this release.
        let e = plan_build(
            &m,
            &select(&["J-004"], None, None, "windows").unwrap(),
            Path::new("/app"),
        )
        .unwrap_err();
        assert!(e.0.contains("runner \"scenario\""), "{}", e.0);
        // Two adapters in one run are refused.
        let e = plan_build(
            &m,
            &select(&["J-001", "J-005"], None, None, "windows").unwrap(),
            Path::new("/app"),
        )
        .unwrap_err();
        assert!(e.0.contains("exactly one adapter"), "{}", e.0);
        // A path-only artifact cannot be bound to this build.
        let e = plan_build(
            &m,
            &select(&["J-006"], None, None, "windows").unwrap(),
            Path::new("/app"),
        )
        .unwrap_err();
        assert!(e.0.starts_with("artifact_not_from_build"), "{}", e.0);
    }

    #[test]
    fn qualifying_mode_refuses_build_build_ui_and_kill_orphans() {
        let none = |_: &str| None;
        assert!(!is_qualifying(&argv("run --build"), &none));
        assert!(is_qualifying(&argv("run --qualifying"), &none));
        for v in ["1", "true", "YES", " on "] {
            let env = move |k: &str| (k == QUALIFYING_ENV).then(|| v.to_string());
            assert!(is_qualifying(&argv("run"), &env), "{v}");
        }
        let off = |k: &str| (k == QUALIFYING_ENV).then(|| "0".to_string());
        assert!(!is_qualifying(&argv("run"), &off));
        for flag in [
            "--build",
            "--build-ui",
            "--kill-orphans",
            "--build=1",
            "--kill-orphans=true",
        ] {
            let e =
                refuse_flags_in_qualifying(&argv(&format!("run --tier fast {flag}"))).unwrap_err();
            assert!(
                e.0.contains("is refused in a qualifying run"),
                "{flag}: {}",
                e.0
            );
        }
        assert!(refuse_flags_in_qualifying(&argv("run --tier fast --keep-data")).is_ok());
        // A flag value that merely resembles a flag name is not a flag.
        assert!(refuse_flags_in_qualifying(&argv("run --only build-ui-tests")).is_ok());
        // The same refusal for options a library caller built.
        let mut o = SuiteOptions::default();
        assert!(refuse_options_in_qualifying(&o).is_ok());
        o.build_engine = true;
        assert!(refuse_options_in_qualifying(&o)
            .unwrap_err()
            .0
            .starts_with("--build is refused"));
        let o = SuiteOptions {
            build_ui: true,
            ..Default::default()
        };
        assert!(refuse_options_in_qualifying(&o)
            .unwrap_err()
            .0
            .starts_with("--build-ui is refused"));
        let o = SuiteOptions {
            kill_orphans: true,
            ..Default::default()
        };
        let e = refuse_options_in_qualifying(&o).unwrap_err();
        assert!(
            e.0.starts_with("--kill-orphans is refused") && e.0.contains("never killed quietly"),
            "{}",
            e.0
        );
    }

    #[test]
    fn the_store_cap_takes_the_flag_then_the_env_then_the_default() {
        let none = |_: &str| None;
        let env = |k: &str| (k == RUN_STORE_CAP_ENV).then(|| "2_000".to_string());
        assert_eq!(
            StoreCap::resolve(None, &none).unwrap(),
            StoreCap(DEFAULT_RUN_STORE_CAP_BYTES)
        );
        assert_eq!(StoreCap::resolve(None, &env).unwrap(), StoreCap(2000));
        assert_eq!(
            StoreCap::resolve(Some("5000"), &env).unwrap(),
            StoreCap(5000)
        );
        for bad in ["0", "-1", "ten", "1.5", ""] {
            assert!(StoreCap::resolve(Some(bad), &none).is_err(), "{bad:?}");
        }
        let blank = |k: &str| (k == RUN_STORE_CAP_ENV).then(|| "  ".to_string());
        assert_eq!(
            StoreCap::resolve(None, &blank).unwrap().0,
            DEFAULT_RUN_STORE_CAP_BYTES
        );
    }

    // ------------------------------------------------------------------ run store cleanup

    fn run_with_bin(managed: &Path, bytes: usize, finish_it: bool) -> ManagedRun {
        let run = storage::create(Some(managed), "app", None, Duration::from_secs(60)).unwrap();
        let bin = run.root.join(STORE_DIR).join("app");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("app.exe"), vec![7u8; bytes]).unwrap();
        std::fs::write(run.root.join("evidence").join("e.txt"), vec![1u8; 100]).unwrap();
        if finish_it {
            storage::set_run_outcome(&run.root, RunOutcome::Failed).unwrap();
            storage::finish(&run).unwrap();
            std::thread::sleep(Duration::from_millis(25));
        }
        run
    }

    #[test]
    fn cleanup_removes_the_oldest_binary_copies_until_the_store_fits() {
        let managed = tmp("cleanup");
        let a = run_with_bin(&managed, 5000, true);
        let b = run_with_bin(&managed, 5000, true);
        let c = run_with_bin(&managed, 5000, false); // still running: never touched
        let id_c = c.marker.run_id.clone();
        let total = storage::directory_bytes(&managed.join("runs").join("app")).unwrap();
        assert!(total > 15_000);
        // Under the cap: nothing is removed.
        let r = cleanup_store(&managed, "app", StoreCap(total + 2000), Some(&id_c));
        assert!(
            r.removed.is_empty() && r.over_cap_bytes == 0,
            "{}",
            r.summary()
        );
        // A cap that one removal satisfies removes only the oldest.
        let r = cleanup_store(&managed, "app", StoreCap(total - 4000), Some(&id_c));
        assert_eq!(r.removed.len(), 1, "{}", r.summary());
        assert!(!a.root.join(STORE_DIR).exists());
        assert!(b.root.join(STORE_DIR).exists());
        assert_eq!(r.over_cap_bytes, 0);
        // Evidence is never removed by the cap.
        assert!(a.root.join("evidence").join("e.txt").is_file());
        // A cap nothing can satisfy removes every finished copy, spares the running run and reports the excess.
        let r = cleanup_store(&managed, "app", StoreCap(100), Some(&id_c));
        assert!(!b.root.join(STORE_DIR).exists());
        assert!(c.root.join(STORE_DIR).exists());
        assert!(r.over_cap_bytes > 0, "{}", r.summary());
        assert!(r.summary().contains("OVER the cap"));
    }

    #[test]
    fn a_prepared_run_is_closed_without_an_outcome_and_keeps_its_record_until_the_cap_needs_room() {
        let managed = tmp("closed");
        let run = run_with_bin(&managed, 3000, false);
        let report = close_prepared(&run, StoreCap(DEFAULT_RUN_STORE_CAP_BYTES));
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        let marker = storage::read_marker(&run.root.join(storage::RUN_MARKER)).unwrap();
        assert_eq!(marker.outcome, None);
        assert!(marker.completed_at.is_some() && marker.owner_pid == 0);
        assert!(
            run.root.join(STORE_DIR).exists(),
            "the copies stay while the store fits"
        );
        // The next run's pre-check reclaims them once the cap is exceeded; the evidence stays.
        let r = cleanup_store(&managed, "app", StoreCap(100), None);
        assert_eq!(r.removed.len(), 1, "{}", r.summary());
        assert!(!run.root.join(STORE_DIR).exists());
        assert!(run.root.join("evidence").join("e.txt").is_file());
    }

    #[test]
    fn a_finished_run_drops_its_own_binaries_and_a_skipped_run_leaves_nothing() {
        let managed = tmp("after");
        let run = run_with_bin(&managed, 3000, false);
        let report = finish_run(
            &run,
            RunOutcome::Failed,
            StoreCap(DEFAULT_RUN_STORE_CAP_BYTES),
            true,
        );
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(
            !run.root.join(STORE_DIR).exists(),
            "the run's own binary copies go after the run"
        );
        assert!(
            run.root.join("evidence").join("e.txt").is_file(),
            "a failed run keeps its evidence"
        );
        // A run in which nothing executed is removed whole by the ordinary retention rule.
        let skipped = run_with_bin(&managed, 3000, false);
        let report = finish_run(
            &skipped,
            RunOutcome::Skipped,
            StoreCap(DEFAULT_RUN_STORE_CAP_BYTES),
            true,
        );
        assert!(!skipped.root.exists(), "{}", report.summary());
    }

    // -------------------------------------------------------------- steps 2 and 3, end to end

    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=rkqa", "-c", "user.email=rkqa@example.com"])
            .args(["-c", "commit.gpgsign=false", "-c", "core.autocrlf=false"])
            .args(args)
            .output()
            .expect("git is required for this fixture");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    struct Repo {
        base: PathBuf,
        root: PathBuf,
        managed: PathBuf,
    }

    const FIXTURE_APP_SECTIONS: &str = r#"
[build]
command = ["rustright", "cargo", "build", "-p", "fxapp", "--features", "qa"]
features = ["qa"]
artifacts = [{ role = "app", cargo_package = "fxapp" }]

[tests]
untagged_baseline = 0
"#;

    fn repo(tag: &str) -> Repo {
        let base = tmp(tag);
        let root = base.join("app");
        std::fs::create_dir_all(root.join("tests")).unwrap();
        // The on-disk manifest also declares what a scenario journey requires (J-004): the app's one
        // QA `[build]` (lane M, consistent with `broker_output`'s artifacts) and the frozen
        // `[tests] untagged_baseline` (RK Dell). The bare `MANIFEST` stays without them for the
        // pure-function tests of `plan_build`.
        std::fs::write(
            root.join("journeys.toml"),
            format!("{MANIFEST}{FIXTURE_APP_SECTIONS}"),
        )
        .unwrap();
        std::fs::write(root.join("tests/j1.scenario.toml"), "name = \"j1\"\n").unwrap();
        std::fs::write(root.join("Cargo.lock"), "# lock\n").unwrap();
        git(&root, &["init", "-q"]);
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "fixture"]);
        let managed = base.join("managed");
        Repo {
            base,
            root,
            managed,
        }
    }

    fn opts(r: &Repo, ids: &[&str]) -> PrepareOptions {
        PrepareOptions {
            app_root: r.root.clone(),
            ids: ids.iter().map(|s| s.to_string()).collect(),
            platform: None,
            tier: None,
            managed_root: Some(r.managed.clone()),
            store_cap: StoreCap(DEFAULT_RUN_STORE_CAP_BYTES),
        }
    }

    /// What the broker would print for a successful `cargo test --no-run` of the fixture app, with
    /// the executables really on disk outside the repository.
    fn broker_output(base: &Path) -> RawBuild {
        let target = base.join("broker-target/debug");
        std::fs::create_dir_all(target.join("deps")).unwrap();
        let adapter = target.join("deps/journeys_adapter-1234.exe");
        let app = target.join("fxapp.exe");
        std::fs::write(&adapter, vec![1u8; 3000]).unwrap();
        let mut bytes = vec![2u8; 3000];
        bytes.extend_from_slice(QA_CONTROL_MARKER.as_bytes());
        std::fs::write(&app, bytes).unwrap();
        let line = |pkg: &str, name: &str, kind: &str, test: bool, exe: &Path| {
            serde_json::json!({
                "reason": "compiler-artifact", "package_id": pkg,
                "target": {"name": name, "kind": [kind]}, "profile": {"test": test},
                "features": ["qa"], "executable": exe.to_string_lossy(), "fresh": false
            })
            .to_string()
        };
        RawBuild {
            broker: None,
            code: Some(0),
            stdout: [
                line(
                    "path+file:///x/fx#fx@0.1.0",
                    "journeys_adapter",
                    "test",
                    true,
                    &adapter,
                ),
                line(
                    "path+file:///x/crates/fxapp#0.1.0",
                    "fxapp",
                    "bin",
                    false,
                    &app,
                ),
                r#"{"reason":"build-finished","success":true}"#.to_string(),
            ]
            .join("\n"),
            stderr: format!("rustright: request {REQ}\n[rustright] target warm at start\n"),
            timed_out: false,
        }
    }

    fn inputs_now(r: &Repo, p: &Prepared) -> CurrentInputs {
        CurrentInputs::gather(
            &canonical_plain(&r.root).unwrap(),
            &p.plan,
            &p.manifest_sha256,
        )
        .unwrap()
    }

    #[test]
    fn prepare_selects_records_the_source_builds_and_writes_the_record() {
        let r = repo("prepare");
        let p = prepare_with(&opts(&r, &["J-001"]), &|_| Ok(broker_output(&r.base))).unwrap();
        // Step 1: the selection and the required ids are on disk before anything else matters.
        let sel = Selection::from_path(&p.selection_path).unwrap();
        assert_eq!(sel.ids(), ["J-001"]);
        assert_eq!(sel, p.selection);
        let required: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&p.required_path).unwrap()).unwrap();
        assert_eq!(required["schema"], REQUIRED_SCHEMA);
        assert_eq!(required["platforms"]["linux"][0], "J-001");
        // Steps 2 and 3: identity, attested broker build, copies in the run store.
        let rec = BuildRecord::read(&p.record_path).unwrap();
        assert_eq!(rec, p.record);
        assert_eq!(rec.broker.request_id, REQ);
        assert!(rec.broker.attested && rec.broker.exit_code == 0);
        assert!(!rec.source.dirty && rec.source == rec.source_after);
        assert_eq!(rec.source.tree.len(), 40);
        assert_eq!(rec.lockfiles.len(), 1);
        assert_eq!(rec.lockfiles[0].path, "Cargo.lock");
        assert_eq!(rec.features.requested, ["qa"]);
        assert_eq!(rec.manifest.sha256, p.manifest_sha256);
        assert!(p
            .run
            .root
            .join(&rec.artifact("app").unwrap().store_path)
            .is_file());
        // The run is open for the launch step, and the launch gate accepts it as built.
        let cur = inputs_now(&r, &p);
        let ok = rec.verify_launch(&p.run.root, "app", &cur).unwrap();
        assert_eq!(ok.sha256, rec.artifact("app").unwrap().sha256);
        assert!(rec.verify_launch(&p.run.root, "adapter", &cur).is_ok());
        assert!(p.describe().contains(REQ));
    }

    #[test]
    fn after_prepare_a_tampered_binary_an_edited_tree_and_a_changed_feature_are_all_refused() {
        let r = repo("afterprep");
        let p = prepare_with(&opts(&r, &["J-001"]), &|_| Ok(broker_output(&r.base))).unwrap();
        let rec = &p.record;
        // A feature set the build did not use: the record is stale.
        let plan2 = BuildPlan::new(
            &p.plan.name,
            &argv("rustright cargo test -p fx --no-run --features qa,extra --message-format=json"),
            p.plan.cwd.clone(),
            p.plan.artifacts.clone(),
        )
        .unwrap();
        let cur2 = CurrentInputs::gather(
            &canonical_plain(&r.root).unwrap(),
            &plan2,
            &p.manifest_sha256,
        )
        .unwrap();
        let e = rec.verify_launch(&p.run.root, "app", &cur2).unwrap_err();
        assert!(
            e.problems()
                .iter()
                .any(|x| x.detail.contains("feature set changed")),
            "{e}"
        );
        // A tracked file edited after the build: the tree is not the one that was built.
        std::fs::write(r.root.join("tests/j1.scenario.toml"), "name = \"edited\"\n").unwrap();
        let cur = inputs_now(&r, &p);
        assert!(cur.source.dirty);
        let e = rec.verify_launch(&p.run.root, "app", &cur).unwrap_err();
        assert!(matches!(e, RecordRefusal::Stale(_)));
        assert!(
            e.problems().iter().any(|x| x.code == code::SOURCE_MISMATCH),
            "{e}"
        );
        // Restoring the file does not excuse a swapped binary.
        git(&r.root, &["checkout", "--", "tests/j1.scenario.toml"]);
        let cur = inputs_now(&r, &p);
        assert!(rec.verify_launch(&p.run.root, "app", &cur).is_ok());
        let stored = p.run.root.join(&rec.artifact("app").unwrap().store_path);
        let mut bytes = std::fs::read(&stored).unwrap();
        bytes[0] ^= 0xff;
        std::fs::write(&stored, bytes).unwrap();
        match rec.verify_launch(&p.run.root, "app", &cur).unwrap_err() {
            RecordRefusal::Binary { refusal, .. } => {
                assert!(
                    matches!(refusal, LaunchRefusal::HashMismatch { .. }),
                    "{refusal}"
                )
            }
            other => panic!("{other}"),
        }
    }

    #[test]
    fn an_edit_during_the_build_refuses_the_run_with_source_mismatch() {
        let r = repo("duringbuild");
        let e = prepare_with(&opts(&r, &["J-001"]), &|_| {
            std::fs::write(
                r.root.join("tests/j1.scenario.toml"),
                "name = \"edited mid-build\"\n",
            )
            .unwrap();
            Ok(broker_output(&r.base))
        })
        .err()
        .expect("a source change during the build must refuse the run");
        assert!(e.0.starts_with("source_mismatch"), "{}", e.0);
        // The failure is kept as evidence: a failed run with the reason beside it, and no record.
        let app_dir = r.managed.join("runs").join("fixtureapp");
        let run_dirs: Vec<PathBuf> = std::fs::read_dir(&app_dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.join(storage::RUN_MARKER).is_file())
            .collect();
        assert_eq!(run_dirs.len(), 1);
        let dir = &run_dirs[0];
        assert!(std::fs::read_to_string(dir.join(PREPARE_ERROR_FILE))
            .unwrap()
            .contains("source_mismatch"));
        assert!(!dir.join(BUILD_RECORD_FILE).exists());
        assert!(!dir.join(STORE_DIR).exists());
        let marker = storage::read_marker(&dir.join(storage::RUN_MARKER)).unwrap();
        assert_eq!(marker.outcome, Some(RunOutcome::Failed));
    }

    #[test]
    fn nothing_is_created_when_selection_or_planning_refuses() {
        let r = repo("noside");
        assert!(prepare_with(&opts(&r, &["J-999"]), &|_| panic!("no build")).is_err());
        // A scenario journey now has a `[build]`, so it plans; mixing it with an adapter journey is
        // the planning refusal ("one run builds once").
        assert!(prepare_with(&opts(&r, &["J-001", "J-004"]), &|_| panic!("no build")).is_err());
        assert!(
            !r.managed.join("runs").exists(),
            "a refused selection creates no run"
        );
    }

    #[test]
    fn an_unattested_broker_build_leaves_a_failed_run_and_no_record() {
        let r = repo("unattested");
        let e = prepare_with(&opts(&r, &["J-001"]), &|_| {
            let mut raw = broker_output(&r.base);
            raw.code = Some(1);
            Ok(raw)
        })
        .err()
        .unwrap();
        assert!(e.0.starts_with("build_not_attested"), "{}", e.0);
        let app_dir = r.managed.join("runs").join("fixtureapp");
        assert!(std::fs::read_dir(&app_dir)
            .unwrap()
            .flatten()
            .all(|e| !e.path().join(BUILD_RECORD_FILE).exists()));
    }

    #[test]
    #[ignore = "needs the real RustRight broker and an app root: set RKQA_REAL_BROKER_APP_ROOT (and RKQA_REAL_BROKER_TIER); RK Dell runs it in the post-go batch"]
    fn real_broker_prepare_only() {
        let Ok(root) = std::env::var("RKQA_REAL_BROKER_APP_ROOT") else {
            return;
        };
        let p = prepare(&PrepareOptions {
            app_root: PathBuf::from(root),
            ids: vec![],
            platform: None,
            tier: std::env::var("RKQA_REAL_BROKER_TIER").ok(),
            managed_root: None,
            store_cap: StoreCap(DEFAULT_RUN_STORE_CAP_BYTES),
        })
        .unwrap();
        assert!(p.record.broker.attested);
        let root = canonical_plain(Path::new(
            &std::env::var("RKQA_REAL_BROKER_APP_ROOT").unwrap(),
        ))
        .unwrap();
        let cur = CurrentInputs::gather(&root, &p.plan, &p.manifest_sha256).unwrap();
        for a in &p.record.artifacts {
            p.record.verify_launch(&p.run.root, &a.role, &cur).unwrap();
        }
        finish_run(
            &p.run,
            RunOutcome::Skipped,
            StoreCap(DEFAULT_RUN_STORE_CAP_BYTES),
            true,
        );
    }

    // ------------------------------------------------------------ steps 4 and 5: launch, collect, receipt

    use crate::worker::{StepSample, StepTiming};
    use std::sync::atomic::{AtomicUsize, Ordering};

    const MANIFEST_S: &str = r#"
version = 1
app = "fixtureapp"

[build]
command = ["rustright", "cargo", "build", "-p", "fxapp", "--features", "qa"]
features = ["qa"]
artifacts = [
  { role = "engine", cargo_target = "fxengine", qa_marker = false },
  { role = "app", cargo_package = "fxapp" },
  { role = "plugin", cargo_target = "fxplugin", qa_marker = false },
]

[tests]
untagged_baseline = 0

[[journey]]
id = "S-001"
title = "scenario journey"
platforms = ["macos", "windows", "linux"]
surface = "engine"
runner = "scenario"
entry = "tests/scenarios/*.scenario.toml"
tier = "fast"
data = "generated"
"#;

    const MANIFEST_B: &str = r#"
version = 1
app = "fixtureapp"

[[adapter]]
name = "fx"
build = ["rustright", "cargo", "test", "-p", "fx", "--no-run", "--features", "qa", "--message-format=json"]
executable = { role = "adapter", cargo_target = "journeys_adapter" }
artifacts = [{ role = "app", cargo_package = "fxapp" }]

[[bridge]]
app = "fxapp"
binary = "journeys_adapter"
removal_date = "2099-12-31"

[[journey]]
id = "B-001"
title = "bridged"
platforms = ["macos", "windows", "linux"]
surface = "engine"
runner = "adapter"
adapter = "fx"
entry = "tests/j1.scenario.toml"
tier = "fast"
data = "generated"
"#;

    /// A committed app root: `journeys.toml`, a one-worker `rightkit-qa.toml` and `scenarios`.
    fn repo_custom(tag: &str, manifest: &str, scenarios: &[&str]) -> Repo {
        let base = tmp(tag);
        let root = base.join("app");
        std::fs::create_dir_all(root.join("tests/scenarios")).unwrap();
        std::fs::write(root.join("journeys.toml"), manifest).unwrap();
        std::fs::write(
            root.join("rightkit-qa.toml"),
            "app = \"fixtureapp\"\n[host]\nworkers = 1\n",
        )
        .unwrap();
        std::fs::write(root.join("tests/j1.scenario.toml"), "name = \"j1\"\n").unwrap();
        for s in scenarios {
            std::fs::write(
                root.join(format!("tests/scenarios/{s}.scenario.toml")),
                format!("name = \"{s}\"\n[[step]]\nkind = \"cli\"\n"),
            )
            .unwrap();
        }
        std::fs::write(root.join("Cargo.lock"), "# lock\n").unwrap();
        git(&root, &["init", "-q"]);
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "fixture"]);
        let managed = base.join("managed");
        Repo {
            base,
            root,
            managed,
        }
    }

    /// The broker's output for the scenario manifest: an engine without the marker, an app with it.
    fn broker_output_s(base: &Path) -> RawBuild {
        let target = base.join("broker-target/debug");
        std::fs::create_dir_all(&target).unwrap();
        let engine = target.join("fxengine.exe");
        let app = target.join("fxapp.exe");
        let plugin = target.join("fxplugin.exe");
        std::fs::write(&engine, vec![1u8; 3000]).unwrap();
        std::fs::write(&plugin, vec![3u8; 3000]).unwrap();
        let mut bytes = vec![2u8; 3000];
        bytes.extend_from_slice(QA_CONTROL_MARKER.as_bytes());
        std::fs::write(&app, bytes).unwrap();
        let line = |pkg: &str, name: &str, exe: &Path| {
            serde_json::json!({
                "reason": "compiler-artifact", "package_id": pkg,
                "target": {"name": name, "kind": ["bin"]}, "profile": {"test": false},
                "features": ["qa"], "executable": exe.to_string_lossy(), "fresh": false
            })
            .to_string()
        };
        RawBuild {
            broker: None,
            code: Some(0),
            stdout: [
                line("path+file:///x/crates/fxengine#0.1.0", "fxengine", &engine),
                line("path+file:///x/crates/fxapp#0.1.0", "fxapp", &app),
                line("path+file:///x/crates/fxplugin#0.1.0", "fxplugin", &plugin),
                r#"{"reason":"build-finished","success":true}"#.to_string(),
            ]
            .join("\n"),
            stderr: format!("rustright: request {REQ}\n"),
            timed_out: false,
        }
    }

    type WorkerFn<'a> = &'a (dyn Fn(&ScenarioSpec) -> Result<WorkerOutcome> + Sync);

    fn hooks_run(
        r: &Repo,
        ids: &[&str],
        broker: &dyn Fn(&BuildPlan) -> Result<RawBuild>,
        worker: WorkerFn<'_>,
        exec: &(dyn Fn(&Tracker, &bridge::Invocation) -> Result<bridge::RawRun> + Sync),
        session: &dyn Fn() -> Option<String>,
    ) -> Result<RunReport> {
        run_with(
            &JourneyRunOptions {
                prepare: opts(r, ids),
                exe: PathBuf::from("unused"),
                mode: RunMode::Qualifying,
            },
            &Hooks {
                broker,
                worker,
                bridge_exec: exec,
                session,
            },
        )
    }

    fn go(r: &Repo, ids: &[&str], worker: WorkerFn<'_>) -> Result<RunReport> {
        hooks_run(
            r,
            ids,
            &|_| Ok(broker_output_s(&r.base)),
            worker,
            &|_, _| err("no bridge in this fixture"),
            &|| None,
        )
    }

    /// What a healthy worker reports for `spec`: it launched the recorded binaries from the paths the
    /// runner put in the spec, told the app a data directory inside its world, and saw the control
    /// token refuse a token-less order.
    fn passing(spec: &ScenarioSpec) -> Result<WorkerOutcome> {
        spec.validate()?;
        let bin = |role: &str| PathBuf::from(spec.scenario["binaries"][role].as_str().unwrap());
        let data_dir = spec
            .world
            .data_dir
            .join(format!("{}-1a2b3c4d", spec.scenario_id));
        std::fs::create_dir_all(&data_dir)?;
        let text = |p: &Path| p.to_string_lossy().into_owned();
        Ok(WorkerOutcome {
            schema_version: worker::SCHEMA_VERSION,
            run_id: spec.run_id.clone(),
            scenario_id: spec.scenario_id.clone(),
            status: CheckStatus::Passed,
            message: String::new(),
            engine_starts: 1,
            engine_starts_by_env: BTreeMap::from([("#1 default".to_string(), 1)]),
            survivors: vec![],
            timing: BTreeMap::from([(
                "cli".to_string(),
                StepTiming {
                    count: 2,
                    total_ms: 30,
                    max_ms: 20,
                },
            )]),
            steps: vec![
                StepSample {
                    label: "status".into(),
                    kind: "cli".into(),
                    ms: 10,
                    status: CheckStatus::Passed,
                },
                StepSample {
                    label: "login".into(),
                    kind: "cli".into(),
                    ms: 20,
                    status: CheckStatus::Passed,
                },
            ],
            launch: Some(WorkerLaunch {
                data_dir: data_dir.clone(),
                data_dir_env: Some("FX_DATA".into()),
                data_dir_arg: None,
                args: vec![],
                env: vec![
                    ("FX_DATA".into(), text(&data_dir)),
                    (
                        spec.world.env_marker.name.clone(),
                        spec.world.env_marker.value.clone(),
                    ),
                    (
                        control::token::ENV_AUTH_FILE.into(),
                        text(&spec.world.control_token_file),
                    ),
                ],
                binaries: vec![bin("engine"), bin("app")],
                token_less_refused: Some(true),
            }),
            checks: vec![],
            artifacts: vec![],
        })
    }

    /// The `[plugin]` declaration reaches the worker's `scenario.json` as the recorded binary of its
    /// role, with the declared args and hello timeout; `${build.<role>}` carries every record role.
    #[test]
    fn a_declared_plugin_reaches_the_worker_as_a_build_record_binary() {
        let manifest = format!(
            "{MANIFEST_S}\n[plugin]\nrole = \"plugin\"\nargs = [\"--qa-plugin\"]\nhello_timeout_ms = 5000\n"
        );
        let r = repo_custom("plugin-decl", &manifest, &["alpha"]);
        let seen = std::sync::Mutex::new(None);
        go(&r, &[], &|spec| {
            *seen.lock().unwrap() = Some((
                spec.scenario["runtime"]["plugin"].clone(),
                spec.scenario["binaries"].clone(),
            ));
            passing(spec)
        })
        .unwrap();
        let (plugin, binaries) = seen.lock().unwrap().take().unwrap();
        assert_eq!(plugin["binary"], binaries["plugin"]);
        assert_eq!(plugin["args"], json!(["--qa-plugin"]));
        assert_eq!(plugin["helloTimeoutMs"], 5000);
        // Every build-record role is a binary the scenario can name (`${build.<role>}`).
        assert!(binaries["app"].is_string() && binaries["engine"].is_string());
        assert!(binaries["plugin"].is_string());
    }

    /// N3: the same manifest with the plugin role set to the engine is refused by `journeys check`
    /// (the manifest issue names the role), so no worker is ever handed the engine as a plugin.
    #[test]
    fn a_plugin_role_that_is_the_engine_is_a_manifest_issue() {
        let text = format!("{MANIFEST_S}\n[plugin]\nrole = \"engine\"\n");
        let (m, _) = manifest::parse_manifest(text.as_bytes()).unwrap();
        let issues = manifest::check_manifest(&m, Path::new("/nonexistent-app-root"));
        assert!(
            issues
                .iter()
                .any(|i| i.contains("[plugin] role \"engine\"")),
            "{issues:?}"
        );
    }

    fn validate_on_disk(report: &RunReport, manifest_text: &str) -> receipt::Validation {
        let rec =
            receipt::parse_receipt(&std::fs::read(report.receipt_path.as_ref().unwrap()).unwrap())
                .unwrap();
        let (m, sha) = manifest::parse_manifest(manifest_text.as_bytes()).unwrap();
        let results = std::fs::read(report.results_path.as_ref().unwrap()).unwrap();
        validate_receipt(
            &rec,
            &ValidateOptions {
                expected_tree: Some(rec.source.tree.as_str()),
                manifest: Some((&m, sha.as_str())),
                results: Some(results.as_slice()),
                evidence_root: Some(report.run_dir.as_path()),
                allow_dirty: false,
            },
        )
    }

    fn read_receipt(report: &RunReport) -> Receipt {
        receipt::parse_receipt(&std::fs::read(report.receipt_path.as_ref().unwrap()).unwrap())
            .unwrap()
    }

    #[test]
    fn a_passing_run_writes_a_receipt_that_validates_as_passed() {
        let r = repo_custom("e2e-pass", MANIFEST_S, &["alpha", "beta"]);
        let report = go(&r, &["S-001"], &passing).unwrap();
        assert!(report.passed, "{}", report.describe());
        assert_eq!(report.exit_code(), 0);
        let rec = read_receipt(&report);
        assert_eq!(rec.runner.as_deref(), Some("rightkit-qa"));
        assert_eq!(rec.verdict, Verdict::Passed);
        assert_eq!(rec.selection.ids, ["S-001"]);
        // One journey of two scenarios: both counted, every step recorded, both engines counted.
        let j = &rec.journeys[0];
        assert_eq!((j.units.total, j.units.passed), (2, 2));
        assert_eq!(j.steps.as_ref().unwrap().len(), 4);
        assert_eq!(j.engine_starts.as_ref().unwrap().total, 2);
        assert_eq!(j.survivors, Some(vec![]));
        assert_eq!(j.tokens_held, Some(vec![]));
        let w = j.world.as_ref().unwrap();
        assert!(w
            .control_auth
            .as_ref()
            .is_some_and(|a| a.owner_only && a.token_less_refused));
        assert_eq!(w.data_dir_env.as_deref(), Some("FX_DATA"));
        assert_eq!(rec.launched.len(), 2);
        assert_eq!(rec.build_record.as_ref().unwrap().path, BUILD_RECORD_FILE);
        assert!(rec.timing.as_ref().unwrap().per_kind.contains_key("cli"));
        // The receipt on disk validates, validated by this test and not by the runner.
        let v = validate_on_disk(&report, MANIFEST_S);
        assert!(v.passed(), "{:?}", v.problems);
        // The run is recorded as passed and its binary copies are gone.
        let marker = storage::read_marker(&report.run_dir.join(storage::RUN_MARKER)).unwrap();
        assert_eq!(marker.outcome, Some(RunOutcome::Passed));
        assert!(!report.run_dir.join(STORE_DIR).exists());
    }

    #[test]
    fn a_survivor_in_an_outcome_makes_the_receipt_not_passed() {
        let r = repo_custom("e2e-survivor", MANIFEST_S, &["alpha"]);
        let report = go(&r, &["S-001"], &|spec| {
            let mut o = passing(spec)?;
            o.survivors.push(Survivor {
                pid: 4242,
                name: "fx-engine".into(),
                owner: SurvivorOwner::WorldMarker,
                detail: String::new(),
            });
            Ok(o)
        })
        .unwrap();
        assert!(
            !report.passed && report.exit_code() == 1,
            "{}",
            report.describe()
        );
        let rec = read_receipt(&report);
        assert_ne!(rec.verdict, Verdict::Passed);
        assert_eq!(rec.journeys[0].status, JourneyStatus::Errored);
        assert_eq!(rec.journeys[0].survivors.as_ref().unwrap().len(), 1);
        assert!(rec.journeys[0]
            .reason
            .as_deref()
            .unwrap()
            .contains("survived"));
        // Whatever the runner wrote, a validator run by the test agrees.
        assert!(!validate_on_disk(&report, MANIFEST_S).passed());
        let marker = storage::read_marker(&report.run_dir.join(storage::RUN_MARKER)).unwrap();
        assert_eq!(marker.outcome, Some(RunOutcome::Failed));
    }

    #[test]
    fn a_failed_scenario_is_failed_and_a_pass_needs_launch_facts() {
        let r = repo_custom("e2e-failed", MANIFEST_S, &["alpha"]);
        let report = go(&r, &["S-001"], &|spec| {
            let mut o = passing(spec)?;
            o.status = CheckStatus::Failed;
            o.message = "expected 200".into();
            Ok(o)
        })
        .unwrap();
        assert!(!report.passed);
        assert_eq!(
            read_receipt(&report).journeys[0].status,
            JourneyStatus::Failed
        );
        // A pass that reports no launch is not a pass: isolation cannot be recorded.
        let r = repo_custom("e2e-nolaunch", MANIFEST_S, &["alpha"]);
        let report = go(&r, &["S-001"], &|spec| {
            let mut o = passing(spec)?;
            o.launch = None;
            Ok(o)
        })
        .unwrap();
        assert!(!report.passed);
        let rec = read_receipt(&report);
        assert_eq!(rec.journeys[0].status, JourneyStatus::Errored);
        assert!(rec.journeys[0]
            .reason
            .as_deref()
            .unwrap()
            .contains("no launch"));
    }

    #[test]
    fn a_launch_outside_its_world_or_of_another_binary_is_not_a_pass() {
        // The app was told a data directory outside its world.
        let r = repo_custom("e2e-escape", MANIFEST_S, &["alpha"]);
        let report = go(&r, &["S-001"], &|spec| {
            let mut o = passing(spec)?;
            let l = o.launch.as_mut().unwrap();
            let elsewhere = std::env::temp_dir().join("rkqa-elsewhere");
            l.env[0].1 = elsewhere.to_string_lossy().into_owned();
            l.data_dir = elsewhere;
            Ok(o)
        })
        .unwrap();
        assert!(!report.passed);
        assert!(read_receipt(&report).journeys[0]
            .reason
            .as_deref()
            .unwrap()
            .contains("not under its world"));
        // A dev build the record does not hold (dev-target substitution).
        let r = repo_custom("e2e-substitute", MANIFEST_S, &["alpha"]);
        let report = go(&r, &["S-001"], &|spec| {
            let mut o = passing(spec)?;
            o.launch
                .as_mut()
                .unwrap()
                .binaries
                .push(PathBuf::from("target/debug/fxapp.exe"));
            Ok(o)
        })
        .unwrap();
        assert!(!report.passed);
        assert!(read_receipt(&report).journeys[0]
            .reason
            .as_deref()
            .unwrap()
            .contains("not a binary of the build record"));
        // A token-less order that was not refused fails the control auth rule.
        let r = repo_custom("e2e-noauth", MANIFEST_S, &["alpha"]);
        let report = go(&r, &["S-001"], &|spec| {
            let mut o = passing(spec)?;
            o.launch.as_mut().unwrap().token_less_refused = Some(false);
            Ok(o)
        })
        .unwrap();
        assert!(!report.passed);
        assert!(
            report.problems.iter().any(|p| p.code == "control_auth"),
            "{:?}",
            report.problems
        );
    }

    #[test]
    fn a_binary_tampered_after_the_build_is_refused_before_launch_and_after_the_run() {
        // The first worker swaps the app's stored copy: the second scenario is refused before it
        // starts, and the first one is refused after the run because its binary changed under it.
        let r = repo_custom("e2e-tamper", MANIFEST_S, &["alpha", "beta"]);
        let calls = AtomicUsize::new(0);
        let launched = AtomicUsize::new(0);
        let report = go(&r, &["S-001"], &|spec| {
            launched.fetch_add(1, Ordering::SeqCst);
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                let p = PathBuf::from(spec.scenario["binaries"]["app"].as_str().unwrap());
                let mut b = std::fs::read(&p).unwrap();
                b[0] ^= 0xff;
                std::fs::write(&p, b).unwrap();
            }
            passing(spec)
        })
        .unwrap();
        assert!(!report.passed);
        assert_eq!(
            launched.load(Ordering::SeqCst),
            1,
            "the tampered binary was launched again"
        );
        let rec = read_receipt(&report);
        assert_eq!(rec.journeys[0].status, JourneyStatus::Errored);
        assert_eq!(rec.journeys[0].units.passed, 0);
        let why = rec.journeys[0].reason.as_deref().unwrap();
        assert!(
            why.contains("changed during the run") || why.contains("refused before launch"),
            "{why}"
        );
        // A binary swapped while the only scenario ran: caught by the re-hash after the run.
        let r = repo_custom("e2e-tamper-during", MANIFEST_S, &["alpha"]);
        let report = go(&r, &["S-001"], &|spec| {
            let p = PathBuf::from(spec.scenario["binaries"]["app"].as_str().unwrap());
            let mut b = std::fs::read(&p).unwrap();
            b[0] ^= 0xff;
            std::fs::write(&p, b).unwrap();
            passing(spec)
        })
        .unwrap();
        assert!(!report.passed);
        assert!(read_receipt(&report).journeys[0]
            .reason
            .as_deref()
            .unwrap()
            .contains("changed during the run"));
    }

    #[test]
    fn a_source_edit_during_the_run_fails_every_journey() {
        let r = repo_custom("e2e-edit", MANIFEST_S, &["alpha"]);
        let report = go(&r, &["S-001"], &|spec| {
            std::fs::write(
                r.root.join("tests/scenarios/alpha.scenario.toml"),
                "name = \"alpha\"\n[[step]]\nkind = \"cli\"\n# edited\n",
            )
            .unwrap();
            passing(spec)
        })
        .unwrap();
        assert!(!report.passed);
        assert!(read_receipt(&report).journeys[0]
            .reason
            .as_deref()
            .unwrap()
            .contains("source changed during the run"));
    }

    #[test]
    fn explore_records_a_scenario_and_never_a_receipt() {
        let r = repo_custom("e2e-explore", MANIFEST_S, &["alpha"]);
        let dir = r.base.join("explored");
        let report = run_with(
            &JourneyRunOptions {
                prepare: opts(&r, &[]),
                exe: PathBuf::from("unused"),
                mode: RunMode::Explore(ExploreRecording {
                    dir: dir.clone(),
                    name: "recorded".into(),
                    target: "engine".into(),
                    tokens: vec![],
                    steps: vec![receipt::ExploreStep {
                        kind: "cli".into(),
                        label: Some("status".into()),
                        params: BTreeMap::new(),
                    }],
                }),
            },
            &Hooks {
                broker: &|_| panic!("an explore run builds nothing"),
                worker: &|_| panic!("an explore run launches nothing"),
                bridge_exec: &|_, _| panic!("an explore run launches nothing"),
                session: &|| None,
            },
        )
        .unwrap();
        assert!(
            report.receipt_path.is_none()
                && report.results_path.is_none()
                && report.exit_code() == 0
        );
        assert!(dir.join("recorded.scenario.toml").is_file());
        assert!(
            !r.managed.join("runs").exists(),
            "an explore run creates no run"
        );
        fn find_receipt(d: &Path) -> bool {
            std::fs::read_dir(d)
                .map(|rd| {
                    rd.flatten().any(|e| {
                        let p = e.path();
                        p.file_name().is_some_and(|n| n == RECEIPT_FILE)
                            || (p.is_dir() && find_receipt(&p))
                    })
                })
                .unwrap_or(false)
        }
        assert!(!find_receipt(&r.base), "an explore run wrote a receipt");
    }

    #[test]
    fn a_session_outside_the_desktop_is_refused_before_anything_is_built() {
        let r = repo_custom("e2e-session0", MANIFEST_S, &["alpha"]);
        let e = hooks_run(
            &r,
            &["S-001"],
            &|_| panic!("nothing is built from session 0"),
            &|_| panic!("nothing is launched from session 0"),
            &|_, _| err("no bridge"),
            &|| Some("session 0".into()),
        )
        .err()
        .unwrap();
        assert!(
            e.0.contains("rightkit-qa-interactive") && e.0.contains("session 0"),
            "{}",
            e.0
        );
        assert!(!r.managed.join("runs").exists());
    }

    #[test]
    fn an_entry_that_matches_no_scenario_is_refused_not_passed() {
        let r = repo_custom("e2e-noscen", MANIFEST_S, &[]);
        let e = go(&r, &["S-001"], &passing);
        // The run refuses it in `prepare_with`, before any run directory or build exists.
        assert!(e.is_err(), "{:?}", e.map(|x| x.passed));
    }

    #[test]
    fn a_bridged_journey_runs_the_recorded_libtest_binary_and_validates() {
        let r = repo_custom("e2e-bridge", MANIFEST_B, &[]);
        let libtest = |_: &Tracker, inv: &bridge::Invocation| -> Result<bridge::RawRun> {
            let a = &inv.args;
            let stdout = if a.iter().any(|x| x == "--list") {
                if a.iter().any(|x| x == "--ignored") {
                    String::new()
                } else {
                    "alpha::passes: test\nbeta::ok: test\n".to_string()
                }
            } else {
                format!("running 1 test\ntest {} ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 0.00s\n", a[0])
            };
            Ok(bridge::RawRun {
                code: Some(0),
                stdout,
                stderr: String::new(),
                timed_out: false,
                started_at: now_iso(),
                duration_ms: 5,
            })
        };
        let report = hooks_run(
            &r,
            &["B-001"],
            &|_| Ok(broker_output(&r.base)),
            &|_| panic!("a bridged journey starts no scenario worker"),
            &libtest,
            &|| None,
        )
        .unwrap();
        assert!(report.passed, "{}", report.describe());
        let rec = read_receipt(&report);
        assert_eq!(rec.adapter.name, "fx");
        assert_eq!(
            (rec.journeys[0].units.total, rec.journeys[0].units.passed),
            (2, 2)
        );
        assert_eq!(rec.journeys[0].survivors, Some(vec![]));
        assert!(rec.journeys[0].steps.is_none());
        assert!(validate_on_disk(&report, MANIFEST_B).passed());
        // A failing libtest result is a failed journey, not a pass.
        let r = repo_custom("e2e-bridge-fail", MANIFEST_B, &[]);
        let failing = |_: &Tracker, inv: &bridge::Invocation| -> Result<bridge::RawRun> {
            let a = &inv.args;
            let listing = a.iter().any(|x| x == "--list");
            let stdout = if listing {
                if a.iter().any(|x| x == "--ignored") {
                    String::new()
                } else {
                    "alpha::passes: test\n".to_string()
                }
            } else {
                "running 1 test\ntest alpha::passes ... FAILED\n\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n".to_string()
            };
            Ok(bridge::RawRun {
                code: Some(if listing { 0 } else { 101 }),
                stdout,
                stderr: String::new(),
                timed_out: false,
                started_at: now_iso(),
                duration_ms: 5,
            })
        };
        let report = hooks_run(
            &r,
            &["B-001"],
            &|_| Ok(broker_output(&r.base)),
            &|_| panic!("no worker"),
            &failing,
            &|| None,
        )
        .unwrap();
        assert!(!report.passed);
        assert_eq!(
            read_receipt(&report).journeys[0].status,
            JourneyStatus::Failed
        );
    }

    #[test]
    fn aggregation_never_softens_a_unit() {
        let u = |n: &str, s: JourneyStatus, why: &str| (n.to_string(), s, why.to_string());
        let agg = |units: &[(String, JourneyStatus, String)]| {
            aggregate("J", units, "2026-10-23T10:00:00Z".into(), 5, vec![], "none")
        };
        let all = agg(&[
            u("a", JourneyStatus::Passed, ""),
            u("b", JourneyStatus::Passed, ""),
        ]);
        assert_eq!(
            (all.status, all.units.total, all.units.passed, all.reason),
            (JourneyStatus::Passed, 2, 2, None)
        );
        let r = agg(&[
            u("a", JourneyStatus::Passed, ""),
            u("b", JourneyStatus::Skipped, "no ui"),
        ]);
        assert_eq!((r.status, r.units.skipped), (JourneyStatus::Skipped, 1));
        let r = agg(&[
            u("a", JourneyStatus::Failed, "x"),
            u("b", JourneyStatus::Skipped, "y"),
        ]);
        assert_eq!(r.status, JourneyStatus::Failed);
        let r = agg(&[
            u("a", JourneyStatus::Failed, "x"),
            u("b", JourneyStatus::Errored, "boom"),
        ]);
        assert_eq!((r.status, r.units.failed), (JourneyStatus::Errored, 2));
        assert!(r.reason.unwrap().starts_with("b: boom"));
        // not run is a skip, never a pass; no unit at all is errored
        assert_eq!(
            agg(&[u("a", JourneyStatus::Missing, "")]).status,
            JourneyStatus::Skipped
        );
        let none = agg(&[]);
        assert_eq!(
            (none.status, none.reason.as_deref()),
            (JourneyStatus::Errored, Some("none"))
        );
        assert!(none.units.consistent());
    }

    #[test]
    fn deadlines_entries_and_launch_facts_are_checked() {
        let d = deadlines_for(None, None);
        assert_eq!(
            (d.step_ms, d.scenario_ms),
            (DEFAULT_STEP_DEADLINE_MS, DEFAULT_SCENARIO_DEADLINE_MS)
        );
        let kb = KindBudget {
            step_ms: Some(2_000),
            journey_ms: Some(60_000),
            suite_ms: None,
        };
        let d = deadlines_for(None, Some(&kb));
        assert_eq!((d.step_ms, d.scenario_ms), (20_000, 600_000));
        assert_eq!(deadlines_for(Some(30), Some(&kb)).scenario_ms, 30_000);
        let tiny = KindBudget {
            step_ms: Some(1),
            journey_ms: Some(1),
            suite_ms: None,
        };
        let d = deadlines_for(None, Some(&tiny));
        d.validate().unwrap();
        assert!(d.step_ms <= d.scenario_ms);

        assert!(wild_match(b"*.scenario.toml", b"a.scenario.toml"));
        assert!(!wild_match(b"*.scenario.toml", b"a.toml"));
        assert!(wild_match(b"s?.toml", b"s1.toml"));
        let dir = tmp("entries");
        std::fs::write(dir.join("b.scenario.toml"), "").unwrap();
        std::fs::write(dir.join("a.scenario.toml"), "").unwrap();
        std::fs::write(dir.join("c.txt"), "").unwrap();
        let names = |v: Vec<PathBuf>| {
            v.iter()
                .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(scenario_files_for(&dir, "*.scenario.toml")),
            ["a.scenario.toml", "b.scenario.toml"]
        );
        assert_eq!(
            names(scenario_files_for(&dir, "a.scenario.toml")),
            ["a.scenario.toml"]
        );
        assert!(scenario_files_for(&dir, "none/*.scenario.toml").is_empty());
        assert!(scenario_files_for(&dir, "x*/y.scenario.toml").is_empty());
    }
}

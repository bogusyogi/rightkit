//! Managed QA run storage: shared marker, retention, and cross-language lock protocol.
use crate::util::{err, new_id, now_ms, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const RUN_MARKER: &str = ".rightkit-run.json";
pub const DEFAULT_KEEP_COUNT: usize = 5;
pub const DEFAULT_YOUNG_MS: u128 = 24 * 60 * 60 * 1000;
pub const DEFAULT_MAX_BYTES: u64 = 20_000_000_000;

/// What a run is for. A `functional` run checks clicks, state and text and takes no screenshots
/// and no screen recordings; a `visual` run is the audit-visual run that captures screens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunKind {
    Functional,
    Visual,
}

impl RunKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RunKind::Functional => "functional",
            RunKind::Visual => "visual",
        }
    }

    /// `functional` or `visual`; anything else is an error that names both.
    pub fn parse(text: &str) -> Result<Self> {
        match text.trim() {
            "functional" => Ok(RunKind::Functional),
            "visual" => Ok(RunKind::Visual),
            other => err(format!("unknown run kind '{other}' (functional|visual)")),
        }
    }
}

impl std::fmt::Display for RunKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How a finished run ended, as recorded in its marker. Retention reads only this.
/// `Skipped` is a run in which nothing executed (every scenario skipped): it holds no evidence,
/// so retention removes it and it never counts as the newest pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunOutcome {
    Passed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunMarker {
    pub schema_version: u8,
    pub app: String,
    pub run_id: String,
    pub created_at: u128,
    pub owner_pid: u32,
    pub lease_until: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<u128>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_root: Option<PathBuf>,
    /// Run kind. Absent in markers written before 0.5.0; retention treats those as `functional`
    /// but never prunes them, because they carry no recorded outcome either.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<RunKind>,
    /// Pass or fail, set when the job ends. `None` while running and in pre-0.5.0 markers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<RunOutcome>,
}

#[derive(Debug, Clone)]
pub struct ManagedRun {
    pub root: PathBuf,
    pub marker: RunMarker,
}

#[derive(Debug, Default, Clone)]
pub struct RetentionReport {
    pub removed: Vec<PathBuf>,
    pub retained: Vec<(PathBuf, String)>,
    pub total_bytes: u64,
    pub cap_bytes: u64,
    pub over_cap_bytes: u64,
    pub blocked_bytes: u64,
    pub unknown_bytes: u64,
}

pub fn managed_root(explicit: Option<&Path>) -> PathBuf {
    if let Some(path) = explicit.filter(|p| !p.as_os_str().is_empty()) {
        return path.to_path_buf();
    }
    if let Ok(path) = std::env::var("RIGHTKIT_MANAGED_ROOT") {
        if !path.trim().is_empty() {
            return PathBuf::from(path);
        }
    }
    // Workstation default only when its drive exists; hosted CI and other machines
    // fall back to the runner's temp dir, then the system temp dir.
    let (workstation, volume) = if cfg!(windows) {
        (
            PathBuf::from(r"D:\.rightkit-managed"),
            PathBuf::from(r"D:\"),
        )
    } else {
        (
            PathBuf::from("/Volumes/D/.rightkit-managed"),
            PathBuf::from("/Volumes/D"),
        )
    };
    if volume.is_dir() {
        return workstation;
    }
    let temp = std::env::var_os("RUNNER_TEMP")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(std::env::temp_dir);
    temp.join("rightkit-managed")
}

pub fn run_root(root: &Path, app: &str, run_id: &str) -> Result<PathBuf> {
    let app = safe_key(app, "app")?;
    let run_id = safe_key(run_id, "run id")?;
    let runs = root.join("runs").join(&app);
    let result = runs.join(&run_id);
    if !is_inside(&runs, &result) {
        return err("QA run path escapes managed root");
    }
    Ok(result)
}

/// Resolves a trusted root once, before [`ensure_dir`] walks it. The root is created if missing
/// and canonicalised, so an OS-level alias above it is resolved rather than refused: on macOS
/// `/var` and `/tmp` are symlinks into `/private`, and `std::env::temp_dir()` lives under `/var`.
/// Components below the returned path are still checked by [`ensure_dir`].
fn trusted_root(root: &Path) -> Result<PathBuf> {
    fs::create_dir_all(root)?;
    Ok(fs::canonicalize(root)?)
}

pub(crate) fn prepare_app_root(root: &Path, app: &str) -> Result<PathBuf> {
    let root = trusted_root(&managed_root(Some(root)))?;
    let app = safe_key(app, "app")?;
    ensure_dir(&root)?;
    ensure_dir(&root.join("runs"))?;
    let app_root = root.join("runs").join(app);
    ensure_dir(&app_root)?;
    Ok(app_root)
}

pub fn create(
    root: Option<&Path>,
    app: &str,
    run_id: Option<&str>,
    lease: Duration,
) -> Result<ManagedRun> {
    let root = trusted_root(&managed_root(root))?;
    let app = safe_key(app, "app")?;
    let id = match run_id {
        Some(id) => safe_key(id, "run id")?,
        None => new_id(),
    };
    let app_root = root.join("runs").join(&app);
    ensure_dir(&root)?;
    ensure_dir(&root.join("runs"))?;
    ensure_dir(&app_root)?;
    // The retention lock guards only the prune pass. A second starter does not wait for it and does
    // not fail: another process is already pruning this app, so it skips its own pass and starts.
    // Parallel scenarios in one binary, or in several processes, all proceed.
    if let Some(guard) = try_lock(&app_root, Duration::ZERO)? {
        let mut report = RetentionReport::default();
        prune_unlocked(
            &app_root,
            &app,
            DEFAULT_MAX_BYTES,
            DEFAULT_KEEP_COUNT,
            DEFAULT_YOUNG_MS,
            &[],
            &mut report,
        )?;
        drop(guard);
        if report.over_cap_bytes > 0 {
            return err(format!(
                "QA retention cap is blocked by {} bytes of protected or unknown data",
                report.over_cap_bytes
            ));
        }
    }
    let run = run_root(&root, &app, &id)?;
    // `create_dir` is the atomic claim on the run id, so no lock is needed to make it unique.
    match fs::create_dir(&run) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return err(format!("QA run already exists: {}", run.display()));
        }
        Err(e) => return Err(e.into()),
    }
    fs::create_dir_all(run.join("data"))?;
    fs::create_dir_all(run.join("evidence"))?;
    let now = now_ms();
    let marker = RunMarker {
        schema_version: 1,
        app: app.clone(),
        run_id: id.clone(),
        created_at: now,
        owner_pid: std::process::id(),
        lease_until: now.saturating_add(lease.as_millis().max(1)),
        completed_at: None,
        app_root: None,
        kind: Some(RunKind::Functional),
        outcome: None,
    };
    write_marker(&run, &marker)?;
    Ok(ManagedRun { root: run, marker })
}

/// Rewrite the marker of the run at `run_root` through `change`. Fails when the marker is missing
/// or malformed; the run directory is never created or guessed.
pub(crate) fn update_marker(run_root: &Path, change: impl FnOnce(&mut RunMarker)) -> Result<()> {
    let mut marker = read_marker(&run_root.join(RUN_MARKER))
        .ok_or_else(|| crate::util::Error("QA run marker is missing or malformed".into()))?;
    change(&mut marker);
    write_marker(run_root, &marker)
}

/// Record the kind of the run at `run_root` in its marker.
pub fn set_run_kind(run_root: &Path, kind: RunKind) -> Result<()> {
    update_marker(run_root, |m| m.kind = Some(kind))
}

/// Record how the run at `run_root` ended in its marker. [`finish`] keeps it.
pub fn set_run_outcome(run_root: &Path, outcome: RunOutcome) -> Result<()> {
    update_marker(run_root, |m| m.outcome = Some(outcome))
}

pub fn finish(run: &ManagedRun) -> Result<RunMarker> {
    let mut marker = read_marker(&run.root.join(RUN_MARKER))
        .ok_or_else(|| crate::util::Error("QA run marker is missing or malformed".into()))?;
    if let Some(home) = owned_app_root(&run.root, &marker)? {
        for name in ["evidence", "captures"] {
            let target = if name == "evidence" {
                run.root.join("evidence/app")
            } else {
                run.root.join(name)
            };
            copy_tree(&home.join(name), &target)?;
        }
        for entry in fs::read_dir(home)? {
            let entry = entry?;
            if entry.file_name().to_string_lossy().ends_with(".log") {
                copy_tree(
                    &entry.path(),
                    &run.root.join("evidence").join(entry.file_name()),
                )?;
            }
        }
    }
    marker.owner_pid = 0;
    marker.lease_until = 0;
    marker.completed_at = Some(now_ms());
    write_marker(&run.root, &marker)?;
    Ok(marker)
}

/// Use short internal temp paths on macOS: launchd needs internal storage &
/// service fallback sockets must fit sockaddr_un even after app temp overrides.
pub(crate) fn app_temp_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        PathBuf::from("/private/tmp")
    }
    #[cfg(not(target_os = "macos"))]
    std::env::temp_dir()
}

/// Environment variable that moves QA app homes: homes go under `<value>/rightkit-qa/`.
/// Must be an absolute path. Unset or empty keeps the system default (`app_temp_dir`).
///
/// The default is NOT the managed data root: on macOS a launchd-started app cannot open its
/// live log on an external volume (`open` fails with -10810, see `control::launch`) and the
/// app's control socket path must fit `sockaddr_un`, so homes stay on internal storage unless the
/// caller opts in with this variable.
pub const HOME_ROOT_ENV: &str = "RIGHTKIT_QA_HOME_ROOT";
/// Set to `0` to skip the automatic home sweep at workspace creation.
pub const HOME_SWEEP_ENV: &str = "RIGHTKIT_QA_HOME_SWEEP";
/// Minimum time between automatic home sweeps, across processes (stamp file in the homes dir).
const SWEEP_INTERVAL: Duration = Duration::from_secs(10 * 60);
/// Default bound on the bytes held by app homes that are not protected as live or young.
pub const DEFAULT_HOME_MAX_BYTES: u64 = 4_000_000_000;

fn home_base_from(override_value: Option<&std::ffi::OsStr>) -> PathBuf {
    match override_value {
        Some(v) if !v.is_empty() && Path::new(v).is_absolute() => PathBuf::from(v),
        _ => app_temp_dir(),
    }
}

/// The directory that holds `rightkit-qa/`: the override, else [`app_temp_dir`].
pub(crate) fn home_base() -> PathBuf {
    home_base_from(std::env::var_os(HOME_ROOT_ENV).as_deref())
}

pub(crate) fn create_app_root(run: &mut ManagedRun) -> Result<PathBuf> {
    let parent = trusted_root(&home_base())?.join("rightkit-qa");
    ensure_dir(&parent)?;
    let parent = fs::canonicalize(parent)?;
    create_app_root_in(run, &parent)
}

fn create_app_root_in(run: &mut ManagedRun, parent: &Path) -> Result<PathBuf> {
    let label = product_label(&run.marker.app);
    let home = parent.join(format!("{}-{}", label, &new_id().replace('-', "")[..16]));
    rightkit_fs::private::ensure_private_dir(&home)
        .map_err(|e| crate::util::Error(format!("cannot create private QA home: {e}")))?;
    run.marker.app_root = Some(home.clone());
    // Bind both sides; a forged path in a managed marker cannot delete arbitrary temp data.
    write_whole(
        &home.join(".rightkit-app.json"),
        &serde_json::to_vec(&serde_json::json!({
            "runRoot": fs::canonicalize(&run.root)?, "app": run.marker.app,
            "runId": run.marker.run_id,
        }))?,
    )?;
    write_home_marker(&home, &run.marker.app, &run.marker.run_id)?;
    write_marker(&run.root, &run.marker)?;
    Ok(home)
}

/// What a home sweep removes: homes whose run is over (or whose run record is gone) and that are
/// older than `young`, then the oldest inactive homes until the rest fit `max_bytes`.
#[derive(Debug, Clone, Copy)]
pub struct HomePolicy {
    pub young: Duration,
    pub max_bytes: u64,
}

impl Default for HomePolicy {
    fn default() -> Self {
        Self {
            young: Duration::from_millis(DEFAULT_YOUNG_MS as u64),
            max_bytes: DEFAULT_HOME_MAX_BYTES,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct HomeSweepReport {
    /// (home, bytes, reason). With `dry_run` these are what would be removed.
    pub removed: Vec<(PathBuf, u64, String)>,
    pub retained: Vec<(PathBuf, String)>,
    /// Bytes of every home found, before removal.
    pub total_bytes: u64,
    pub removed_bytes: u64,
}

struct HomeEntry {
    dir: PathBuf,
    bytes: u64,
    /// Milliseconds since the epoch the home was last known to be in use or created.
    stamp: u128,
    kind: HomeKind,
}

enum HomeKind {
    /// Run record present and live (owner alive or lease unexpired).
    Active,
    /// Run record present, run over.
    Finished,
    /// Run record missing or not matching: nothing else will ever reclaim this home.
    Orphan,
}

fn modified_ms(path: &Path) -> Option<u128> {
    fs::symlink_metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis())
}

/// Bytes under `dir`, skipping symlinks and unreadable entries (a size estimate, not a proof).
fn tree_bytes(dir: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                stack.push(e.path());
            } else if ft.is_file() {
                total = total.saturating_add(e.metadata().map(|m| m.len()).unwrap_or(0));
            }
        }
    }
    total
}

/// Sweep app homes under `parent` (the `.../rightkit-qa` directory). Marker-driven pruning only
/// reaches homes whose run record still exists; this pass also reclaims homes left by killed
/// processes and tests whose run root was deleted. A home is never removed while its run is
/// live, while it is younger than the policy window and unowned, or when it has no binding file
/// (it may not be ours). `dry_run` reports without deleting.
pub fn sweep_homes(parent: &Path, policy: HomePolicy, dry_run: bool) -> HomeSweepReport {
    let mut report = HomeSweepReport::default();
    if parent.file_name().is_none_or(|n| n != "rightkit-qa") || !safe_dir(parent) {
        return report;
    }
    let Ok(canonical_parent) = fs::canonicalize(parent) else {
        return report;
    };
    let Ok(entries) = fs::read_dir(&canonical_parent) else {
        return report;
    };
    let now = now_ms();
    let young_ms = policy.young.as_millis();
    let mut entries_found: Vec<HomeEntry> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(ft) = entry.file_type() else { continue };
        if !ft.is_dir() || ft.is_symlink() || name.starts_with('.') {
            continue;
        }
        let dir = entry.path();
        let binding_path = dir.join(".rightkit-app.json");
        let Some(binding) = fs::read(&binding_path)
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        else {
            report
                .retained
                .push((dir, "no readable binding file: not proven ours".into()));
            continue;
        };
        if read_home_marker(&dir).is_some_and(|(pid, _)| pid_live(pid)) {
            report.retained.push((dir, "owner process alive".into()));
            continue;
        }
        let bytes = tree_bytes(&dir);
        report.total_bytes = report.total_bytes.saturating_add(bytes);
        let field = |k: &str| binding.get(k).and_then(|v| v.as_str()).map(str::to_owned);
        let marker = field("runRoot")
            .and_then(|root| read_marker(&Path::new(&root).join(RUN_MARKER)))
            .filter(|m| {
                Some(&m.app) == field("app").as_ref()
                    && Some(&m.run_id) == field("runId").as_ref()
                    && m.app_root.as_deref().is_some_and(|r| r == dir)
            });
        let (kind, stamp) = match marker {
            Some(m) if marker_active(&m, now) => (HomeKind::Active, m.created_at),
            Some(m) => (HomeKind::Finished, m.completed_at.unwrap_or(m.created_at)),
            None => {
                let stamp = modified_ms(&binding_path)
                    .into_iter()
                    .chain(modified_ms(&dir))
                    .max()
                    .unwrap_or(now);
                (HomeKind::Orphan, stamp)
            }
        };
        entries_found.push(HomeEntry {
            dir,
            bytes,
            stamp,
            kind,
        });
    }
    entries_found.sort_by_key(|e| e.stamp);

    let mut remaining = report.total_bytes;
    let mut doomed: Vec<(usize, &'static str)> = Vec::new();
    for (i, e) in entries_found.iter().enumerate() {
        let aged = now.saturating_sub(e.stamp) >= young_ms;
        match e.kind {
            HomeKind::Active => report.retained.push((e.dir.clone(), "active run".into())),
            _ if !aged => {}
            HomeKind::Finished => doomed.push((i, "run over and older than the window")),
            HomeKind::Orphan => doomed.push((i, "run record gone and older than the window")),
        }
    }
    for (i, _) in &doomed {
        remaining = remaining.saturating_sub(entries_found[*i].bytes);
    }
    // Size bound: oldest finished homes go next, whatever their age. Orphans are unproven dead
    // when young, so they stay.
    // Only when it can work: deleting finished homes that still leave the total over the bound
    // (live or young homes hold the rest) would lose debugging state for nothing.
    let finished_pool: u64 = entries_found
        .iter()
        .enumerate()
        .filter(|(i, e)| {
            matches!(e.kind, HomeKind::Finished) && !doomed.iter().any(|(d, _)| d == i)
        })
        .map(|(_, e)| e.bytes)
        .sum();
    if remaining > policy.max_bytes && remaining.saturating_sub(finished_pool) <= policy.max_bytes {
        for (i, e) in entries_found.iter().enumerate() {
            if remaining <= policy.max_bytes {
                break;
            }
            if matches!(e.kind, HomeKind::Finished) && !doomed.iter().any(|(d, _)| *d == i) {
                doomed.push((i, "run over; homes exceed the size bound"));
                remaining = remaining.saturating_sub(e.bytes);
            }
        }
    }
    let doomed_idx: std::collections::HashSet<usize> = doomed.iter().map(|(i, _)| *i).collect();
    for (i, reason) in doomed {
        let e = &entries_found[i];
        let removable = safe_dir(&e.dir)
            && fs::canonicalize(&e.dir)
                .is_ok_and(|c| c == e.dir && c.parent() == Some(canonical_parent.as_path()));
        if !removable {
            report
                .retained
                .push((e.dir.clone(), "path is not a plain home directory".into()));
            continue;
        }
        if !dry_run {
            if let Err(error) = remove_home_tree(&canonical_parent, &e.dir) {
                report.retained.push((e.dir.clone(), error));
                continue;
            }
        }
        report.removed_bytes = report.removed_bytes.saturating_add(e.bytes);
        report
            .removed
            .push((e.dir.clone(), e.bytes, reason.to_string()));
    }
    for (i, e) in entries_found.iter().enumerate() {
        if !doomed_idx.contains(&i) && !matches!(e.kind, HomeKind::Active) {
            report
                .retained
                .push((e.dir.clone(), "younger than the window".into()));
        }
    }
    report
}

/// [`sweep_homes`] with the default policy, at most once per [`SWEEP_INTERVAL`] across processes.
/// Never fails the caller.
fn sweep_homes_throttled(parent: &Path) {
    let stamp = parent.join(".last-sweep");
    let due = modified_ms(&stamp)
        .is_none_or(|t| now_ms().saturating_sub(t) >= SWEEP_INTERVAL.as_millis());
    if !due {
        return;
    }
    // Claim the interval first so concurrent starters do not all walk the tree.
    let _ = fs::write(&stamp, now_ms().to_string());
    let _ = sweep_homes(parent, HomePolicy::default(), false);
}

// ---------------------------------------------------------------------------------------------
// QA root hygiene: every home is removed when its run ends; dead homes are swept at run start;
// a product whose own homes hold too much, or a root that holds far too much, refuses to start
// another run.

/// Marker inside each home: the owning process and when the run started.
pub const HOME_MARKER: &str = ".rightkit-home.json";
/// Binding file inside each home: the run record it belongs to and its product (`app`).
const BINDING_FILE: &str = ".rightkit-app.json";
/// Bytes one product's homes may hold after the start-of-run sweep. `0` disables the cap. The cap
/// is per product: a home belongs to the product named by its binding file (else by its directory
/// name prefix), and other products' homes never count against it.
pub const ROOT_CAP_ENV: &str = "RIGHTKIT_QA_ROOT_CAP_BYTES";
/// Bytes the whole `rightkit-qa` root may hold, all products together, after the start-of-run
/// sweep. Above it every run is refused, because the disk itself is at risk. `0` disables it.
pub const ROOT_CEILING_ENV: &str = "RIGHTKIT_QA_ROOT_CEILING_BYTES";
/// Seconds a home whose owner is dead must be untouched before the start-of-run sweep removes it.
pub const HOME_MAX_AGE_ENV: &str = "RIGHTKIT_QA_HOME_MAX_AGE_SECS";
/// `1` keeps the home of a failed run for inspection (its path and size are printed).
pub const KEEP_FAILED_HOME_ENV: &str = "RIGHTKIT_QA_KEEP_FAILED_HOME";
pub const DEFAULT_ROOT_CAP_BYTES: u64 = 4_000_000_000;
pub const DEFAULT_ROOT_CEILING_BYTES: u64 = 32_000_000_000;
pub const DEFAULT_HOME_MAX_AGE: Duration = Duration::from_secs(2 * 3600);
/// The app key is cut to this many characters to make the product label in a home's name.
const HOME_LABEL_LEN: usize = 16;

fn bytes_from(raw: Option<&str>, default: u64) -> u64 {
    raw.and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

/// The per-product cap from the environment, else [`DEFAULT_ROOT_CAP_BYTES`].
pub fn root_cap_bytes() -> u64 {
    bytes_from(
        std::env::var(ROOT_CAP_ENV).ok().as_deref(),
        DEFAULT_ROOT_CAP_BYTES,
    )
}

/// The root-wide ceiling from the environment, else [`DEFAULT_ROOT_CEILING_BYTES`].
pub fn root_ceiling_bytes() -> u64 {
    bytes_from(
        std::env::var(ROOT_CEILING_ENV).ok().as_deref(),
        DEFAULT_ROOT_CEILING_BYTES,
    )
}

/// The limits a run start enforces on the QA root, as an app sets them in code. A value set here
/// wins over the environment variable; `None` leaves the limit to the environment, then the
/// default; `Some(0)` disables it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RootLimits {
    /// Per-product cap ([`ROOT_CAP_ENV`]).
    pub cap_bytes: Option<u64>,
    /// Root-wide ceiling ([`ROOT_CEILING_ENV`]).
    pub ceiling_bytes: Option<u64>,
}

/// [`RootLimits`] after setter, environment and default are applied. `0` means disabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedRootLimits {
    pub cap_bytes: u64,
    pub ceiling_bytes: u64,
}

impl RootLimits {
    pub fn with_cap_bytes(mut self, bytes: u64) -> Self {
        self.cap_bytes = Some(bytes);
        self
    }

    pub fn with_ceiling_bytes(mut self, bytes: u64) -> Self {
        self.ceiling_bytes = Some(bytes);
        self
    }

    /// Setter, else environment, else default.
    pub fn resolve(&self) -> ResolvedRootLimits {
        self.resolve_with(&|key: &str| std::env::var(key).ok())
    }

    fn resolve_with(&self, env: &dyn Fn(&str) -> Option<String>) -> ResolvedRootLimits {
        ResolvedRootLimits {
            cap_bytes: self.cap_bytes.unwrap_or_else(|| {
                bytes_from(env(ROOT_CAP_ENV).as_deref(), DEFAULT_ROOT_CAP_BYTES)
            }),
            ceiling_bytes: self.ceiling_bytes.unwrap_or_else(|| {
                bytes_from(env(ROOT_CEILING_ENV).as_deref(), DEFAULT_ROOT_CEILING_BYTES)
            }),
        }
    }
}

pub fn home_max_age() -> Duration {
    std::env::var(HOME_MAX_AGE_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_HOME_MAX_AGE)
}

pub fn keep_failed_home_requested() -> bool {
    std::env::var(KEEP_FAILED_HOME_ENV).is_ok_and(|v| matches!(v.trim(), "1" | "true" | "yes"))
}

fn write_home_marker(home: &Path, app: &str, run_id: &str) -> Result<()> {
    write_whole(
        &home.join(HOME_MARKER),
        &serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 1, "pid": std::process::id(), "startedAtMs": now_ms() as u64,
            "app": app, "runId": run_id,
        }))?,
    )
}

/// Write a binding or marker file so that another process (a retention sweep reads these while
/// runs start) sees the whole file or none of it: temp file, flush, rename.
fn write_whole(path: &Path, bytes: &[u8]) -> Result<()> {
    rightkit_fs::atomic_write(path, bytes).map_err(std::io::Error::from)?;
    Ok(())
}

/// Write the runner binding of one per-scenario directory under a home, through the same
/// whole-file write as the home's own binding. The identity (`app`, `runRoot`, `runId`) is copied
/// from the home's existing binding, never made up here; `scenario` is added as extra runner
/// metadata. A home without a readable binding is refused.
///
/// This is bookkeeping a test-mode app can find at the root it is told to use. It does not
/// authenticate the runner: rightkit-fs checks only that the binding's `app` equals the app's id.
pub(crate) fn write_scenario_binding(home: &Path, dir: &Path, scenario: &str) -> Result<()> {
    let home_binding = read_binding(home).ok_or_else(|| {
        crate::util::Error(format!(
            "{}: no readable runner binding; a scenario directory is bound only from its home's binding",
            home.display()
        ))
    })?;
    let field = |key: &str| {
        home_binding
            .get(key)
            .and_then(serde_json::Value::as_str)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| {
                crate::util::Error(format!(
                    "{}: runner binding has no `{key}` to bind a scenario directory to",
                    home.display()
                ))
            })
    };
    write_whole(
        &dir.join(BINDING_FILE),
        &serde_json::to_vec(&serde_json::json!({
            "app": field("app")?, "runRoot": field("runRoot")?, "runId": field("runId")?,
            "scenario": scenario,
        }))?,
    )
}

/// `(pid, started_at_ms)` recorded in a home.
fn read_home_marker(home: &Path) -> Option<(u32, u128)> {
    let v: serde_json::Value =
        serde_json::from_slice(&fs::read(home.join(HOME_MARKER)).ok()?).ok()?;
    let pid = u32::try_from(v.get("pid")?.as_u64()?).ok()?;
    Some((pid, u128::from(v.get("startedAtMs")?.as_u64()?)))
}

/// The binding file of a home, when it is readable JSON object.
fn read_binding(home: &Path) -> Option<serde_json::Value> {
    fs::read(home.join(BINDING_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .filter(serde_json::Value::is_object)
}

/// The product label a home's directory name carries: the app key cut to [`HOME_LABEL_LEN`].
fn product_label(app: &str) -> String {
    app.chars().take(HOME_LABEL_LEN).collect()
}

/// The product label of a directory name that follows the naming pattern of a QA home, else
/// `None`. The pattern is `<label>-<suffix>`: the label is an app key cut to 16 characters (a
/// safe key: ASCII letters, digits, `.`, `_`, `-`, starting with a letter or digit), and the suffix
/// is the 16 lowercase hex digits this crate appends or the 6 letters and digits `mkdtemp` appends
/// in `@rightkit/qa`. Both runners write into the same root, so both suffixes are accepted.
fn parse_home_name(name: &str) -> Option<&str> {
    let (label, suffix) = name.rsplit_once('-')?;
    let suffix_ok = (suffix.len() == 16
        && suffix
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f')))
        || (suffix.len() == 6 && suffix.bytes().all(|b| b.is_ascii_alphanumeric()));
    let label_ok = !label.is_empty()
        && label.chars().count() <= HOME_LABEL_LEN
        && label.starts_with(|c: char| c.is_ascii_alphanumeric())
        && label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    (suffix_ok && label_ok).then_some(label)
}

/// The product a home belongs to: the `app` in its binding file when present, else the label in
/// its directory name, else the whole name (a directory this runner did not name).
fn home_product(name: &str, binding: Option<&serde_json::Value>) -> String {
    binding
        .and_then(|b| b.get("app"))
        .and_then(|v| v.as_str())
        .filter(|app| !app.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| parse_home_name(name).unwrap_or(name).to_owned())
}

/// Two product names are the same product when their home-name labels are equal.
fn same_product(home_product: &str, app: &str) -> bool {
    product_label(home_product) == product_label(app)
}

/// A directory this runner created as a QA home: a real directory directly under `rightkit-qa`
/// with the binding file. `workspace::dispose` deletes a home only when this holds.
pub(crate) fn is_owned_home(home: &Path) -> bool {
    safe_dir(home)
        && home.parent().and_then(Path::file_name) == Some(std::ffi::OsStr::new("rightkit-qa"))
        && home.join(BINDING_FILE).is_file()
}

/// `<home base>/rightkit-qa`, the root this runner owns.
pub fn qa_root() -> PathBuf {
    home_base().join("rightkit-qa")
}

/// Remove `dir` only when it is a plain directory directly inside `canonical_parent`. Never
/// follows a link out of the root ([`remove_home_tree`] removes a link as itself).
fn remove_home_dir(dir: &Path, canonical_parent: &Path) -> std::result::Result<(), String> {
    let plain = safe_dir(dir)
        && fs::canonicalize(dir).is_ok_and(|c| c == dir && c.parent() == Some(canonical_parent));
    if !plain {
        return Err("path is not a plain home directory".into());
    }
    remove_home_tree(canonical_parent, dir)
}

/// Remove the QA home `home` and everything in it. `qa_root` is the `rightkit-qa` directory the
/// caller may delete under: a `home` that is not a real directory strictly inside it, after both
/// paths are resolved, is refused and nothing is touched.
///
/// A link met in the tree (a symlink, or on Windows a junction) is removed as itself. It is never
/// listed and never followed, so what it points at stays, inside the home or outside it. The
/// binding file goes last, just before the home directory: a removal that stops part way leaves a
/// home that [`is_owned_home`] and the sweeps can still prove ours, so a later run finishes it.
/// The error names the entry that could not be removed.
///
/// `std::fs::remove_dir_all` is not used here. On Windows it fails with "Access is denied" on a
/// home whose app used WinINet, because WinINet leaves the junction
/// `AppData\Local\Microsoft\Windows\INetCache\Content.IE5` with an entry that denies listing to
/// everyone, and it removes entries in directory order, so the binding file (its name sorts first)
/// was already gone when it failed (gap ledger 216). The test
/// `std_remove_dir_all_stops_at_a_junction_that_denies_listing` shows the failure; that the cause
/// inside the standard library is an open of the junction with list access is our reading of it.
pub(crate) fn remove_home_tree(qa_root: &Path, home: &Path) -> std::result::Result<(), String> {
    let (Ok(root), Ok(real)) = (fs::canonicalize(qa_root), fs::canonicalize(home)) else {
        return Err(format!("{}: cannot be resolved", home.display()));
    };
    if root.file_name().is_none_or(|n| n != "rightkit-qa")
        || !safe_dir(home)
        || !is_inside(&root, &real)
    {
        return Err(format!(
            "{}: not a directory inside the QA root {}",
            home.display(),
            root.display()
        ));
    }
    let binding = std::ffi::OsStr::new(BINDING_FILE);
    remove_children(&real, Some(binding))?;
    remove_children(&real, None)?;
    fs::remove_dir(&real).map_err(|e| removal_error(&real, &e))
}

fn removal_error(path: &Path, error: &std::io::Error) -> String {
    format!("{}: {error}", path.display())
}

/// Remove everything in `dir` except the entry named `keep`. Links are removed as themselves.
pub(crate) fn remove_children(
    dir: &Path,
    keep: Option<&std::ffi::OsStr>,
) -> std::result::Result<(), String> {
    for entry in fs::read_dir(dir).map_err(|e| removal_error(dir, &e))? {
        let entry = entry.map_err(|e| removal_error(dir, &e))?;
        if keep == Some(entry.file_name().as_os_str()) {
            continue;
        }
        let path = entry.path();
        // From the directory listing: the entry itself is not opened, so a link is not followed.
        let kind = entry.file_type().map_err(|e| removal_error(&path, &e))?;
        let outcome = if kind.is_symlink() {
            remove_link(&entry)
        } else if kind.is_dir() {
            remove_children(&path, None)?;
            fs::remove_dir(&path)
        } else {
            remove_plain_file(&path)
        };
        match outcome {
            Ok(()) => {}
            // Someone else removed it first: gone is what was wanted.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(removal_error(&path, &e)),
        }
    }
    Ok(())
}

/// Remove a link itself. Windows removes a link to a directory (a junction included) as a
/// directory and a link to a file as a file; which one it is comes from the listing.
fn remove_link(entry: &fs::DirEntry) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
        if entry.metadata()?.file_attributes() & FILE_ATTRIBUTE_DIRECTORY != 0 {
            return fs::remove_dir(entry.path());
        }
    }
    fs::remove_file(entry.path())
}

/// Remove a file. Windows refuses a read-only file, so that attribute is cleared and the removal
/// tried once more; elsewhere the attribute of the file does not decide.
fn remove_plain_file(path: &Path) -> std::io::Result<()> {
    let first = match fs::remove_file(path) {
        Ok(()) => return Ok(()),
        Err(e) => e,
    };
    #[cfg(windows)]
    if first.kind() == std::io::ErrorKind::PermissionDenied {
        if let Ok(meta) = fs::symlink_metadata(path) {
            let mut permissions = meta.permissions();
            if permissions.readonly() {
                #[allow(clippy::permissions_set_readonly_false)]
                permissions.set_readonly(false);
                if fs::set_permissions(path, permissions).is_ok() {
                    return fs::remove_file(path);
                }
            }
        }
    }
    Err(first)
}

/// True when `dir` itself or anything under it was modified after `cutoff_ms` (milliseconds since
/// the epoch). Symlinks are examined, never followed. Stops at the first newer entry. A tree that
/// cannot be inspected counts as recently modified: it is not proven idle.
fn modified_after(dir: &Path, cutoff_ms: u128) -> bool {
    if modified_ms(dir).is_none_or(|t| t > cutoff_ms) {
        return true;
    }
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = fs::read_dir(&d) else {
            return true;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if modified_ms(&path).is_none_or(|t| t > cutoff_ms) {
                return true;
            }
            if entry.file_type().is_ok_and(|ft| ft.is_dir()) {
                stack.push(path);
            }
        }
    }
    false
}

/// Whether a home that has no owner marker (`.rightkit-home.json`, or no binding file beside it)
/// may be swept. `Ok(reason)` removes it, `Err(why)` keeps it.
///
/// "Dead" for such a home means every one of these holds:
/// * its name matches the home naming pattern ([`parse_home_name`]);
/// * no pid recorded for it is alive: the pid in a home marker that has no binding beside it, and
///   the `ownerPid` of the run record its binding names (`runRoot`), when either is readable
///   ([`pid_live`]: a process that cannot be measured counts as alive);
/// * nothing in the home, the directory itself included, was modified within `max_age_ms`.
///
/// Not checked: which processes have the home as their working directory or an open file in it
/// (no process table walk), the run record's lease (`leaseUntil` is a timer set when the run
/// began, not proof of a live process, and a cancelled run's lease is what kept these homes for
/// 24 hours), and whether a pid in the record now belongs to an unrelated process (that keeps the
/// home). A process that holds the home but writes nothing for the whole window is not noticed.
fn unmarked_home_verdict(
    dir: &Path,
    name: &str,
    binding: Option<&serde_json::Value>,
    marker_pid: Option<u32>,
    now: u128,
    max_age_ms: u128,
) -> std::result::Result<&'static str, String> {
    if parse_home_name(name).is_none() {
        return Err("no owner marker and the name is not a QA home name: not proven ours".into());
    }
    let record_pid = binding
        .and_then(|b| b.get("runRoot"))
        .and_then(|v| v.as_str())
        .and_then(|root| read_marker(&Path::new(root).join(RUN_MARKER)))
        .map(|m| m.owner_pid);
    for pid in marker_pid.into_iter().chain(record_pid) {
        if pid_live(pid) {
            return Err(format!(
                "no owner marker; recorded owner process {pid} is alive"
            ));
        }
    }
    if modified_after(dir, now.saturating_sub(max_age_ms)) {
        return Err("no owner marker; modified within the window".into());
    }
    Ok("no owner marker, no live recorded owner, untouched for longer than the window")
}

/// Remove the homes of dead runs. Two kinds of home exist:
///
/// * A home with an owner marker (`.rightkit-home.json`) and a binding file goes when its recorded
///   process is gone and its run started more than `max_age` ago. A home whose pid is alive is
///   never touched.
/// * Any other home (written before the marker existed, or missing a file) goes only under
///   `unmarked_home_verdict`: a QA home name, no live recorded pid, and nothing in it modified
///   within `max_age`. That function states what is and is not checked.
///
/// Symlinks, dot-directories, anything outside `parent` (the `.../rightkit-qa` directory), and any
/// path that is not a plain directory directly inside it are never removed. `dry_run` reports
/// without deleting.
pub fn clean_homes(parent: &Path, max_age: Duration, dry_run: bool) -> HomeSweepReport {
    let mut report = HomeSweepReport::default();
    if parent.file_name().is_none_or(|n| n != "rightkit-qa") || !safe_dir(parent) {
        return report;
    }
    let Ok(canonical_parent) = fs::canonicalize(parent) else {
        return report;
    };
    let Ok(entries) = fs::read_dir(&canonical_parent) else {
        return report;
    };
    let now = now_ms();
    let max_age_ms = max_age.as_millis();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let Ok(ft) = entry.file_type() else { continue };
        let dir = entry.path();
        if ft.is_symlink() {
            report.retained.push((dir, "symlink skipped".into()));
            continue;
        }
        if !ft.is_dir() {
            continue;
        }
        let binding = read_binding(&dir);
        let owner = read_home_marker(&dir);
        let bytes = tree_bytes(&dir);
        report.total_bytes = report.total_bytes.saturating_add(bytes);
        let verdict: std::result::Result<&'static str, String> = match (&binding, owner) {
            (Some(_), Some((pid, started))) => {
                if pid_live(pid) {
                    Err("owner process alive".into())
                } else if now.saturating_sub(started) < max_age_ms {
                    Err("owner gone but younger than the window".into())
                } else {
                    Ok("owner process gone and older than the window")
                }
            }
            _ => unmarked_home_verdict(
                &dir,
                &name,
                binding.as_ref(),
                owner.map(|(pid, _)| pid),
                now,
                max_age_ms,
            ),
        };
        match verdict {
            Err(why) => report.retained.push((dir, why)),
            Ok(reason) => {
                if !dry_run {
                    if let Err(why) = remove_home_dir(&dir, &canonical_parent) {
                        report.retained.push((dir, why));
                        continue;
                    }
                }
                report.removed_bytes = report.removed_bytes.saturating_add(bytes);
                report.removed.push((dir, bytes, reason.to_string()));
            }
        }
    }
    report
}

/// [`clean_homes`] on this runner's own root with the environment's bounds. The entry point for
/// apps and for `rightkit-qa clean`.
pub fn clean(dry_run: bool) -> HomeSweepReport {
    clean_homes(&qa_root(), home_max_age(), dry_run)
}

/// Which limit a refused run exceeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootLimitKind {
    /// The run's own product holds more than its per-product cap.
    ProductCap,
    /// All products together hold more than the root-wide ceiling.
    RootCeiling,
}

/// A run was refused because a QA root limit is exceeded even after the sweep.
#[derive(Debug, Clone)]
pub struct RootOverCap {
    pub kind: RootLimitKind,
    pub root: PathBuf,
    /// The product the refused run is for.
    pub app: String,
    /// Bytes counted against the limit: the product's homes, or every home.
    pub total_bytes: u64,
    /// Homes counted against the limit.
    pub home_count: usize,
    /// The limit that was exceeded.
    pub limit_bytes: u64,
    /// Largest homes first: `(home, bytes, product)`.
    pub largest: Vec<(PathBuf, u64, String)>,
}

/// `"5.26 GB (5260000000 bytes)"`, or `"812 bytes"` below a megabyte.
fn human_bytes(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.2} GB ({bytes} bytes)", bytes as f64 / 1e9)
    } else if bytes >= 1_000_000 {
        format!("{:.1} MB ({bytes} bytes)", bytes as f64 / 1e6)
    } else {
        format!("{bytes} bytes")
    }
}

impl std::fmt::Display for RootOverCap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            RootLimitKind::ProductCap => write!(
                f,
                "QA run for `{app}` refused: the {app} homes in {root} hold {total} in {n} home(s), over the {limit} per-product cap ({env}; `Harness::with_root_cap_bytes` sets it in code, 0 disables). Other products' homes are not counted. Largest {app} homes:",
                app = self.app,
                root = self.root.display(),
                total = human_bytes(self.total_bytes),
                n = self.home_count,
                limit = human_bytes(self.limit_bytes),
                env = ROOT_CAP_ENV,
            )?,
            RootLimitKind::RootCeiling => write!(
                f,
                "QA run for `{app}` refused: {root} holds {total} across all products in {n} home(s), over the {limit} root ceiling ({env}; `Harness::with_root_ceiling_bytes` sets it in code, 0 disables), so the disk may be at risk. Largest homes:",
                app = self.app,
                root = self.root.display(),
                total = human_bytes(self.total_bytes),
                n = self.home_count,
                limit = human_bytes(self.limit_bytes),
                env = ROOT_CEILING_ENV,
            )?,
        }
        for (home, bytes, product) in &self.largest {
            write!(
                f,
                " {} [{product}] ({});",
                home.display(),
                human_bytes(*bytes)
            )?;
        }
        write!(
            f,
            " Run `rightkit-qa clean` to remove the homes of dead runs (owner process gone and untouched for the home age bound, 2 hours by default, {HOME_MAX_AGE_ENV}); a home it keeps belongs to a live or recent run, so remove it by hand once you know that run is over, then retry"
        )
    }
}
impl std::error::Error for RootOverCap {}
impl From<RootOverCap> for crate::util::Error {
    fn from(e: RootOverCap) -> Self {
        crate::util::Error(e.to_string())
    }
}

/// Fail with [`RootOverCap`] when the homes of the product `app` hold more than `cap_bytes`
/// (`0` disables the check). Other products' homes are not counted.
pub fn check_root_cap(
    parent: &Path,
    app: &str,
    cap_bytes: u64,
) -> std::result::Result<(), RootOverCap> {
    check_root_limits(parent, app, cap_bytes, 0)
}

/// Fail with [`RootOverCap`] when the homes of the product `app` hold more than `cap_bytes`
/// (checked first), or when every directory under `parent` together holds more than
/// `ceiling_bytes`. `0` disables a limit. A home's product is the `app` in its binding file, else
/// the label in its directory name. Symlinks are not followed or counted.
pub fn check_root_limits(
    parent: &Path,
    app: &str,
    cap_bytes: u64,
    ceiling_bytes: u64,
) -> std::result::Result<(), RootOverCap> {
    if (cap_bytes == 0 && ceiling_bytes == 0) || !safe_dir(parent) {
        return Ok(());
    }
    struct SizedHome {
        path: PathBuf,
        bytes: u64,
        product: String,
        own: bool,
    }
    let mut homes: Vec<SizedHome> = Vec::new();
    if let Ok(entries) = fs::read_dir(parent) {
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            if !ft.is_dir() || ft.is_symlink() {
                continue;
            }
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let product = home_product(&name, read_binding(&path).as_ref());
            let own = same_product(&product, app);
            // Another product's home is sized only for the ceiling.
            let bytes = if (own && cap_bytes > 0) || ceiling_bytes > 0 {
                tree_bytes(&path)
            } else {
                0
            };
            homes.push(SizedHome {
                path,
                bytes,
                product,
                own,
            });
        }
    }
    fn refusal(
        kind: RootLimitKind,
        limit_bytes: u64,
        parent: &Path,
        app: &str,
        mut counted: Vec<&SizedHome>,
    ) -> RootOverCap {
        let total_bytes = counted
            .iter()
            .fold(0u64, |sum, home| sum.saturating_add(home.bytes));
        let home_count = counted.len();
        counted.sort_by_key(|home| std::cmp::Reverse(home.bytes));
        RootOverCap {
            kind,
            root: parent.to_path_buf(),
            app: app.to_string(),
            total_bytes,
            home_count,
            limit_bytes,
            largest: counted
                .into_iter()
                .take(5)
                .map(|home| (home.path.clone(), home.bytes, home.product.clone()))
                .collect(),
        }
    }
    if cap_bytes > 0 {
        let own: Vec<&SizedHome> = homes.iter().filter(|home| home.own).collect();
        let own_total = own
            .iter()
            .fold(0u64, |sum, home| sum.saturating_add(home.bytes));
        if own_total > cap_bytes {
            return Err(refusal(
                RootLimitKind::ProductCap,
                cap_bytes,
                parent,
                app,
                own,
            ));
        }
    }
    if ceiling_bytes > 0 {
        let all: Vec<&SizedHome> = homes.iter().collect();
        let total = all
            .iter()
            .fold(0u64, |sum, home| sum.saturating_add(home.bytes));
        if total > ceiling_bytes {
            return Err(refusal(
                RootLimitKind::RootCeiling,
                ceiling_bytes,
                parent,
                app,
                all,
            ));
        }
    }
    Ok(())
}

/// Sweep dead homes under `parent`, then the 24 hour rules (at most every ten minutes).
/// `RIGHTKIT_QA_HOME_SWEEP=0` skips both.
fn sweep_root_at_run_start(parent: &Path) {
    if std::env::var(HOME_SWEEP_ENV).is_ok_and(|v| v.trim() == "0") {
        return;
    }
    let _ = clean_homes(parent, home_max_age(), false);
    sweep_homes_throttled(parent);
}

/// [`check_root_limits`] with `limits` resolved against `env`: a limit set in code wins over the
/// environment, and the environment over the default.
fn enforce_limits_with(
    parent: &Path,
    app: &str,
    limits: RootLimits,
    env: &dyn Fn(&str) -> Option<String>,
) -> std::result::Result<(), RootOverCap> {
    let limits = limits.resolve_with(env);
    check_root_limits(parent, app, limits.cap_bytes, limits.ceiling_bytes)
}

/// The sweep half of the start-of-run preflight, for a caller that checks the limits later (the
/// harness, so a limit set after construction takes effect).
pub(crate) fn sweep_home_root() {
    let parent = qa_root();
    if safe_dir(&parent) {
        sweep_root_at_run_start(&parent);
    }
}

/// The limit half of the start-of-run preflight for the product `app`.
pub(crate) fn enforce_root_limits(
    app: &str,
    limits: RootLimits,
) -> std::result::Result<(), RootOverCap> {
    let parent = qa_root();
    if !safe_dir(&parent) {
        return Ok(());
    }
    enforce_limits_with(&parent, app, limits, &|key: &str| std::env::var(key).ok())
}

/// Run before a run is created: sweep dead homes, then refuse to start when the run's own product
/// is over its cap or the root is over its ceiling (`limits` set in code, else the environment,
/// else the defaults).
pub(crate) fn preflight_home_root(app: &str, limits: RootLimits) -> Result<()> {
    sweep_home_root();
    enforce_root_limits(app, limits)?;
    Ok(())
}

/// `"<path> (<bytes> bytes)"` for a kept home.
pub fn describe_kept_home(home: &Path) -> String {
    format!("{} ({} bytes)", home.display(), tree_bytes(home))
}

fn owned_app_root(root: &Path, marker: &RunMarker) -> Result<Option<PathBuf>> {
    let Some(home) = &marker.app_root else {
        return Ok(None);
    };
    if !home.exists() {
        return Ok(None);
    }
    let parent = home
        .parent()
        .ok_or_else(|| crate::util::Error("invalid QA app temp root".into()))?;
    let temp = parent
        .parent()
        .ok_or_else(|| crate::util::Error("invalid QA app temp root".into()))?;
    let allowed_temp = temp == fs::canonicalize(app_temp_dir())?
        || fs::canonicalize(home_base()).is_ok_and(|base| temp == base)
        || (cfg!(target_os = "macos")
            && (temp == Path::new("/private/tmp")
                || (temp.starts_with("/private/var/folders")
                    && temp.file_name().is_some_and(|n| n == "T"))));
    if parent.file_name().is_none_or(|n| n != "rightkit-qa")
        || !allowed_temp
        || fs::canonicalize(home)? != *home
        || !safe_dir(home)
    {
        return err("invalid QA app temp root");
    }
    let binding: serde_json::Value =
        serde_json::from_slice(&fs::read(home.join(".rightkit-app.json"))?)?;
    if binding
        != serde_json::json!({"runRoot": fs::canonicalize(root)?, "app": marker.app,
        "runId": marker.run_id})
    {
        return err("QA app temp ownership mismatch");
    }
    Ok(Some(home.clone()))
}

pub(crate) fn remove_app_root(root: &Path, marker: &RunMarker) -> Result<()> {
    if let Some(home) = owned_app_root(root, marker)? {
        let Some(qa_root) = home.parent() else {
            return err("invalid QA app temp root");
        };
        if let Err(why) = remove_home_tree(qa_root, &home) {
            return err(why);
        }
    }
    Ok(())
}

pub(crate) fn copy_tree(source: &Path, target: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(source)?;
    if meta.file_type().is_symlink() {
        return err("symlink in QA app evidence");
    }
    if target
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        return err("symlink in managed QA evidence");
    }
    if meta.is_dir() {
        ensure_dir(target)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_tree(&entry.path(), &target.join(entry.file_name()))?;
        }
    } else if meta.is_file() {
        ensure_dir(
            target
                .parent()
                .ok_or_else(|| crate::util::Error("missing evidence parent".into()))?,
        )?;
        fs::copy(source, target)?;
    } else {
        return err("unsupported QA app evidence file");
    }
    Ok(())
}

/// A plain file in a run directory that says its owner asked to keep the run's data
/// (`SuiteOptions::keep_data`): the run's home and worlds are what the caller reads after the run.
/// Both removal paths honour it (`prune_unlocked` and `retention::prune_runs`), whatever the
/// run's outcome, age or count, so a later or concurrent run of the same app cannot remove it.
pub const KEEP_DATA_FILE: &str = ".rightkit-keep-data";

/// Mark the run at `run_root` as kept: call it before the run's outcome is recorded, so a pass that
/// reads the marker never sees a finished run whose keep request is not there yet.
pub fn mark_keep_data(run_root: &Path) -> std::io::Result<()> {
    fs::write(
        run_root.join(KEEP_DATA_FILE),
        "the run's owner asked to keep its data; retention does not remove this run\n",
    )
}

/// True when `run_dir` carries [`KEEP_DATA_FILE`] as a plain file (a link of that name does not
/// count). The one check both retention rules use.
pub fn run_keeps_data(run_dir: &Path) -> bool {
    fs::symlink_metadata(run_dir.join(KEEP_DATA_FILE))
        .map(|m| m.is_file())
        .unwrap_or(false)
}

pub fn prune(root: &Path, app: &str, max_bytes: Option<u64>) -> Result<RetentionReport> {
    prune_with_keep(root, app, max_bytes, &[])
}

pub fn prune_with_keep(
    root: &Path,
    app: &str,
    max_bytes: Option<u64>,
    keep_run_ids: &[String],
) -> Result<RetentionReport> {
    prune_with_policy(
        root,
        app,
        max_bytes,
        keep_run_ids,
        Duration::from_millis(DEFAULT_YOUNG_MS as u64),
    )
}

pub fn prune_with_policy(
    root: &Path,
    app: &str,
    max_bytes: Option<u64>,
    keep_run_ids: &[String],
    young_for: Duration,
) -> Result<RetentionReport> {
    prune_with_policy_count(
        root,
        app,
        max_bytes,
        keep_run_ids,
        young_for,
        DEFAULT_KEEP_COUNT,
    )
}

pub fn prune_with_age_override(
    root: &Path,
    app: &str,
    keep_run_ids: &[String],
    young_for: Duration,
) -> Result<RetentionReport> {
    prune_with_policy_count(root, app, Some(0), keep_run_ids, young_for, 0)
}

fn prune_with_policy_count(
    root: &Path,
    app: &str,
    max_bytes: Option<u64>,
    keep_run_ids: &[String],
    young_for: Duration,
    keep_count: usize,
) -> Result<RetentionReport> {
    let app = safe_key(app, "app")?;
    let app_root = managed_root(Some(root)).join("runs").join(&app);
    let initial = RetentionReport {
        cap_bytes: max_bytes.unwrap_or(DEFAULT_MAX_BYTES),
        ..Default::default()
    };
    if !safe_dir(&app_root) {
        return Ok(initial);
    }
    let _guard = lock_or_wait(&app_root, PRUNE_LOCK_WAIT)?;
    {
        let mut report = RetentionReport::default();
        prune_unlocked(
            &app_root,
            &app,
            max_bytes.unwrap_or(DEFAULT_MAX_BYTES),
            keep_count,
            young_for.as_millis(),
            keep_run_ids,
            &mut report,
        )?;
        Ok(report)
    }
}

fn prune_unlocked(
    app_root: &Path,
    app: &str,
    cap: u64,
    keep_count: usize,
    young_ms: u128,
    keep_run_ids: &[String],
    report: &mut RetentionReport,
) -> Result<()> {
    let now = now_ms();
    report.cap_bytes = cap;
    let mut runs = Vec::new();
    let entries = match fs::read_dir(app_root) {
        Ok(x) => x,
        Err(_) => return Ok(()),
    };
    for entry in entries.flatten() {
        let ft = match entry.file_type() {
            Ok(x) => x,
            Err(_) => continue,
        };
        if ft.is_symlink() {
            report
                .retained
                .push((entry.path(), "symlink skipped".into()));
            continue;
        }
        if !ft.is_dir() || entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let dir = entry.path();
        if !is_inside(app_root, &dir) {
            continue;
        }
        let Some(marker) = read_marker(&dir.join(RUN_MARKER)) else {
            if let Some(bytes) = directory_bytes(&dir) {
                report.unknown_bytes = report.unknown_bytes.saturating_add(bytes);
            }
            report
                .retained
                .push((dir, "unknown or malformed run metadata".into()));
            continue;
        };
        if marker.app != app || marker.run_id != entry.file_name().to_string_lossy() {
            if let Some(bytes) = directory_bytes(&dir) {
                report.unknown_bytes = report.unknown_bytes.saturating_add(bytes);
            }
            report
                .retained
                .push((dir, "run marker identity mismatch".into()));
            continue;
        }
        let Some(bytes) = directory_bytes(&dir) else {
            report
                .retained
                .push((dir, "symlink or unreadable content skipped".into()));
            continue;
        };
        let active =
            (marker.owner_pid > 0 && pid_live(marker.owner_pid)) || marker.lease_until > now;
        runs.push((dir, marker, bytes, active));
    }
    runs.sort_by(|a, b| {
        b.1.created_at
            .cmp(&a.1.created_at)
            .then_with(|| a.1.run_id.cmp(&b.1.run_id))
    });
    let count_protected: std::collections::HashSet<String> = runs
        .iter()
        .take(keep_count)
        .map(|x| x.1.run_id.clone())
        .collect();
    let protected: std::collections::HashSet<String> = runs
        .iter()
        .filter(|x| now.saturating_sub(x.1.created_at) < young_ms || x.3 || run_keeps_data(&x.0))
        .map(|x| x.1.run_id.clone())
        .collect();
    let caller_protected: std::collections::HashSet<&str> =
        keep_run_ids.iter().map(String::as_str).collect();
    let protected: std::collections::HashSet<String> = protected
        .into_iter()
        .chain(caller_protected.iter().map(|x| (*x).to_string()))
        .collect();
    // A failed run is kept by the count rule too; only the byte cap, a disk-safety limit, can
    // still evict one (oldest first).
    let failed: std::collections::HashSet<String> = runs
        .iter()
        .filter(|x| x.1.outcome == Some(RunOutcome::Failed))
        .map(|x| x.1.run_id.clone())
        .collect();
    let mut total: u64 = runs.iter().map(|x| x.2).sum();
    let mut old: Vec<_> = runs
        .iter()
        .filter(|x| {
            !protected.contains(&x.1.run_id)
                && (total > cap
                    || (!count_protected.contains(&x.1.run_id) && !failed.contains(&x.1.run_id)))
        })
        .collect();
    old.sort_by(|a, b| {
        a.1.created_at
            .cmp(&b.1.created_at)
            .then_with(|| a.1.run_id.cmp(&b.1.run_id))
    });
    let mut removed = std::collections::HashSet::new();
    for item in old {
        if total <= cap
            && (count_protected.contains(&item.1.run_id) || failed.contains(&item.1.run_id))
        {
            continue;
        }
        if !safe_dir(&item.0) {
            continue;
        }
        match remove_app_root(&item.0, &item.1)
            .and_then(|()| fs::remove_dir_all(&item.0).map_err(Into::into))
        {
            Ok(()) => {
                report.removed.push(item.0.clone());
                removed.insert(item.1.run_id.clone());
                total = total.saturating_sub(item.2);
            }
            Err(e) => report.retained.push((item.0.clone(), e.to_string())),
        }
    }
    report.blocked_bytes = runs
        .iter()
        .filter(|x| {
            !removed.contains(&x.1.run_id)
                && (protected.contains(&x.1.run_id)
                    || (total <= cap
                        && (count_protected.contains(&x.1.run_id) || failed.contains(&x.1.run_id))))
        })
        .map(|x| x.2)
        .sum();
    report.total_bytes = total.saturating_add(report.unknown_bytes);
    report.over_cap_bytes = report.total_bytes.saturating_sub(report.cap_bytes);
    for item in runs.iter().filter(|x| {
        !removed.contains(&x.1.run_id)
            && (protected.contains(&x.1.run_id)
                || (total <= cap
                    && (count_protected.contains(&x.1.run_id) || failed.contains(&x.1.run_id))))
    }) {
        report.retained.push((
            item.0.clone(),
            if item.3 {
                "active run".into()
            } else if now.saturating_sub(item.1.created_at) < young_ms {
                "younger than retention window".into()
            } else if run_keeps_data(&item.0) {
                "kept data (the run's owner asked to keep it)".into()
            } else if failed.contains(&item.1.run_id) {
                "failed run kept".into()
            } else {
                "newest protected run".into()
            },
        ));
    }
    Ok(())
}

/// How long an explicit prune waits for another prune of the same app before it reports
/// [`LOCK_HELD`]. Pruning a large tree takes seconds, so this is generous.
const PRUNE_LOCK_WAIT: Duration = Duration::from_secs(120);
/// A lock directory with no owner file this old was abandoned between `mkdir` and the owner write.
const OWNERLESS_LOCK_GRACE: Duration = Duration::from_secs(30);
/// Prefix of the error message for a retention lock that stayed held. [`is_lock_held`] tests for it.
pub const LOCK_HELD: &str = "QA retention lock is held";

/// True when `error` is the retention lock staying held past an explicit prune's wait.
pub fn is_lock_held(error: &crate::util::Error) -> bool {
    error.0.starts_with(LOCK_HELD)
}

/// The retention lock, released on drop (including a panic in the prune).
pub(crate) struct LockGuard(PathBuf);

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Take the lock, waiting up to `wait`. `None` means a live owner still held it.
/// A lock whose owner process is dead, or that never got an owner file, is recovered.
pub(crate) fn try_lock(app_root: &Path, wait: Duration) -> Result<Option<LockGuard>> {
    ensure_dir(app_root)?;
    let lock = app_root.join(".retention-lock");
    let owner = lock.join("owner.json");
    let deadline = std::time::Instant::now() + wait;
    loop {
        match fs::create_dir(&lock) {
            Ok(()) => {
                let guard = LockGuard(lock.clone());
                let mut file = File::create(&owner)?;
                write!(
                    file,
                    "{{\"pid\":{},\"createdAt\":{}}}",
                    std::process::id(),
                    now_ms()
                )?;
                return Ok(Some(guard));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let pid = fs::read_to_string(&owner)
                    .ok()
                    .and_then(|x| serde_json::from_str::<serde_json::Value>(&x).ok())
                    .and_then(|x| x.get("pid").and_then(|v| v.as_u64()))
                    .and_then(|x| u32::try_from(x).ok())
                    .filter(|p| *p > 0);
                let abandoned = match pid {
                    Some(pid) => !pid_live(pid),
                    None => fs::metadata(&lock)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|age| age >= OWNERLESS_LOCK_GRACE),
                };
                if abandoned {
                    let _ = fs::remove_dir_all(&lock);
                    continue;
                }
                if std::time::Instant::now() >= deadline {
                    return Ok(None);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            // Windows: a directory that is being removed keeps its name until the last handle on
            // it closes, and creating that name meanwhile is refused with "access denied", not
            // "already exists". Another starter is releasing the lock (or reading it while it is
            // released): the lock counts as held, and a starter never fails for it.
            Err(e) if cfg!(windows) && e.kind() == std::io::ErrorKind::PermissionDenied => {
                if std::time::Instant::now() >= deadline {
                    return Ok(None);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(e.into()),
        }
    }
}

fn lock_or_wait(app_root: &Path, wait: Duration) -> Result<LockGuard> {
    try_lock(app_root, wait)?.ok_or_else(|| {
        crate::util::Error(format!(
            "{LOCK_HELD}: {}",
            app_root.join(".retention-lock").display()
        ))
    })
}

pub(crate) fn write_marker(root: &Path, marker: &RunMarker) -> Result<()> {
    let target = root.join(RUN_MARKER);
    let temp = root.join(format!(".{RUN_MARKER}.{}.tmp", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    file.write_all(serde_json::to_string(marker)?.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(temp, target)?;
    Ok(())
}

pub(crate) fn read_marker(path: &Path) -> Option<RunMarker> {
    let marker: RunMarker = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    if marker.schema_version != 1
        || safe_key(&marker.app, "app").is_err()
        || safe_key(&marker.run_id, "run id").is_err()
        || marker.created_at == 0
    {
        return None;
    }
    Some(marker)
}

pub(crate) fn marker_active(marker: &RunMarker, now: u128) -> bool {
    (marker.owner_pid > 0 && pid_live(marker.owner_pid)) || marker.lease_until > now
}

pub(crate) fn directory_bytes(root: &Path) -> Option<u64> {
    let mut total = 0u64;
    fn walk(dir: &Path, total: &mut u64) -> bool {
        let entries = match fs::read_dir(dir) {
            Ok(x) => x,
            Err(_) => return false,
        };
        for e in entries.flatten() {
            let ft = match e.file_type() {
                Ok(x) => x,
                Err(_) => return false,
            };
            if ft.is_symlink() {
                return false;
            }
            if ft.is_dir() {
                if !walk(&e.path(), total) {
                    return false;
                }
            } else if ft.is_file() {
                let Some(size) = e.metadata().ok().map(|m| m.len()) else {
                    return false;
                };
                let Some(next) = total.checked_add(size) else {
                    return false;
                };
                *total = next;
            } else {
                return false;
            }
        }
        true
    }
    walk(root, &mut total).then_some(total)
}

fn ensure_dir(dir: &Path) -> Result<()> {
    let mut current = PathBuf::new();
    for component in dir.components() {
        current.push(component.as_os_str());
        if current.exists() && fs::symlink_metadata(&current)?.file_type().is_symlink() {
            return err(format!(
                "refusing symlink in managed path: {}",
                current.display()
            ));
        }
    }
    fs::create_dir_all(dir)?;
    Ok(())
}
pub(crate) fn safe_dir(dir: &Path) -> bool {
    fs::symlink_metadata(dir)
        .map(|m| m.is_dir() && !m.file_type().is_symlink())
        .unwrap_or(false)
}
pub(crate) fn safe_key(value: &str, name: &str) -> Result<String> {
    if !value.is_empty()
        && value
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        Ok(value.to_string())
    } else {
        err(format!("unsafe QA {name}: {value}"))
    }
}
pub(crate) fn is_inside(root: &Path, candidate: &Path) -> bool {
    candidate
        .strip_prefix(root)
        .map(|r| {
            !r.as_os_str().is_empty()
                && !r
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
        })
        .unwrap_or(false)
}
fn pid_live(pid: u32) -> bool {
    let Some(pid) = std::num::NonZeroU32::new(pid) else {
        return false;
    };
    // An unmeasurable process counts as live: a sweep must never reap a run it cannot prove dead.
    rightkit_child_process::AdoptedProcess::new(pid)
        .is_running()
        .unwrap_or(true)
}

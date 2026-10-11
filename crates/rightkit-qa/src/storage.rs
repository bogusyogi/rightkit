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
    };
    write_marker(&run, &marker)?;
    Ok(ManagedRun { root: run, marker })
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
/// Must be an absolute path. Unset or empty keeps the system default ([`app_temp_dir`]).
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
    let label: String = run.marker.app.chars().take(16).collect();
    let home = parent.join(format!("{}-{}", label, &new_id().replace('-', "")[..16]));
    fs::create_dir(&home)?;
    run.marker.app_root = Some(home.clone());
    // Bind both sides; a forged path in a managed marker cannot delete arbitrary temp data.
    fs::write(
        home.join(".rightkit-app.json"),
        serde_json::to_vec(&serde_json::json!({
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
            if let Err(error) = fs::remove_dir_all(&e.dir) {
                report.retained.push((e.dir.clone(), error.to_string()));
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
// a root over its size cap refuses to start another run.

/// Marker inside each home: the owning process and when the run started.
pub const HOME_MARKER: &str = ".rightkit-home.json";
/// Bytes the whole `rightkit-qa` root may hold after the start-of-run sweep. `0` disables the cap.
pub const ROOT_CAP_ENV: &str = "RIGHTKIT_QA_ROOT_CAP_BYTES";
/// Seconds a home whose owner is dead must be old before the start-of-run sweep removes it.
pub const HOME_MAX_AGE_ENV: &str = "RIGHTKIT_QA_HOME_MAX_AGE_SECS";
/// `1` keeps the home of a failed run for inspection (its path and size are printed).
pub const KEEP_FAILED_HOME_ENV: &str = "RIGHTKIT_QA_KEEP_FAILED_HOME";
pub const DEFAULT_ROOT_CAP_BYTES: u64 = 4_000_000_000;
pub const DEFAULT_HOME_MAX_AGE: Duration = Duration::from_secs(2 * 3600);

pub fn root_cap_bytes() -> u64 {
    std::env::var(ROOT_CAP_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_ROOT_CAP_BYTES)
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
    fs::write(
        home.join(HOME_MARKER),
        serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 1, "pid": std::process::id(), "startedAtMs": now_ms() as u64,
            "app": app, "runId": run_id,
        }))?,
    )?;
    Ok(())
}

/// `(pid, started_at_ms)` recorded in a home.
fn read_home_marker(home: &Path) -> Option<(u32, u128)> {
    let v: serde_json::Value =
        serde_json::from_slice(&fs::read(home.join(HOME_MARKER)).ok()?).ok()?;
    let pid = u32::try_from(v.get("pid")?.as_u64()?).ok()?;
    Some((pid, u128::from(v.get("startedAtMs")?.as_u64()?)))
}

/// A directory this runner created as a QA home: a real directory directly under `rightkit-qa`
/// with the binding file. `workspace::dispose` deletes a home only when this holds.
pub(crate) fn is_owned_home(home: &Path) -> bool {
    safe_dir(home)
        && home.parent().and_then(Path::file_name) == Some(std::ffi::OsStr::new("rightkit-qa"))
        && home.join(".rightkit-app.json").is_file()
}

/// `<home base>/rightkit-qa`, the root this runner owns.
pub fn qa_root() -> PathBuf {
    home_base().join("rightkit-qa")
}

/// Remove `dir` only when it is a plain directory directly inside `canonical_parent`. Never
/// follows a symlink out of the root (`remove_dir_all` unlinks symlinks it meets).
fn remove_home_dir(dir: &Path, canonical_parent: &Path) -> std::result::Result<(), String> {
    let plain = safe_dir(dir)
        && fs::canonicalize(dir).is_ok_and(|c| c == dir && c.parent() == Some(canonical_parent));
    if !plain {
        return Err("path is not a plain home directory".into());
    }
    fs::remove_dir_all(dir).map_err(|e| e.to_string())
}

/// Remove the homes of dead runs. A home goes when its recorded process is gone and it is older
/// than `max_age`; a home whose pid is alive is never touched, nor is a directory without the
/// binding file, a symlink, or anything outside `parent` (the `.../rightkit-qa` directory).
/// Homes from before the pid marker existed fall back to their run record's liveness.
/// `dry_run` reports without deleting.
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
        let (alive, started) = match read_home_marker(&dir) {
            Some((pid, started)) => (pid_live(pid), started),
            None => {
                let run = binding
                    .get("runRoot")
                    .and_then(|v| v.as_str())
                    .and_then(|root| read_marker(&Path::new(root).join(RUN_MARKER)));
                match run {
                    Some(m) => (marker_active(&m, now), m.created_at),
                    None => (
                        false,
                        modified_ms(&binding_path)
                            .into_iter()
                            .chain(modified_ms(&dir))
                            .max()
                            .unwrap_or(now),
                    ),
                }
            }
        };
        let bytes = tree_bytes(&dir);
        report.total_bytes = report.total_bytes.saturating_add(bytes);
        if alive {
            report.retained.push((dir, "owner process alive".into()));
            continue;
        }
        if now.saturating_sub(started) < max_age.as_millis() {
            report
                .retained
                .push((dir, "owner gone but younger than the window".into()));
            continue;
        }
        if !dry_run {
            if let Err(why) = remove_home_dir(&dir, &canonical_parent) {
                report.retained.push((dir, why));
                continue;
            }
        }
        report.removed_bytes = report.removed_bytes.saturating_add(bytes);
        report.removed.push((
            dir,
            bytes,
            "owner process gone and older than the window".into(),
        ));
    }
    report
}

/// [`clean_homes`] on this runner's own root with the environment's bounds. The entry point for
/// apps and for `rightkit-qa clean`.
pub fn clean(dry_run: bool) -> HomeSweepReport {
    clean_homes(&qa_root(), home_max_age(), dry_run)
}

/// The QA root holds more than its cap even after the sweep.
#[derive(Debug, Clone)]
pub struct RootOverCap {
    pub root: PathBuf,
    pub total_bytes: u64,
    pub cap_bytes: u64,
    /// Largest homes first: `(home, bytes)`.
    pub largest: Vec<(PathBuf, u64)>,
}

impl std::fmt::Display for RootOverCap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "QA root {} holds {} bytes, over its {} byte cap ({}); largest homes:",
            self.root.display(),
            self.total_bytes,
            self.cap_bytes,
            ROOT_CAP_ENV
        )?;
        for (home, bytes) in &self.largest {
            write!(f, " {} ({bytes} bytes);", home.display())?;
        }
        f.write_str(" run `rightkit-qa clean`, or remove those homes, then retry")
    }
}
impl std::error::Error for RootOverCap {}
impl From<RootOverCap> for crate::util::Error {
    fn from(e: RootOverCap) -> Self {
        crate::util::Error(e.to_string())
    }
}

/// Fail with [`RootOverCap`] when the directories under `parent` hold more than `cap_bytes`
/// (`0` disables the check).
pub fn check_root_cap(parent: &Path, cap_bytes: u64) -> std::result::Result<(), RootOverCap> {
    if cap_bytes == 0 || !safe_dir(parent) {
        return Ok(());
    }
    let mut homes: Vec<(PathBuf, u64)> = Vec::new();
    if let Ok(entries) = fs::read_dir(parent) {
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() && !ft.is_symlink() {
                homes.push((entry.path(), tree_bytes(&entry.path())));
            }
        }
    }
    let total: u64 = homes.iter().map(|(_, b)| *b).sum();
    if total <= cap_bytes {
        return Ok(());
    }
    homes.sort_by(|a, b| b.1.cmp(&a.1));
    homes.truncate(5);
    Err(RootOverCap {
        root: parent.to_path_buf(),
        total_bytes: total,
        cap_bytes,
        largest: homes,
    })
}

/// Run before a run is created: sweep dead homes, then refuse to start over the cap. Skipped in
/// this crate's own unit tests, which run against the developer's real root.
pub(crate) fn preflight_home_root() -> Result<()> {
    if cfg!(test) {
        return Ok(());
    }
    let parent = qa_root();
    if !safe_dir(&parent) {
        return Ok(());
    }
    if std::env::var(HOME_SWEEP_ENV).map_or(true, |v| v.trim() != "0") {
        let _ = clean_homes(&parent, home_max_age(), false);
        sweep_homes_throttled(&parent);
    }
    check_root_cap(&parent, root_cap_bytes())?;
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
        fs::remove_dir_all(home)?;
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
        .filter(|x| now.saturating_sub(x.1.created_at) < young_ms || x.3)
        .map(|x| x.1.run_id.clone())
        .collect();
    let caller_protected: std::collections::HashSet<&str> =
        keep_run_ids.iter().map(String::as_str).collect();
    let protected: std::collections::HashSet<String> = protected
        .into_iter()
        .chain(caller_protected.iter().map(|x| (*x).to_string()))
        .collect();
    let mut total: u64 = runs.iter().map(|x| x.2).sum();
    let mut old: Vec<_> = runs
        .iter()
        .filter(|x| {
            !protected.contains(&x.1.run_id)
                && (total > cap || !count_protected.contains(&x.1.run_id))
        })
        .collect();
    old.sort_by(|a, b| {
        a.1.created_at
            .cmp(&b.1.created_at)
            .then_with(|| a.1.run_id.cmp(&b.1.run_id))
    });
    let mut removed = std::collections::HashSet::new();
    for item in old {
        if total <= cap && count_protected.contains(&item.1.run_id) {
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
                    || (total <= cap && count_protected.contains(&x.1.run_id)))
        })
        .map(|x| x.2)
        .sum();
    report.total_bytes = total.saturating_add(report.unknown_bytes);
    report.over_cap_bytes = report.total_bytes.saturating_sub(report.cap_bytes);
    for item in runs.iter().filter(|x| {
        !removed.contains(&x.1.run_id)
            && (protected.contains(&x.1.run_id)
                || (total <= cap && count_protected.contains(&x.1.run_id)))
    }) {
        report.retained.push((
            item.0.clone(),
            if item.3 {
                "active run".into()
            } else if now.saturating_sub(item.1.created_at) < young_ms {
                "younger than retention window".into()
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
struct LockGuard(PathBuf);

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Take the lock, waiting up to `wait`. `None` means a live owner still held it.
/// A lock whose owner process is dead, or that never got an owner file, is recovered.
fn try_lock(app_root: &Path, wait: Duration) -> Result<Option<LockGuard>> {
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

fn write_marker(root: &Path, marker: &RunMarker) -> Result<()> {
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

fn directory_bytes(root: &Path) -> Option<u64> {
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
fn safe_dir(dir: &Path) -> bool {
    fs::symlink_metadata(dir)
        .map(|m| m.is_dir() && !m.file_type().is_symlink())
        .unwrap_or(false)
}
fn safe_key(value: &str, name: &str) -> Result<String> {
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
fn is_inside(root: &Path, candidate: &Path) -> bool {
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
    rightkit_process::AdoptedProcess::new(pid)
        .is_running()
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Set the modification time of a file or directory. Windows opens a directory only with
    /// backup semantics, and changing the time needs write-attributes access.
    fn set_modified(path: &Path, time: std::time::SystemTime) {
        #[cfg(windows)]
        let handle = {
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
            const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
            fs::OpenOptions::new()
                .access_mode(FILE_WRITE_ATTRIBUTES)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
                .open(path)
                .unwrap()
        };
        #[cfg(not(windows))]
        let handle = File::open(path).unwrap();
        handle.set_modified(time).unwrap();
    }

    fn temp_root(label: &str) -> PathBuf {
        // `app_temp_dir`, not `std::env::temp_dir`: managed paths refuse symlinks, and the managed
        // build's TMPDIR (`/tmp`) is one on macOS.
        let root = app_temp_dir().join(format!(
            "rkqa-storage-{label}-{}-{}",
            std::process::id(),
            new_id()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn age(run: &ManagedRun) {
        age_at(run, 1);
    }

    fn age_at(run: &ManagedRun, created_at: u128) {
        let mut marker = read_marker(&run.root.join(RUN_MARKER)).unwrap();
        marker.created_at = created_at;
        marker.owner_pid = 0;
        marker.lease_until = 0;
        write_marker(&run.root, &marker).unwrap();
    }

    #[test]
    fn newest_five_are_kept_under_byte_cap() {
        let root = temp_root("count");
        let mut runs = Vec::new();
        for i in 0..7 {
            let run = create(
                Some(&root),
                "count",
                Some(&format!("run-{i}")),
                Duration::from_secs(1),
            )
            .unwrap();
            runs.push(run);
        }
        for (i, run) in runs.iter().enumerate() {
            finish(run).unwrap();
            age_at(run, 100 + i as u128);
        }
        let report = prune(&root, "count", Some(1024 * 1024)).unwrap();
        assert_eq!(report.removed.len(), 2);
        assert_eq!(report.over_cap_bytes, 0);
        assert!(root.join("runs/count/run-5").exists());
        assert!(root.join("runs/count/run-6").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn young_run_is_protected_when_cap_requires_old_eviction() {
        let root = temp_root("young");
        let old = create(Some(&root), "young", Some("old"), Duration::from_secs(1)).unwrap();
        let young = create(Some(&root), "young", Some("young"), Duration::from_secs(1)).unwrap();
        finish(&old).unwrap();
        finish(&young).unwrap();
        age(&old);
        let report = prune(&root, "young", Some(0)).unwrap();
        assert!(report.removed.iter().any(|p| p.ends_with("old")));
        assert!(root.join("runs/young/young").exists());
        assert!(report.over_cap_bytes > 0);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn live_pid_and_future_lease_are_protected() {
        let root = temp_root("active");
        let live = create(Some(&root), "active", Some("live"), Duration::from_secs(1)).unwrap();
        let lease = create(
            Some(&root),
            "active",
            Some("lease"),
            Duration::from_secs(3600),
        )
        .unwrap();
        let old = create(Some(&root), "active", Some("old"), Duration::from_secs(1)).unwrap();
        finish(&lease).unwrap();
        let mut lease_marker = read_marker(&lease.root.join(RUN_MARKER)).unwrap();
        lease_marker.created_at = 1;
        lease_marker.lease_until = now_ms() + 3600 * 1000;
        write_marker(&lease.root, &lease_marker).unwrap();
        let mut live_marker = read_marker(&live.root.join(RUN_MARKER)).unwrap();
        live_marker.created_at = 1;
        write_marker(&live.root, &live_marker).unwrap();
        finish(&old).unwrap();
        age(&old);
        let report = prune(&root, "active", Some(0)).unwrap();
        assert!(report.removed.iter().any(|p| p.ends_with("old")));
        assert!(root.join("runs/active/live").exists());
        assert!(root.join("runs/active/lease").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn malformed_and_symlink_roots_are_retained_and_reported() {
        let root = temp_root("unknown");
        let app_root = prepare_app_root(&root, "unknown").unwrap();
        let malformed = app_root.join("bad");
        fs::create_dir_all(&malformed).unwrap();
        fs::write(malformed.join("payload"), b"unknown").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&malformed, app_root.join("link")).unwrap();
        let report = prune(&root, "unknown", Some(0)).unwrap();
        assert!(report.removed.is_empty());
        assert!(report.unknown_bytes >= 7);
        assert!(report.retained.iter().any(|(p, _)| p.ends_with("bad")));
        #[cfg(unix)]
        assert!(report
            .retained
            .iter()
            .any(|(p, reason)| p.ends_with("link") && reason.contains("symlink")));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn caller_keep_id_blocks_eviction_and_reports_overage() {
        let root = temp_root("keep");
        let keep = create(Some(&root), "keep", Some("keep-me"), Duration::from_secs(1)).unwrap();
        let remove = create(
            Some(&root),
            "keep",
            Some("remove-me"),
            Duration::from_secs(1),
        )
        .unwrap();
        finish(&keep).unwrap();
        finish(&remove).unwrap();
        age(&keep);
        age(&remove);
        let report = prune_with_keep(&root, "keep", Some(0), &["keep-me".into()]).unwrap();
        assert!(report.removed.iter().any(|p| p.ends_with("remove-me")));
        assert!(root.join("runs/keep/keep-me").exists());
        assert!(report.over_cap_bytes > 0);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn create_refuses_when_protected_data_exceeds_cap() {
        let root = temp_root("blocked");
        let run = create(
            Some(&root),
            "blocked",
            Some("live"),
            Duration::from_secs(3600),
        )
        .unwrap();
        let mut file = File::create(run.root.join("large")).unwrap();
        file.set_len(DEFAULT_MAX_BYTES + 1).unwrap();
        file.flush().unwrap();
        let result = create(Some(&root), "blocked", Some("next"), Duration::from_secs(1));
        assert!(result.is_err());
        assert!(!root.join("runs/blocked/next").exists());
        let _ = fs::remove_dir_all(root);
    }

    // ---- retention lock (regression: parallel scenarios failed with "lock is held") ----

    fn dead_pid() -> u32 {
        let mut child = std::process::Command::new(if cfg!(windows) { "cmd" } else { "true" })
            .args(if cfg!(windows) {
                vec!["/C", "exit"]
            } else {
                vec![]
            })
            .spawn()
            .unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    }

    #[test]
    fn a_starter_does_not_wait_for_or_fail_on_a_held_prune_lock() {
        let root = temp_root("lock-held");
        let first = create(Some(&root), "app", Some("one"), Duration::from_secs(60)).unwrap();
        let app_root = first.root.parent().unwrap().to_path_buf();
        let guard = try_lock(&app_root, Duration::ZERO)
            .unwrap()
            .expect("lock is free");
        let started = std::time::Instant::now();
        let second = create(Some(&root), "app", Some("two"), Duration::from_secs(60)).unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "starter waited on the lock"
        );
        assert!(second.root.join(RUN_MARKER).exists());
        drop(guard);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn parallel_starters_in_one_process_all_proceed() {
        let root = temp_root("lock-threads");
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let root = root.clone();
                std::thread::spawn(move || {
                    create(
                        Some(&root),
                        "app",
                        Some(&format!("run{i}")),
                        Duration::from_secs(60),
                    )
                    .map(|r| r.root)
                })
            })
            .collect();
        for h in handles {
            let run = h.join().unwrap().expect("every starter proceeds");
            assert!(run.join(RUN_MARKER).exists());
        }
        let _ = fs::remove_dir_all(&root);
    }

    /// Child half of `parallel_starters_in_two_processes_all_proceed`: inert unless driven.
    #[test]
    fn child_starter_for_the_two_process_test() {
        let (Some(root), Some(id)) = (
            std::env::var_os("RKQA_TEST_LOCK_ROOT"),
            std::env::var_os("RKQA_TEST_LOCK_ID"),
        ) else {
            return;
        };
        let started = std::time::Instant::now();
        create(
            Some(Path::new(&root)),
            "app",
            id.to_str(),
            Duration::from_secs(60),
        )
        .unwrap();
        assert!(started.elapsed() < Duration::from_secs(20));
    }

    #[test]
    fn parallel_starters_in_two_processes_all_proceed() {
        let root = temp_root("lock-procs");
        let exe = std::env::current_exe().unwrap();
        let spawned: Vec<_> = (0..3)
            .map(|i| {
                std::process::Command::new(&exe)
                    .args([
                        "--exact",
                        "storage::tests::child_starter_for_the_two_process_test",
                        "--nocapture",
                    ])
                    .env("RKQA_TEST_LOCK_ROOT", &root)
                    .env("RKQA_TEST_LOCK_ID", format!("par{i}"))
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .unwrap()
            })
            .collect();
        for child in spawned {
            let out = child.wait_with_output().unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stdout)
            );
        }
        for i in 0..3 {
            assert!(root
                .join("runs/app")
                .join(format!("par{i}"))
                .join(RUN_MARKER)
                .exists());
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_lock_held_by_a_dead_process_is_recovered() {
        let root = temp_root("lock-dead");
        let app_root = root.join("runs/app");
        fs::create_dir_all(app_root.join(".retention-lock")).unwrap();
        fs::write(
            app_root.join(".retention-lock/owner.json"),
            format!("{{\"pid\":{},\"createdAt\":1}}", dead_pid()),
        )
        .unwrap();
        assert!(try_lock(&app_root, Duration::ZERO).unwrap().is_some());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_ownerless_lock_is_recovered_only_after_the_grace_period() {
        let root = temp_root("lock-ownerless");
        let app_root = root.join("runs/app");
        let lock = app_root.join(".retention-lock");
        fs::create_dir_all(&lock).unwrap();
        assert!(
            try_lock(&app_root, Duration::ZERO).unwrap().is_none(),
            "fresh: owner may be mid-write"
        );
        let old = std::time::SystemTime::now() - Duration::from_secs(120);
        set_modified(&lock, old);
        assert!(try_lock(&app_root, Duration::ZERO).unwrap().is_some());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_explicit_prune_that_cannot_get_the_lock_fails_with_the_typed_error() {
        let root = temp_root("lock-typed");
        let app_root = root.join("runs/app");
        fs::create_dir_all(&app_root).unwrap();
        let _guard = try_lock(&app_root, Duration::ZERO).unwrap().unwrap();
        let error = lock_or_wait(&app_root, Duration::from_millis(60))
            .err()
            .expect("held");
        assert!(is_lock_held(&error), "{error}");
        let _ = fs::remove_dir_all(&root);
    }

    // ---- symlink guard ----

    #[cfg(unix)]
    #[test]
    fn a_symlink_above_the_root_is_resolved_and_a_link_below_it_is_refused() {
        let base = fs::canonicalize(temp_root("symlink-guard")).unwrap();
        let real = base.join("real");
        fs::create_dir_all(&real).unwrap();
        let alias = base.join("alias");
        std::os::unix::fs::symlink(&real, &alias).unwrap();

        // The managed root sits behind an alias, the way `/var/folders/...` sits behind `/var`.
        let run = create(
            Some(&alias.join("managed")),
            "app",
            Some("alias-run"),
            Duration::from_secs(60),
        )
        .unwrap();
        assert!(run.root.starts_with(&real), "{}", run.root.display());

        // A link below the root is still refused.
        let elsewhere = base.join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        let managed = real.join("managed");
        std::os::unix::fs::symlink(&elsewhere, managed.join("runs/linked")).unwrap();
        let e = ensure_dir(&managed.join("runs/linked/deeper")).unwrap_err();
        assert!(e.0.contains("refusing symlink"), "{}", e.0);
        let _ = fs::remove_dir_all(&base);
    }

    // ---- app home sweep ----

    fn homes_parent(label: &str) -> (PathBuf, PathBuf) {
        let root = temp_root(label);
        let parent = fs::canonicalize(&root).unwrap().join("rightkit-qa");
        fs::create_dir_all(&parent).unwrap();
        (root, parent)
    }

    fn home_for(root: &Path, parent: &Path, run_id: &str, payload: usize) -> (ManagedRun, PathBuf) {
        let mut run = create(Some(root), "app", Some(run_id), Duration::from_secs(60)).unwrap();
        let home = create_app_root_in(&mut run, parent).unwrap();
        // The sweep tests model homes of other, finished processes: record a dead owner.
        set_home_marker(&home, 0, now_ms());
        fs::write(home.join("payload.bin"), vec![0u8; payload]).unwrap();
        (run, home)
    }

    fn set_home_marker(home: &Path, pid: u32, started_at_ms: u128) {
        fs::write(
            home.join(HOME_MARKER),
            serde_json::to_vec(&serde_json::json!({
                "schemaVersion": 1, "pid": pid, "startedAtMs": started_at_ms as u64,
                "app": "app", "runId": "x",
            }))
            .unwrap(),
        )
        .unwrap();
    }

    fn make_finished(run: &ManagedRun, completed_at: u128) {
        let mut marker = read_marker(&run.root.join(RUN_MARKER)).unwrap();
        marker.owner_pid = 0;
        marker.lease_until = 0;
        marker.completed_at = Some(completed_at);
        write_marker(&run.root, &marker).unwrap();
    }

    fn make_old(path: &Path) {
        let old = std::time::SystemTime::now() - Duration::from_secs(3 * 24 * 3600);
        set_modified(path, old);
    }

    #[test]
    fn sweep_removes_old_orphans_and_finished_homes_and_spares_the_rest() {
        let (root, parent) = homes_parent("sweep");
        let day = Duration::from_secs(24 * 3600);
        let policy = HomePolicy {
            young: day,
            max_bytes: u64::MAX,
        };

        // Orphan (run record deleted), old: removed.
        let (orphan_run, orphan_old) = home_for(&root, &parent, "orphan-old", 1000);
        fs::remove_dir_all(&orphan_run.root).unwrap();
        make_old(&orphan_old.join(".rightkit-app.json"));
        make_old(&orphan_old);
        // Orphan, young: kept (liveness unknown).
        let (orphan_run2, orphan_young) = home_for(&root, &parent, "orphan-young", 1000);
        fs::remove_dir_all(&orphan_run2.root).unwrap();
        // Finished, old: removed.
        let (done_run, done_old) = home_for(&root, &parent, "done-old", 1000);
        make_finished(&done_run, 1);
        // Finished, just now: kept.
        let (done_run2, done_young) = home_for(&root, &parent, "done-young", 1000);
        make_finished(&done_run2, now_ms());
        // Active (this process owns it): kept even if the home looks old.
        let (_active_run, active) = home_for(&root, &parent, "active", 1000);
        make_old(&active);
        // Not ours (no binding): kept.
        let stranger = parent.join("stranger-0000000000000000");
        fs::create_dir_all(&stranger).unwrap();
        fs::write(stranger.join("x"), b"x").unwrap();

        let dry = sweep_homes(&parent, policy, true);
        assert_eq!(dry.removed.len(), 2, "{dry:?}");
        assert!(
            orphan_old.exists() && done_old.exists(),
            "dry run deletes nothing"
        );

        let report = sweep_homes(&parent, policy, false);
        assert_eq!(report.removed.len(), 2, "{report:?}");
        assert!(!orphan_old.exists() && !done_old.exists());
        for kept in [&orphan_young, &done_young, &active, &stranger] {
            assert!(kept.exists(), "{} must survive", kept.display());
        }
        assert!(report.removed_bytes >= 2000);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn sweep_enforces_the_size_bound_on_finished_homes_oldest_first() {
        let (root, parent) = homes_parent("sweep-size");
        let policy = HomePolicy {
            young: Duration::from_secs(24 * 3600),
            max_bytes: 5_000_000,
        };
        let (r1, h1) = home_for(&root, &parent, "a", 3_000_000);
        let (r2, h2) = home_for(&root, &parent, "b", 3_000_000);
        let (r3, h3) = home_for(&root, &parent, "c", 3_000_000);
        let now = now_ms();
        make_finished(&r1, now - 3000);
        make_finished(&r2, now - 2000);
        make_finished(&r3, now - 1000);
        let report = sweep_homes(&parent, policy, false);
        assert!(!h1.exists() && !h2.exists(), "{report:?}");
        assert!(h3.exists(), "the newest finished home fits the bound");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn sweep_keeps_finished_homes_when_the_size_bound_cannot_be_met_by_removing_them() {
        let (root, parent) = homes_parent("sweep-unmet");
        let policy = HomePolicy {
            young: Duration::from_secs(24 * 3600),
            max_bytes: 5_000_000,
        };
        let (_live, live_home) = home_for(&root, &parent, "live", 6_000_000);
        let (done, done_home) = home_for(&root, &parent, "done", 1_000_000);
        make_finished(&done, now_ms());
        let report = sweep_homes(&parent, policy, false);
        assert!(report.removed.is_empty(), "{report:?}");
        assert!(live_home.exists() && done_home.exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn sweep_refuses_a_parent_that_is_not_the_homes_directory() {
        let root = temp_root("sweep-parent");
        let victim = root.join("elsewhere");
        fs::create_dir_all(victim.join("h")).unwrap();
        let report = sweep_homes(&victim, HomePolicy::default(), false);
        assert!(report.removed.is_empty());
        assert!(victim.join("h").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn home_root_override_must_be_absolute() {
        use std::ffi::OsStr;
        // A rooted path without a drive is not absolute on Windows.
        let absolute = if cfg!(windows) {
            r"D:\qa-homes"
        } else {
            "/Volumes/D/qa-homes"
        };
        assert_eq!(
            home_base_from(Some(OsStr::new(absolute))),
            PathBuf::from(absolute)
        );
        assert_eq!(
            home_base_from(Some(OsStr::new("relative/dir"))),
            app_temp_dir()
        );
        assert_eq!(home_base_from(Some(OsStr::new(""))), app_temp_dir());
        assert_eq!(home_base_from(None), app_temp_dir());
    }

    // ---- dead-home clean, cap, and containment ----

    #[test]
    fn a_created_home_records_its_owner_and_start() {
        let (root, parent) = homes_parent("home-marker");
        let (_run, home) = home_for(&root, &parent, "m", 10);
        // `home_for` overwrites it with a dead owner; create one untouched.
        let mut run = create(Some(&root), "app", Some("m2"), Duration::from_secs(60)).unwrap();
        let fresh = create_app_root_in(&mut run, &parent).unwrap();
        let (pid, started) = read_home_marker(&fresh).unwrap();
        assert_eq!(pid, std::process::id());
        assert!(now_ms().saturating_sub(started) < 60_000);
        assert!(home.exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn clean_removes_dead_old_homes_and_never_a_live_one() {
        let (root, parent) = homes_parent("clean");
        let window = Duration::from_secs(2 * 3600);
        let old = now_ms() - 3 * 3600 * 1000;
        let (_r1, dead_old) = home_for(&root, &parent, "dead-old", 100);
        set_home_marker(&dead_old, 0, old);
        let (_r2, dead_young) = home_for(&root, &parent, "dead-young", 100);
        set_home_marker(&dead_young, 0, now_ms());
        let (_r3, alive_old) = home_for(&root, &parent, "alive-old", 100);
        set_home_marker(&alive_old, std::process::id(), old);
        let stranger = parent.join("stranger-0000000000000000");
        fs::create_dir_all(&stranger).unwrap();
        fs::write(stranger.join("x"), b"x").unwrap();

        let dry = clean_homes(&parent, window, true);
        assert_eq!(dry.removed.len(), 1, "{dry:?}");
        assert!(dead_old.exists(), "dry run deletes nothing");
        let report = clean_homes(&parent, window, false);
        assert_eq!(report.removed.len(), 1, "{report:?}");
        assert!(!dead_old.exists());
        for kept in [&dead_young, &alive_old, &stranger] {
            assert!(kept.exists(), "{} must survive", kept.display());
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn clean_refuses_a_parent_that_is_not_the_qa_root() {
        let root = temp_root("clean-parent");
        let victim = root.join("elsewhere");
        fs::create_dir_all(victim.join("h")).unwrap();
        assert!(clean_homes(&victim, Duration::ZERO, false)
            .removed
            .is_empty());
        assert!(victim.join("h").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn clean_never_follows_a_symlink_out_of_the_root() {
        let (root, parent) = homes_parent("clean-symlink");
        let outside = root.join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("precious"), b"keep").unwrap();
        // A link inside a doomed home that points out of the root.
        let (_r, doomed) = home_for(&root, &parent, "doomed", 10);
        set_home_marker(&doomed, 0, 1);
        std::os::unix::fs::symlink(&outside, doomed.join("link-out")).unwrap();
        // A link directly in the QA root that points out of it.
        std::os::unix::fs::symlink(&outside, parent.join("link-home")).unwrap();
        let report = clean_homes(&parent, Duration::ZERO, false);
        assert!(!doomed.exists(), "{report:?}");
        assert_eq!(fs::read(outside.join("precious")).unwrap(), b"keep");
        assert!(fs::symlink_metadata(parent.join("link-home")).is_ok());
        // `remove_home_dir` refuses a symlinked or foreign path outright.
        let canonical_parent = fs::canonicalize(&parent).unwrap();
        assert!(remove_home_dir(&parent.join("link-home"), &canonical_parent).is_err());
        assert!(remove_home_dir(&outside, &canonical_parent).is_err());
        assert!(outside.join("precious").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn root_over_cap_names_the_largest_homes() {
        let (root, parent) = homes_parent("cap");
        let (_a, big) = home_for(&root, &parent, "big", 3_000_000);
        let (_b, small) = home_for(&root, &parent, "small", 1_000_000);
        check_root_cap(&parent, 10_000_000).unwrap();
        check_root_cap(&parent, 0).unwrap();
        let e = check_root_cap(&parent, 2_000_000).unwrap_err();
        assert!(e.total_bytes >= 4_000_000 && e.cap_bytes == 2_000_000);
        assert_eq!(e.largest[0].0, big);
        assert!(e.largest.iter().any(|(p, _)| p == &small));
        let text = crate::util::Error::from(e).to_string();
        assert!(
            text.contains(&big.display().to_string()) && text.contains(ROOT_CAP_ENV),
            "{text}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn only_a_runner_created_home_counts_as_owned() {
        let (root, parent) = homes_parent("owned");
        let (_r, home) = home_for(&root, &parent, "h", 1);
        assert!(is_owned_home(&home));
        let stranger = parent.join("stranger");
        fs::create_dir_all(&stranger).unwrap();
        assert!(!is_owned_home(&stranger));
        assert!(!is_owned_home(&root));
        let _ = fs::remove_dir_all(&root);
    }
}

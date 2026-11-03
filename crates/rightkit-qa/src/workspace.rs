//! The run-owned scratch directory of one QA run: evidence and capture dirs, staged app bundles,
//! the control channel's suite root, retrying disposal that reports what it could not remove, and a
//! sweep for crashed runs. It is NOT the app's home or data folder: the app runs on the owner's real
//! data, protected by the journey run's snapshot and restore ([`crate::snapshot`], rightkit#1437).
use crate::storage;
use crate::util::{require_text, Result};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct QaWorkspace {
    pub run_id: String,
    /// Managed run root: `<managed>/runs/<app>/<run-id>`.
    pub root: PathBuf,
    /// The run's scratch directory (`<managed run>/app`), where the runner stages launches and writes
    /// logs. The launched app never gets it as HOME or as a data folder.
    pub home: PathBuf,
    pub evidence_dir: PathBuf,
    pub capture_dir: PathBuf,
    /// Environment every launch carries: run id, evidence and capture dirs, the control channel's
    /// suite root. No HOME, profile, data-dir or temp redirection.
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Default, Clone)]
pub struct DisposeReport {
    pub removed: Vec<PathBuf>,
    pub retained: Vec<(PathBuf, String)>,
}

/// What happens before a run is created. `Full` sweeps dead homes and enforces the QA root limits
/// (the product cap and the root ceiling). `SweepOnly` sweeps and leaves the limit check to the
/// caller, which runs it later through [`storage::enforce_root_limits`].
#[derive(Clone, Copy)]
enum Preflight {
    Full(storage::RootLimits),
    SweepOnly,
}

/// Create a workspace. The QA root limits come from the environment (`RIGHTKIT_QA_ROOT_CAP_BYTES`,
/// `RIGHTKIT_QA_ROOT_CEILING_BYTES`), else the defaults; see [`create_with_limits`] to set them in
/// code.
pub fn create(cache_root: &Path, run_id: Option<&str>, app_key: &str) -> Result<QaWorkspace> {
    create_with_limits(cache_root, run_id, app_key, storage::RootLimits::default())
}

/// [`create`] with the QA root limits set in code. A limit set in `limits` wins over its
/// environment variable (`0` disables it); a limit left `None` falls back to the environment, then
/// the default. This is for a test process that cannot receive the environment variable, such as
/// one run under `rustright cargo`.
pub fn create_with_limits(
    cache_root: &Path,
    run_id: Option<&str>,
    app_key: &str,
    limits: storage::RootLimits,
) -> Result<QaWorkspace> {
    create_impl(Some(cache_root), run_id, app_key, Preflight::Full(limits))
}

/// Create under RIGHTKIT_MANAGED_ROOT (or its platform default).
pub fn create_managed(run_id: Option<&str>, app_key: &str) -> Result<QaWorkspace> {
    create_impl(
        None,
        run_id,
        app_key,
        Preflight::Full(storage::RootLimits::default()),
    )
}

/// The harness sweeps at construction and checks its limits at its first scenario, so limits set
/// with `Harness::with_root_cap_bytes` / `with_root_ceiling_bytes` after construction apply.
pub(crate) fn create_impl_for_harness(
    managed_root: Option<&Path>,
    app_key: &str,
) -> Result<QaWorkspace> {
    create_impl(managed_root, None, app_key, Preflight::SweepOnly)
}

fn create_impl(
    cache_root: Option<&Path>,
    run_id: Option<&str>,
    app_key: &str,
    preflight: Preflight,
) -> Result<QaWorkspace> {
    let app_key = require_text(app_key, "app")?;
    // Sweep dead homes and, unless the caller checks later, refuse to start over a QA root limit,
    // before a run exists.
    match preflight {
        Preflight::Full(limits) => storage::preflight_home_root(&app_key, limits)?,
        Preflight::SweepOnly => storage::sweep_home_root(),
    }
    let mut managed =
        storage::create(cache_root, &app_key, run_id, Duration::from_secs(24 * 3600))?;
    // Storage works on canonical (on Windows verbatim, `\\?\C:\...`) paths and keeps them in the
    // run marker. What this workspace hands to apps and tests is the ordinary drive path: some
    // programs reject or mis-handle the verbatim form (Orthic-Labs/rightkit#185). Both name the
    // same directory; every storage check canonicalises before it compares.
    let home = crate::util::plain_path(&storage::create_app_root(&mut managed)?);
    let run_id = managed.marker.run_id.clone();
    let root = crate::util::plain_path(&managed.root);
    // Generic managed storage creates this directory; QA profiles belong only in home.
    fs::remove_dir(root.join("data"))?;
    let evidence_dir = root.join("evidence");
    let capture_dir = root.join("captures");
    fs::create_dir_all(&evidence_dir)?;
    fs::create_dir_all(&capture_dir)?;
    fs::create_dir_all(home.join("evidence"))?;
    fs::create_dir_all(home.join("captures"))?;
    let mut env = BTreeMap::new();
    env.insert("RIGHTKIT_QA_NATIVE".into(), "1".into());
    env.insert("RIGHTKIT_QA_RUN_ID".into(), run_id.clone());
    env.insert("RIGHTKIT_RUN_ROOT".into(), home.to_string_lossy().into());
    // The control channel's discovery record only: no build reads this variable for its own data.
    env.insert(
        "RIGHTKIT_SUITE_ROOT".into(),
        home.join("suite").to_string_lossy().into(),
    );
    env.insert(
        "RIGHTKIT_QA_EVIDENCE_DIR".into(),
        home.join("evidence").to_string_lossy().into(),
    );
    env.insert(
        "RIGHTKIT_QA_CAPTURE_DIR".into(),
        home.join("captures").to_string_lossy().into(),
    );
    env.insert("RIGHTKIT_QA_MUTE".into(), "1".into());
    // Every directory an exported variable points at must exist: apps and
    // frameworks treat a missing one as "unknown path".
    for value in env.values() {
        let path = Path::new(value);
        if path.starts_with(&home) {
            fs::create_dir_all(path)?;
        }
    }
    Ok(QaWorkspace {
        run_id,
        root,
        home,
        evidence_dir,
        capture_dir,
        env,
    })
}

impl QaWorkspace {
    /// Persist an app-produced artifact before hashing it into managed evidence.
    pub(crate) fn preserve_artifact(&self, path: &Path) -> Result<PathBuf> {
        let Ok(relative) = path.strip_prefix(&self.home) else {
            return Ok(path.to_path_buf());
        };
        if relative
            .components()
            .any(|c| c == std::path::Component::ParentDir)
        {
            return crate::util::err("app artifact escapes QA app home");
        }
        let target = if let Ok(p) = relative.strip_prefix("captures") {
            self.capture_dir.join(p)
        } else {
            self.evidence_dir
                .join("app")
                .join(relative.strip_prefix("evidence").unwrap_or(relative))
        };
        storage::copy_tree(path, &target)?;
        Ok(target)
    }

    /// Record the run kind in the run marker (see [`storage::RunKind`]).
    pub fn set_kind(&self, kind: storage::RunKind) -> Result<()> {
        storage::set_run_kind(&self.root, kind)
    }

    /// Record how the run ended in the run marker. Retention reads this and nothing else; a run
    /// that never gets an outcome is never pruned by it.
    pub fn set_outcome(&self, outcome: storage::RunOutcome) -> Result<()> {
        storage::set_run_outcome(&self.root, outcome)
    }

    /// Preserve app-written outputs before marking this run complete.
    pub fn finish(&self) -> Result<()> {
        if let Some(marker) = storage::read_marker(&self.root.join(storage::RUN_MARKER)) {
            storage::finish(&storage::ManagedRun {
                root: self.root.clone(),
                marker,
            })?;
        }
        Ok(())
    }
}

/// A process that still uses a QA home.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomeHolder {
    pub pid: u32,
    pub name: String,
    /// Its executable is inside the home.
    pub runs_from_home: bool,
    /// It descends from an app this run launched into the home (see [`record_launched`]).
    pub launched_by_run: bool,
}

impl HomeHolder {
    /// Whether disposal may end it. A process is never ended merely because its command line
    /// names the home: only one that runs from the home, or that the run's own app started.
    pub fn owned_by_run(&self) -> bool {
        self.runs_from_home || self.launched_by_run
    }
}

/// One row of the process scan.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ScannedProcess {
    pid: u32,
    parent: u32,
    names_home: bool,
    runs_from_home: bool,
    name: String,
}

/// The process scan's output: `pid<TAB>parent pid<TAB>flags<TAB>name` per process, where flags
/// holds `C` (the command line names the home) and `E` (the executable is inside it).
#[cfg_attr(not(windows), allow(dead_code))]
fn parse_scan(output: &str) -> Vec<ScannedProcess> {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.trim().splitn(4, '\t');
            let pid = fields.next()?.trim().parse::<u32>().ok()?;
            let parent = fields.next()?.trim().parse::<u32>().ok()?;
            let flags = fields.next()?;
            let name = fields.next()?.trim().to_string();
            Some(ScannedProcess {
                pid,
                parent,
                names_home: flags.contains('C'),
                runs_from_home: flags.contains('E'),
                name,
            })
        })
        .collect()
}

/// Add to `launched` every scanned process whose parent is already in it, until nothing is
/// added: the apps the run launched and everything they started that is still listed. The set
/// is kept across scans, so a helper stays known after its parent has exited.
fn extend_descendants(launched: &mut std::collections::BTreeSet<u32>, scan: &[ScannedProcess]) {
    loop {
        let before = launched.len();
        for process in scan {
            if process.pid != 0 && launched.contains(&process.parent) {
                launched.insert(process.pid);
            }
        }
        if launched.len() == before {
            return;
        }
    }
}

/// The scanned processes that use the home, without this process.
fn holders_in(
    scan: &[ScannedProcess],
    launched: &std::collections::BTreeSet<u32>,
) -> Vec<HomeHolder> {
    let mut holders: Vec<HomeHolder> = scan
        .iter()
        .filter(|process| process.pid != std::process::id())
        .filter(|process| process.names_home || process.runs_from_home)
        .map(|process| HomeHolder {
            pid: process.pid,
            name: process.name.clone(),
            runs_from_home: process.runs_from_home,
            launched_by_run: launched.contains(&process.pid),
        })
        .collect();
    holders.sort_by_key(|holder| holder.pid);
    holders.dedup();
    holders
}

fn describe_holders(holders: &[HomeHolder]) -> String {
    holders
        .iter()
        .map(|holder| format!("{} (pid {})", holder.name, holder.pid))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Windows: every process, marked when its command line names `home` or its executable lives in
/// it. `None` when the scan itself failed.
#[cfg(windows)]
fn scan_processes(home: &Path) -> Option<Vec<ScannedProcess>> {
    use std::os::windows::process::CommandExt;
    use std::process::Command;
    const SCAN: &str = "$h = $env:RIGHTKIT_QA_SCAN_HOME; \
        Get-CimInstance Win32_Process | ForEach-Object { \
        $f = '-'; \
        if ($_.CommandLine -and $_.CommandLine.IndexOf($h, [StringComparison]::OrdinalIgnoreCase) -ge 0) { $f += 'C' }; \
        if ($_.ExecutablePath -and $_.ExecutablePath.StartsWith($h + [char]92, [StringComparison]::OrdinalIgnoreCase)) { $f += 'E' }; \
        [string]$_.ProcessId + [char]9 + [string]$_.ParentProcessId + [char]9 + $f + [char]9 + $_.Name }";
    let system_root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let powershell = Path::new(&system_root)
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    // Bounded: a stalled WMI query must not hang the run; no answer reads as "scan unavailable".
    let output = crate::process::output_bounded(
        Command::new(powershell)
            .args(["-NoProfile", "-NonInteractive", "-Command", SCAN])
            // The home travels in the environment, so the scanner's own command line never names it.
            .env("RIGHTKIT_QA_SCAN_HOME", home)
            .creation_flags(0x0800_0000),
        Duration::from_secs(60),
    )
    .ok()??;
    output
        .status
        .success()
        .then(|| parse_scan(&String::from_utf8_lossy(&output.stdout)))
}

/// Other systems remove a directory whatever holds its files open: nothing to scan.
#[cfg(not(windows))]
fn scan_processes(_home: &Path) -> Option<Vec<ScannedProcess>> {
    Some(Vec::new())
}

/// Windows: the processes that use `home`: their command line names it, or their executable is
/// inside it. Empty on other systems. `None` when the scan itself failed.
///
/// An app launched into a QA home starts helpers that outlive it for a moment after it is
/// stopped: WebView2 and GPU processes carry the home in `--user-data-dir=` and keep its
/// `EBWebView`, shader cache and log files open while they exit, so removing the home at once
/// fails with "Access is denied".
pub fn home_holders(home: &Path) -> Option<Vec<HomeHolder>> {
    let scan = scan_processes(home)?;
    Some(holders_in(&scan, &Default::default()))
}

/// The file in a run root that lists the pids of the apps launched into its home.
const LAUNCHED_PIDS: &str = ".launched-pids";

/// Record that this run launched process `pid` into the workspace's home. Disposal may end
/// what that process started and left behind; see [`dispose`]. Called by `control::launch`;
/// call it for an app started by hand.
pub fn record_launched(ws: &QaWorkspace, pid: u32) {
    use std::io::Write;
    if let Ok(mut file) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(ws.root.join(LAUNCHED_PIDS))
    {
        let _ = writeln!(file, "{pid}");
    }
}

fn launched_pids(ws: &QaWorkspace) -> std::collections::BTreeSet<u32> {
    fs::read_to_string(ws.root.join(LAUNCHED_PIDS))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.trim().parse().ok())
        .filter(|pid| *pid != 0 && *pid != std::process::id())
        .collect()
}

/// Wait until no process uses the home, at most `wait`. Then end the holders that belong to the
/// run (they run from the home, or descend from an app the run launched) and leave the others.
/// Returns the holders that were still there when the wait ran out.
fn settle_home_holders(ws: &QaWorkspace, wait: Duration) -> Vec<HomeHolder> {
    let deadline = std::time::Instant::now() + wait;
    let mut launched = launched_pids(ws);
    let mut holders;
    loop {
        let Some(scan) = scan_processes(&ws.home) else {
            return Vec::new();
        };
        extend_descendants(&mut launched, &scan);
        holders = holders_in(&scan, &launched);
        if holders.is_empty() || std::time::Instant::now() >= deadline {
            break;
        }
        crate::util::sleep_ms(250);
    }
    for holder in holders.iter().filter(|holder| holder.owned_by_run()) {
        crate::process::kill_tree(holder.pid);
    }
    holders
}

/// How long [`dispose`] waits for the helpers of a stopped app to leave its home.
const HOLDER_WAIT: Duration = Duration::from_secs(20);

/// Remove the workspace (or only its volatile data dir), retrying through the
/// window where an exiting app still holds files.
///
/// On Windows it first waits (up to 20 seconds) for every process that still uses the home to
/// exit. Of those that remain it ends only the run's own: a process whose executable is inside
/// the home, or that descends from an app the run launched into it ([`record_launched`]). A
/// process that merely names the home on its command line is never ended. A home that still
/// cannot be removed is reported with the processes that hold it.
pub fn dispose(
    ws: &QaWorkspace,
    keep_evidence: bool,
    attempts: u32,
    retry_ms: u64,
) -> DisposeReport {
    dispose_waiting(ws, keep_evidence, attempts, retry_ms, HOLDER_WAIT)
}

fn dispose_waiting(
    ws: &QaWorkspace,
    keep_evidence: bool,
    attempts: u32,
    retry_ms: u64,
    holder_wait: Duration,
) -> DisposeReport {
    let mut report = DisposeReport::default();
    if let Err(e) = ws.finish() {
        report.retained.push((ws.root.clone(), e.to_string()));
        return report;
    }
    let mut targets = vec![ws.home.clone()];
    if !keep_evidence {
        targets.push(ws.root.clone());
    }
    for target in targets {
        if !target.exists() {
            report.removed.push(target);
            continue;
        }
        // Deletion happens only inside the QA root this runner owns, never by a caller's path.
        if target == ws.home && !storage::is_owned_home(&target) {
            report.retained.push((
                target,
                "not a QA home created by rightkit-qa; not deleted".into(),
            ));
            continue;
        }
        if target == ws.home {
            settle_home_holders(ws, holder_wait);
        }
        let mut last = String::new();
        for attempt in 1..=attempts.max(1) {
            // The home goes through the link-safe removal, which keeps the binding file until
            // everything else is gone. The run root keeps `remove_dir_all`: it holds only files
            // this crate wrote and no app profile lives in it, so no app-made link can be there.
            // If a profile is ever placed in the run root, it must go through the same removal.
            let removal = if target == ws.home {
                match target.parent() {
                    Some(qa_root) => storage::remove_home_tree(qa_root, &target),
                    None => Err("home has no parent directory".to_string()),
                }
            } else {
                fs::remove_dir_all(&target).map_err(|e| e.to_string())
            };
            match removal {
                Ok(()) => {
                    last.clear();
                    break;
                }
                Err(e) => {
                    last = e;
                    if attempt < attempts {
                        crate::util::sleep_ms(retry_ms);
                    }
                }
            }
        }
        if last.is_empty() && !target.exists() {
            report.removed.push(target);
        } else {
            let mut why = if last.is_empty() {
                "still present".to_string()
            } else {
                last
            };
            if target == ws.home {
                // Name what holds it, so the report says which program to look at.
                match home_holders(&target) {
                    Some(holders) if !holders.is_empty() => {
                        why.push_str(&format!("; held by {}", describe_holders(&holders)));
                    }
                    Some(_) if cfg!(windows) => {
                        why.push_str("; no running process names this home");
                    }
                    _ => {}
                }
            }
            report.retained.push((target.clone(), why));
            if target == ws.home {
                if !keep_evidence {
                    report
                        .retained
                        .push((ws.root.clone(), "app temp cleanup pending".into()));
                }
                break;
            }
        }
    }
    report
}

/// Sweep run directories left by crashed runs. Bounded to `<root>/runs`, to
/// entries older than `older_than`, and never touches `keep`.
pub fn prune_abandoned(cache_root: &Path, older_than: Duration, keep: &[String]) -> DisposeReport {
    let mut report = DisposeReport::default();
    let runs_root = cache_root.join("runs");
    let Ok(entries) = fs::read_dir(&runs_root) else {
        return report;
    };
    let entries: Vec<_> = entries.flatten().collect();
    for entry in &entries {
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() && !ft.is_symlink() {
            let app = entry.file_name().to_string_lossy().to_string();
            if let Ok(r) = storage::prune_with_age_override(cache_root, &app, keep, older_than) {
                report.removed.extend(r.removed);
                report.retained.extend(r.retained);
            }
        }
    }
    report
}

//! An isolated, reliably removable home for one QA run: own data dir (so the app
//! takes its own single-instance lock), own evidence dir, retrying disposal that
//! reports what it could not remove, and a sweep for crashed runs.
use crate::storage;
use crate::util::{new_id, require_text, Result};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct QaWorkspace {
    pub run_id: String,
    /// Managed run root: `<managed>/runs/<app>/<run-id>`.
    pub root: PathBuf,
    /// Owned app home on internal system temp storage.
    pub home: PathBuf,
    pub data_dir: PathBuf,
    pub evidence_dir: PathBuf,
    pub capture_dir: PathBuf,
    /// Environment that points an app at this workspace (profile isolation).
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
/// one run under `rightkit cargo`.
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
    let home = storage::create_app_root(&mut managed)?;
    let run_id = managed.marker.run_id.clone();
    let root = managed.root;
    // Generic managed storage creates this directory; QA profiles belong only in home.
    fs::remove_dir(root.join("data"))?;
    let data_dir = home.join("data");
    let evidence_dir = root.join("evidence");
    let capture_dir = root.join("captures");
    let tmp_dir = home.join("tmp");
    let cache_dir = home.join("cache");
    fs::create_dir_all(&data_dir)?;
    fs::create_dir_all(&evidence_dir)?;
    fs::create_dir_all(&capture_dir)?;
    fs::create_dir_all(&tmp_dir)?;
    fs::create_dir_all(&cache_dir)?;
    fs::create_dir_all(home.join("evidence"))?;
    fs::create_dir_all(home.join("captures"))?;
    create_profile_dirs(&home)?;
    let mut env = BTreeMap::new();
    env.insert("RIGHTKIT_QA_NATIVE".into(), "1".into());
    env.insert("RIGHTKIT_QA_RUN_ID".into(), run_id.clone());
    env.insert("RIGHTKIT_RUN_ROOT".into(), home.to_string_lossy().into());
    env.insert(
        "RIGHTKIT_SUITE_ROOT".into(),
        home.join("suite").to_string_lossy().into(),
    );
    env.insert("HOME".into(), home.to_string_lossy().into());
    env.insert("USERPROFILE".into(), home.to_string_lossy().into());
    env.insert("APPDATA".into(), data_dir.to_string_lossy().into());
    env.insert("LOCALAPPDATA".into(), data_dir.to_string_lossy().into());
    env.insert("XDG_CACHE_HOME".into(), cache_dir.to_string_lossy().into());
    env.insert(
        "RIGHTKIT_QA_DATA_DIR".into(),
        data_dir.to_string_lossy().into(),
    );
    env.insert(
        "RIGHTKIT_QA_EVIDENCE_DIR".into(),
        home.join("evidence").to_string_lossy().into(),
    );
    env.insert(
        "RIGHTKIT_QA_CAPTURE_DIR".into(),
        home.join("captures").to_string_lossy().into(),
    );
    env.insert("TMPDIR".into(), tmp_dir.to_string_lossy().into());
    env.insert("TMP".into(), tmp_dir.to_string_lossy().into());
    env.insert("TEMP".into(), tmp_dir.to_string_lossy().into());
    env.insert("RIGHTKIT_QA_MUTE".into(), "1".into());
    env.insert("XDG_DATA_HOME".into(), data_dir.to_string_lossy().into());
    env.insert("XDG_CONFIG_HOME".into(), data_dir.to_string_lossy().into());
    env.insert(
        "WEBVIEW2_USER_DATA_FOLDER".into(),
        data_dir.join("webview2").to_string_lossy().into(),
    );
    let key: String = app_key
        .to_uppercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    env.insert(format!("{key}_DATA_DIR"), data_dir.to_string_lossy().into());
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
        data_dir,
        evidence_dir,
        capture_dir,
        env,
    })
}

/// Create the per-user directories that platform known-folder lookups resolve
/// under the isolated profile. On Windows `SHGetKnownFolderPath` expands the
/// registry's `%USERPROFILE%\AppData\Local` against the overridden
/// USERPROFILE and fails (Tauri "unknown path", `dirs::cache_dir` None) when the
/// result is absent, even though LOCALAPPDATA itself points elsewhere.
fn create_profile_dirs(home: &Path) -> Result<()> {
    let relative: &[&str] = if cfg!(windows) {
        &[
            "AppData/Local",
            "AppData/Local/Temp",
            "AppData/LocalLow",
            "AppData/Roaming",
            "Desktop",
            "Documents",
            "Downloads",
            "Music",
            "Pictures",
            "Videos",
        ]
    } else if cfg!(target_os = "macos") {
        &[
            "Library/Application Support",
            "Library/Caches",
            "Library/Preferences",
            "Library/Logs",
            "Desktop",
            "Documents",
            "Downloads",
        ]
    } else {
        &[
            ".local/share",
            ".local/state",
            ".config",
            ".cache",
            "Desktop",
            "Documents",
            "Downloads",
        ]
    };
    for dir in relative {
        fs::create_dir_all(home.join(dir))?;
    }
    Ok(())
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
    /// A fresh per-scenario directory inside the isolated data dir.
    pub fn scenario_dir(&self, name: &str) -> Result<PathBuf> {
        let safe: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let dir = self.data_dir.join(format!("{safe}-{}", &new_id()[..8]));
        fs::create_dir_all(&dir)?;
        Ok(dir)
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
    use std::process::{Command, Stdio};
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
    let output = Command::new(powershell)
        .args(["-NoProfile", "-NonInteractive", "-Command", SCAN])
        // The home travels in the environment, so the scanner's own command line never names it.
        .env("RIGHTKIT_QA_SCAN_HOME", home)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(0x0800_0000)
        .output()
        .ok()?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolates_and_disposes() {
        let root = storage::app_temp_dir().join(format!("rkqa-ws-{}", new_id()));
        let ws = create(&root, Some("run1"), "genright").unwrap();
        assert!(ws.env.contains_key("GENRIGHT_DATA_DIR"));
        assert!(ws.data_dir.exists());
        assert!(!ws.root.join("data").exists());
        // The home is created under the canonical temp directory, which on Windows differs in
        // form from the one the environment names (short names, no extended-length prefix).
        let temp = fs::canonicalize(storage::app_temp_dir()).unwrap();
        assert!(ws.home.starts_with(temp.join("rightkit-qa")));
        assert!(!ws.home.starts_with(&ws.root));
        assert!(ws.evidence_dir.starts_with(&ws.root));
        assert!(ws.capture_dir.starts_with(&ws.root));
        for key in [
            "RIGHTKIT_RUN_ROOT",
            "RIGHTKIT_SUITE_ROOT",
            "HOME",
            "USERPROFILE",
            "APPDATA",
            "LOCALAPPDATA",
            "XDG_CACHE_HOME",
            "XDG_DATA_HOME",
            "XDG_CONFIG_HOME",
            "WEBVIEW2_USER_DATA_FOLDER",
            "GENRIGHT_DATA_DIR",
            "RIGHTKIT_QA_DATA_DIR",
            "RIGHTKIT_QA_EVIDENCE_DIR",
            "RIGHTKIT_QA_CAPTURE_DIR",
            "TMPDIR",
            "TMP",
            "TEMP",
        ] {
            assert!(Path::new(&ws.env[key]).starts_with(&ws.home), "{key}");
        }
        for (key, value) in &ws.env {
            let path = Path::new(value);
            if path.starts_with(&ws.home) {
                assert!(path.is_dir(), "{key} -> {value} must exist as a directory");
            }
        }
        let known: &[&str] = if cfg!(windows) {
            &["AppData/Local", "AppData/Roaming", "AppData/LocalLow"]
        } else if cfg!(target_os = "macos") {
            &["Library/Application Support", "Library/Caches"]
        } else {
            &[".local/share", ".config", ".cache"]
        };
        for dir in known {
            assert!(ws.home.join(dir).is_dir(), "{dir}");
        }
        fs::write(ws.home.join("app-launch.log"), "live log").unwrap();
        fs::write(ws.home.join("evidence/from-app.json"), "app evidence").unwrap();
        fs::write(ws.home.join("captures/frame.png"), "frame").unwrap();
        fs::write(ws.data_dir.join("result.txt"), "result").unwrap();
        let artifact = ws
            .preserve_artifact(&ws.data_dir.join("result.txt"))
            .unwrap();
        assert_eq!(artifact, ws.evidence_dir.join("app/data/result.txt"));
        assert!(create(&root, Some("../x"), "a").is_err());
        let r = dispose(&ws, true, 2, 1);
        assert!(
            r.retained.is_empty()
                && !ws.data_dir.exists()
                && ws.root.exists()
                && ws.evidence_dir.exists()
        );
        assert!(!ws.home.exists());
        assert_eq!(fs::read_to_string(artifact).unwrap(), "result");
        assert_eq!(
            fs::read_to_string(ws.evidence_dir.join("app-launch.log")).unwrap(),
            "live log"
        );
        assert_eq!(
            fs::read_to_string(ws.evidence_dir.join("app/from-app.json")).unwrap(),
            "app evidence"
        );
        assert_eq!(
            fs::read_to_string(ws.capture_dir.join("frame.png")).unwrap(),
            "frame"
        );
        let r = dispose(&ws, false, 2, 1);
        assert!(r.retained.is_empty() && !ws.root.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn orphan_sweep_removes_registered_temp_home_but_spares_active_runs() {
        // Canonical from the start, so the paths the sweep reports and the workspace's own
        // root have the same form on Windows.
        let root = fs::canonicalize(storage::app_temp_dir())
            .unwrap()
            .join(format!("rkqa-orphan-{}", new_id()));
        let ws = create(&root, Some("orphan"), "app").unwrap();
        assert!(prune_abandoned(&root, Duration::ZERO, &[])
            .removed
            .is_empty());
        assert!(ws.home.exists());
        ws.finish().unwrap();
        let report = prune_abandoned(&root, Duration::ZERO, &[]);
        assert!(report.retained.is_empty());
        assert_eq!(report.removed, vec![ws.root]);
        assert!(!ws.home.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn prune_spares_young_and_kept_runs() {
        let root = storage::app_temp_dir().join(format!("rkqa-prune-{}", new_id()));
        let a = storage::create(Some(&root), "x", Some("a"), Duration::from_secs(3600)).unwrap();
        let _b = storage::create(Some(&root), "x", Some("b"), Duration::from_secs(3600)).unwrap();
        storage::finish(&a).unwrap();
        let r = prune_abandoned(&root, Duration::from_secs(3600), &["b".into()]);
        assert!(r.removed.is_empty());
        let r = prune_abandoned(&root, Duration::ZERO, &["b".into()]);
        assert_eq!(r.removed.len(), 1);
        assert!(root.join("runs/x/b").exists() && !root.join("runs/x/a").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn dispose_deletes_only_a_home_the_runner_created() {
        let root = storage::app_temp_dir().join(format!("rkqa-ws-own-{}", new_id()));
        let mut ws = create(&root, Some("own1"), "ownapp").unwrap();
        // A caller-supplied path that is not a QA home is never deleted.
        let foreign = root.join("precious");
        fs::create_dir_all(&foreign).unwrap();
        fs::write(foreign.join("keep"), b"keep").unwrap();
        let real_home = ws.home.clone();
        ws.home = foreign.clone();
        let report = dispose(&ws, true, 1, 0);
        assert!(foreign.join("keep").exists(), "{report:?}");
        assert!(report.retained.iter().any(|(p, _)| p == &foreign));
        // The genuine home is removed.
        ws.home = real_home.clone();
        let report = dispose(&ws, false, 1, 0);
        assert!(report.retained.is_empty(), "{report:?}");
        assert!(!real_home.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn dispose_removes_a_home_that_holds_the_wininet_junction_and_a_link_out() {
        // Gap ledger 216: every home of an app that used WinINet stayed behind.
        let root = storage::app_temp_dir().join(format!("rkqa-ws-links-{}", new_id()));
        let ws = create(&root, Some("links1"), "linkapp").unwrap();
        let models = root.join("models-cache");
        fs::create_dir_all(&models).unwrap();
        fs::write(models.join("model.bin"), b"model bytes").unwrap();
        storage::test_links::wininet_cache(&ws.home);
        fs::create_dir_all(ws.home.join("data").join("shared")).unwrap();
        storage::test_links::junction(&ws.home.join("data").join("shared").join("models"), &models);
        let report = dispose(&ws, true, 1, 0);
        assert!(report.retained.is_empty(), "{report:?}");
        assert!(!ws.home.exists());
        assert_eq!(fs::read(models.join("model.bin")).unwrap(), b"model bytes");
        let _ = fs::remove_dir_all(root);
    }

    fn scanned(pid: u32, parent: u32, flags: &str, name: &str) -> ScannedProcess {
        ScannedProcess {
            pid,
            parent,
            names_home: flags.contains('C'),
            runs_from_home: flags.contains('E'),
            name: name.into(),
        }
    }

    #[test]
    fn the_process_scan_is_read_row_by_row() {
        let scan = parse_scan(
            "  412\t98\t-C\tmsedgewebview2.exe\r\n98\t4\t-\tCutRight.exe\n\nnot a row\n7\t1\t-CE\ttool.exe\n",
        );
        assert_eq!(
            scan,
            vec![
                scanned(412, 98, "C", "msedgewebview2.exe"),
                scanned(98, 4, "", "CutRight.exe"),
                scanned(7, 1, "CE", "tool.exe"),
            ]
        );
        assert!(parse_scan("").is_empty());
    }

    #[test]
    fn only_the_runs_own_processes_may_be_ended_and_every_holder_is_named() {
        let me = std::process::id();
        let scan = vec![
            // The launched app (already recorded), its WebView2 browser and that one's GPU child.
            scanned(98, 4, "", "CutRight.exe"),
            scanned(412, 98, "C", "msedgewebview2.exe"),
            scanned(413, 412, "C", "msedgewebview2.exe"),
            // A tool that runs from inside the home.
            scanned(7, 1, "CE", "tool.exe"),
            // Someone's editor or shell that merely names the home: a holder, never ended.
            scanned(900, 1, "C", "notepad.exe"),
            // Unrelated, and this process itself even if its command line named the home.
            scanned(5000, 1, "", "explorer.exe"),
            scanned(me, 1, "C", "rightkit-qa.exe"),
        ];
        let mut launched = std::collections::BTreeSet::from([98]);
        extend_descendants(&mut launched, &scan);
        assert_eq!(launched, std::collections::BTreeSet::from([98, 412, 413]));

        let holders = holders_in(&scan, &launched);
        assert_eq!(
            holders
                .iter()
                .map(|h| (h.pid, h.owned_by_run()))
                .collect::<Vec<_>>(),
            vec![(7, true), (412, true), (413, true), (900, false)]
        );
        assert_eq!(
            describe_holders(&holders),
            "tool.exe (pid 7), msedgewebview2.exe (pid 412), msedgewebview2.exe (pid 413), notepad.exe (pid 900)"
        );

        // A later scan, after the browser process has exited: its child is still known.
        let later = vec![scanned(413, 412, "C", "msedgewebview2.exe")];
        extend_descendants(&mut launched, &later);
        assert!(holders_in(&later, &launched)[0].launched_by_run);
        // Without a recorded launch nothing that only names the home may be ended.
        assert!(!holders_in(&later, &Default::default())[0].owned_by_run());
    }

    #[test]
    fn launched_pids_are_recorded_in_the_run_root() {
        let root = storage::app_temp_dir().join(format!("rkqa-ws-pids-{}", new_id()));
        let ws = create(&root, Some("pids1"), "pidsapp").unwrap();
        assert!(launched_pids(&ws).is_empty());
        record_launched(&ws, 4242);
        record_launched(&ws, 4343);
        record_launched(&ws, std::process::id());
        assert_eq!(
            launched_pids(&ws),
            std::collections::BTreeSet::from([4242, 4343])
        );
        let report = dispose(&ws, false, 2, 1);
        assert!(report.retained.is_empty(), "{report:?}");
        let _ = fs::remove_dir_all(root);
    }

    /// Ledger 174. A helper that the launched app started outlives it, sits in the home (its
    /// working directory) and names it on its command line, as WebView2 does. Removing the home
    /// at once fails; disposal finds the helper, waits, ends it and then removes the home.
    #[cfg(windows)]
    #[test]
    fn dispose_waits_for_a_helper_the_app_left_in_the_home_then_removes_it() {
        use std::os::windows::process::CommandExt;
        let root = storage::app_temp_dir().join(format!("rkqa-ws-held-{}", new_id()));
        let ws = create(&root, Some("held1"), "heldapp").unwrap();
        // The "app": a shell the run launched. Its child keeps running in the home.
        let mut app = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "ping -n 120 127.0.0.1 >nul & rem"])
            .arg(format!("--user-data-dir={}", ws.home.display()))
            .current_dir(&ws.home)
            .creation_flags(0x0800_0000)
            .spawn()
            .unwrap();
        record_launched(&ws, app.id());

        // The scan names it, and it is not this process.
        let holders = home_holders(&ws.home).expect("the process scan runs");
        assert!(
            holders.iter().any(|holder| holder.pid == app.id()),
            "{holders:?}"
        );
        assert!(holders
            .iter()
            .all(|holder| holder.pid != std::process::id()));

        let report = dispose_waiting(&ws, false, 20, 250, Duration::from_secs(2));
        assert!(report.retained.is_empty(), "{report:?}");
        assert!(!ws.home.exists());
        let _ = app.kill();
        let _ = app.wait();
        let _ = fs::remove_dir_all(root);
    }

    /// A process that only names the home on its command line, and that the run did not launch,
    /// is left running and named in the report.
    #[cfg(windows)]
    #[test]
    fn dispose_leaves_a_foreign_process_that_names_the_home_and_reports_it() {
        use std::os::windows::process::CommandExt;
        let root = storage::app_temp_dir().join(format!("rkqa-ws-foreign-{}", new_id()));
        let ws = create(&root, Some("foreign1"), "foreignapp").unwrap();
        let mut foreign = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "ping -n 120 127.0.0.1 >nul & rem"])
            .arg(format!("--watch={}", ws.home.display()))
            .current_dir(&ws.home)
            .creation_flags(0x0800_0000)
            .spawn()
            .unwrap();

        let report = dispose_waiting(&ws, false, 2, 50, Duration::from_millis(500));
        assert!(matches!(foreign.try_wait(), Ok(None)), "it must still run");
        let (_, why) = report
            .retained
            .iter()
            .find(|(path, _)| path == &ws.home)
            .unwrap_or_else(|| panic!("the home is held: {report:?}"));
        assert!(
            why.contains(&format!("held by cmd.exe (pid {})", foreign.id())),
            "{why}"
        );

        crate::process::kill_tree(foreign.id());
        let _ = foreign.wait();
        // With the holder gone the home can be removed.
        crate::util::sleep_ms(300);
        assert!(fs::remove_dir_all(&ws.home).is_ok());
        let _ = fs::remove_dir_all(root);
    }
}

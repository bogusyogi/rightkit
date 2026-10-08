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

pub(crate) fn prepare_app_root(root: &Path, app: &str) -> Result<PathBuf> {
    let root = managed_root(Some(root));
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
    let root = managed_root(root);
    let app = safe_key(app, "app")?;
    let id = match run_id {
        Some(id) => safe_key(id, "run id")?,
        None => new_id(),
    };
    let app_root = root.join("runs").join(&app);
    with_lock(&app_root, || {
        ensure_dir(&root)?;
        ensure_dir(&root.join("runs"))?;
        ensure_dir(&app_root)?;
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
        if report.over_cap_bytes > 0 {
            return err(format!(
                "QA retention cap is blocked by {} bytes of protected or unknown data",
                report.over_cap_bytes
            ));
        }
        let run = run_root(&root, &app, &id)?;
        if run.exists() {
            return err(format!("QA run already exists: {}", run.display()));
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
    })
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

pub(crate) fn create_app_root(run: &mut ManagedRun) -> Result<PathBuf> {
    let parent = app_temp_dir().join("rightkit-qa");
    ensure_dir(&parent)?;
    let parent = fs::canonicalize(parent)?;
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
    write_marker(&run.root, &run.marker)?;
    Ok(home)
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
    with_lock(&app_root, || {
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
    })
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

fn with_lock<T>(app_root: &Path, body: impl FnOnce() -> Result<T>) -> Result<T> {
    ensure_dir(app_root)?;
    let lock = app_root.join(".retention-lock");
    let owner = lock.join("owner.json");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match fs::create_dir(&lock) {
            Ok(()) => {
                let mut file = File::create(&owner)?;
                write!(
                    file,
                    "{{\"pid\":{},\"createdAt\":{}}}",
                    std::process::id(),
                    now_ms()
                )?;
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let pid = fs::read_to_string(&owner)
                    .ok()
                    .and_then(|x| serde_json::from_str::<serde_json::Value>(&x).ok())
                    .and_then(|x| x.get("pid").and_then(|v| v.as_u64()))
                    .and_then(|x| u32::try_from(x).ok());
                if let Some(pid) = pid.filter(|p| *p > 0) {
                    if !pid_live(pid) {
                        let _ = fs::remove_dir_all(&lock);
                        continue;
                    }
                }
                if std::time::Instant::now() >= deadline {
                    return err(format!("QA retention lock is held: {}", lock.display()));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(e.into()),
        }
    }
    let result = body();
    let _ = fs::remove_dir_all(lock);
    result
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
    if pid == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        let result = unsafe { libc::kill(pid as i32, 0) };
        result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let output = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}")])
            .creation_flags(0x0800_0000)
            .output();
        match output {
            Ok(x) if x.status.success() => {
                let wanted = pid.to_string();
                !String::from_utf8_lossy(&x.stdout)
                    .lines()
                    .skip(1)
                    .all(|line| line.split_whitespace().next() != Some(wanted.as_str()))
            }
            _ => true,
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
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
}

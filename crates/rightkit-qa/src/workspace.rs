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

pub fn create(cache_root: &Path, run_id: Option<&str>, app_key: &str) -> Result<QaWorkspace> {
    create_impl(Some(cache_root), run_id, app_key)
}

/// Create under RIGHTKIT_MANAGED_ROOT (or its platform default).
pub fn create_managed(run_id: Option<&str>, app_key: &str) -> Result<QaWorkspace> {
    create_impl(None, run_id, app_key)
}

pub(crate) fn create_impl_for_harness(
    managed_root: Option<&Path>,
    app_key: &str,
) -> Result<QaWorkspace> {
    create_impl(managed_root, None, app_key)
}

fn create_impl(
    cache_root: Option<&Path>,
    run_id: Option<&str>,
    app_key: &str,
) -> Result<QaWorkspace> {
    let app_key = require_text(app_key, "app")?;
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

/// Remove the workspace (or only its volatile data dir), retrying through the
/// window where an exiting app still holds files.
pub fn dispose(
    ws: &QaWorkspace,
    keep_evidence: bool,
    attempts: u32,
    retry_ms: u64,
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
        let mut last = String::new();
        for attempt in 1..=attempts.max(1) {
            match fs::remove_dir_all(&target) {
                Ok(()) => {
                    last.clear();
                    break;
                }
                Err(e) => {
                    last = e.to_string();
                    if attempt < attempts {
                        crate::util::sleep_ms(retry_ms);
                    }
                }
            }
        }
        if last.is_empty() && !target.exists() {
            report.removed.push(target);
        } else {
            report.retained.push((
                target.clone(),
                if last.is_empty() {
                    "still present".into()
                } else {
                    last
                },
            ));
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
}

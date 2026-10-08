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
    /// Run-local app home.
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
    let managed = storage::create(cache_root, &app_key, run_id, Duration::from_secs(24 * 3600))?;
    let run_id = managed.marker.run_id.clone();
    let root = managed.root;
    let home = root.clone();
    let data_dir = root.join("data");
    let evidence_dir = root.join("evidence");
    let capture_dir = root.join("captures");
    let tmp_dir = root.join("tmp");
    fs::create_dir_all(&data_dir)?;
    fs::create_dir_all(&evidence_dir)?;
    fs::create_dir_all(&capture_dir)?;
    fs::create_dir_all(&tmp_dir)?;
    let mut env = BTreeMap::new();
    env.insert("RIGHTKIT_QA_NATIVE".into(), "1".into());
    env.insert("RIGHTKIT_QA_RUN_ID".into(), run_id.clone());
    env.insert("RIGHTKIT_RUN_ROOT".into(), root.to_string_lossy().into());
    env.insert(
        "RIGHTKIT_QA_DATA_DIR".into(),
        data_dir.to_string_lossy().into(),
    );
    env.insert(
        "RIGHTKIT_QA_EVIDENCE_DIR".into(),
        evidence_dir.to_string_lossy().into(),
    );
    env.insert(
        "RIGHTKIT_QA_CAPTURE_DIR".into(),
        capture_dir.to_string_lossy().into(),
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

impl QaWorkspace {
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
    if let Ok(marker_bytes) = fs::read(ws.root.join(storage::RUN_MARKER)) {
        if let Ok(marker) = serde_json::from_slice(&marker_bytes) {
            let _ = storage::finish(&storage::ManagedRun {
                root: ws.root.clone(),
                marker,
            });
        }
    }
    let mut targets = vec![ws.data_dir.clone()];
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
                target,
                if last.is_empty() {
                    "still present".into()
                } else {
                    last
                },
            ));
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
        let root = std::env::temp_dir().join(format!("rkqa-ws-{}", new_id()));
        let ws = create(&root, Some("run1"), "genright").unwrap();
        assert!(ws.env.contains_key("GENRIGHT_DATA_DIR"));
        assert!(ws.data_dir.exists());
        assert!(create(&root, Some("../x"), "a").is_err());
        let r = dispose(&ws, true, 2, 1);
        assert!(
            r.retained.is_empty()
                && !ws.data_dir.exists()
                && ws.root.exists()
                && ws.evidence_dir.exists()
        );
        let r = dispose(&ws, false, 2, 1);
        assert!(r.retained.is_empty() && !ws.root.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn prune_spares_young_and_kept_runs() {
        let root = std::env::temp_dir().join(format!("rkqa-prune-{}", new_id()));
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

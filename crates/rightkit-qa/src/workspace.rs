//! An isolated, reliably removable home for one QA run: own data dir (so the app
//! takes its own single-instance lock), own evidence dir, retrying disposal that
//! reports what it could not remove, and a sweep for crashed runs.
use crate::util::{err, new_id, require_text, Result};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

#[derive(Debug, Clone)]
pub struct QaWorkspace {
    pub run_id: String,
    /// Evidence side: lives under the cache root next to the app.
    pub root: PathBuf,
    /// App side: under the system temp dir. Apps started by launchd on macOS cannot
    /// write to external volumes, so everything the app touches (data dir, port file,
    /// live log) lives here and evidence is copied back.
    pub home: PathBuf,
    pub data_dir: PathBuf,
    pub evidence_dir: PathBuf,
    /// Environment that points an app at this workspace (profile isolation).
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Default, Clone)]
pub struct DisposeReport {
    pub removed: Vec<PathBuf>,
    pub retained: Vec<(PathBuf, String)>,
}

pub fn create(cache_root: &Path, run_id: Option<&str>, app_key: &str) -> Result<QaWorkspace> {
    let app_key = require_text(app_key, "app")?;
    let run_id = run_id.map(str::to_string).unwrap_or_else(new_id);
    if run_id.is_empty()
        || !run_id.chars().next().unwrap().is_ascii_alphanumeric()
        || !run_id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return err(format!("unsafe QA run id: {run_id}"));
    }
    let root = cache_root.join("runs").join(&run_id);
    let home = std::env::temp_dir().join("rightkit-qa").join(format!("{}-{run_id}", app_key));
    let data_dir = home.join("data");
    let evidence_dir = root.join("evidence");
    fs::create_dir_all(&data_dir)?;
    fs::create_dir_all(&evidence_dir)?;
    let mut env = BTreeMap::new();
    env.insert("RIGHTKIT_QA_NATIVE".into(), "1".into());
    env.insert("RIGHTKIT_QA_RUN_ID".into(), run_id.clone());
    env.insert("RIGHTKIT_QA_DATA_DIR".into(), data_dir.to_string_lossy().into());
    env.insert("RIGHTKIT_QA_EVIDENCE_DIR".into(), evidence_dir.to_string_lossy().into());
    env.insert("RIGHTKIT_QA_MUTE".into(), "1".into());
    env.insert("XDG_DATA_HOME".into(), data_dir.to_string_lossy().into());
    env.insert("XDG_CONFIG_HOME".into(), data_dir.to_string_lossy().into());
    env.insert("WEBVIEW2_USER_DATA_FOLDER".into(), data_dir.join("webview2").to_string_lossy().into());
    let key: String = app_key.to_uppercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    env.insert(format!("{key}_DATA_DIR"), data_dir.to_string_lossy().into());
    Ok(QaWorkspace { run_id, root, home, data_dir, evidence_dir, env })
}

impl QaWorkspace {
    /// A fresh per-scenario directory inside the isolated data dir.
    pub fn scenario_dir(&self, name: &str) -> Result<PathBuf> {
        let safe: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
        let dir = self.data_dir.join(format!("{safe}-{}", &new_id()[..8]));
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }
}

/// Remove the workspace (or only its volatile data dir), retrying through the
/// window where an exiting app still holds files.
pub fn dispose(ws: &QaWorkspace, keep_evidence: bool, attempts: u32, retry_ms: u64) -> DisposeReport {
    let mut report = DisposeReport::default();
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
            report.retained.push((target, if last.is_empty() { "still present".into() } else { last }));
        }
    }
    report
}

/// Sweep run directories left by crashed runs. Bounded to `<root>/runs`, to
/// entries older than `older_than`, and never touches `keep`.
pub fn prune_abandoned(cache_root: &Path, older_than: Duration, keep: &[String]) -> DisposeReport {
    let mut report = DisposeReport::default();
    let Ok(entries) = fs::read_dir(cache_root.join("runs")) else { return report };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Ok(ft) = entry.file_type() else { continue };
        if !ft.is_dir() || ft.is_symlink() || keep.contains(&name) {
            continue;
        }
        let path = entry.path();
        let age = fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|t| SystemTime::now().duration_since(t).ok());
        match age {
            Some(a) if a >= older_than => match fs::remove_dir_all(&path) {
                Ok(()) => report.removed.push(path),
                Err(e) => report.retained.push((path, e.to_string())),
            },
            Some(a) => report.retained.push((path, format!("only {}s old; may belong to a live run", a.as_secs()))),
            None => continue,
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
        assert!(r.retained.is_empty() && !ws.data_dir.exists() && !ws.home.exists() && ws.evidence_dir.exists());
        let r = dispose(&ws, false, 2, 1);
        assert!(r.retained.is_empty() && !ws.root.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn prune_spares_young_and_kept_runs() {
        let root = std::env::temp_dir().join(format!("rkqa-prune-{}", new_id()));
        create(&root, Some("a"), "x").unwrap();
        create(&root, Some("b"), "x").unwrap();
        let r = prune_abandoned(&root, Duration::from_secs(3600), &["b".into()]);
        assert!(r.removed.is_empty());
        let r = prune_abandoned(&root, Duration::ZERO, &["b".into()]);
        assert_eq!(r.removed.len(), 1);
        assert!(root.join("runs/b").exists() && !root.join("runs/a").exists());
        let _ = fs::remove_dir_all(root);
    }
}

/// Sweep temp-side homes (`<temp>/rightkit-qa/<app>-<run>`) of crashed runs for one app.
pub fn prune_abandoned_homes(app_key: &str, older_than: Duration, keep_run_ids: &[String]) -> DisposeReport {
    let mut report = DisposeReport::default();
    let Ok(rd) = fs::read_dir(std::env::temp_dir().join("rightkit-qa")) else { return report };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !name.starts_with(&format!("{app_key}-")) || keep_run_ids.iter().any(|k| name.ends_with(k.as_str())) {
            continue;
        }
        let age = fs::metadata(e.path()).and_then(|m| m.modified()).ok().and_then(|t| SystemTime::now().duration_since(t).ok());
        if age.map(|a| a >= older_than).unwrap_or(false) {
            match fs::remove_dir_all(e.path()) {
                Ok(()) => report.removed.push(e.path()),
                Err(err) => report.retained.push((e.path(), err.to_string())),
            }
        }
    }
    report
}

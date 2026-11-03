//! Evidence retention for finished QA runs.
//!
//! Every run writes into its own directory `<managed root>/runs/<app>/<run-id>/` and carries a
//! marker, `.rightkit-run.json` ([`crate::storage::RunMarker`]: run id, app, kind, start and end
//! time, pass or fail). [`prune_runs`] decides from that marker alone:
//!
//! * a directory without a valid marker is never touched, and neither is one whose marker names
//!   another app or run id than its path;
//! * a run that has not finished (no recorded outcome) or is still active is kept;
//! * every failed run is kept;
//! * of the passing runs, the newest per app and kind is kept and the others are deleted;
//! * a run in which nothing executed (outcome `skipped`) holds no evidence and is deleted;
//! * a run whose owner asked to keep its data (the marker's `keep_until`, written by [`mark_keep_data`], at most [`KEEP_DATA_MAX`])
//!   is kept whatever its outcome, and does not count as the newest passing run.
//!
//! The pass runs right after each job ends ([`prune_runs_after_job`], called by the suite and by
//! the Rust harness) and is also the CLI `rightkit-qa prune-runs`, for a schedule.
//!
//! Containment: nothing outside `<managed root>/runs` is read for deletion or removed. A `runs`
//! directory, an app directory or a run directory that is a link (symlink, or junction on Windows)
//! is never entered; a link inside a run is removed as itself and what it points at stays.
use crate::storage::{
    directory_bytes, is_inside, marker_active, read_marker, remove_app_root, remove_children,
    safe_dir, safe_key, try_lock, RunKind, RunMarker, RunOutcome, RUN_MARKER,
};
use crate::util::{err, now_ms, Result};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long a pass waits for another pass over the same app before it skips that app.
const LOCK_WAIT: Duration = Duration::from_secs(10);

// The keep-data request is defined once, in `storage`, because the count/age prune there honours
// it too.
pub use crate::storage::{mark_keep_data, release_keep_data, run_keeps_data, KEEP_DATA_MAX};

/// What one retention pass did. With `dry_run` the `removed` entries are what would go.
#[derive(Debug, Default, Clone)]
pub struct RunsRetentionReport {
    /// (run directory, bytes it held, why).
    pub removed: Vec<(PathBuf, u64, String)>,
    /// (path, why): runs and directories the pass looked at and left alone.
    pub kept: Vec<(PathBuf, String)>,
    pub removed_bytes: u64,
    pub dry_run: bool,
}

impl RunsRetentionReport {
    /// One line for a log or the CLI.
    pub fn summary(&self) -> String {
        format!(
            "{} {} run dir(s) ({} bytes), kept {}",
            if self.dry_run {
                "would remove"
            } else {
                "removed"
            },
            self.removed.len(),
            self.removed_bytes,
            self.kept.len()
        )
    }
}

/// The managed root a run directory sits under (`<root>/runs/<app>/<run-id>`), if the path has
/// that shape.
pub fn managed_root_of_run(run_root: &Path) -> Option<PathBuf> {
    let app_dir = run_root.parent()?;
    let runs = app_dir.parent()?;
    if runs.file_name()? != "runs" {
        return None;
    }
    Some(runs.parent()?.to_path_buf())
}

/// Prune finished runs under `<managed_root>/runs`, for one `app` or, with `None`, for every app.
/// See the module documentation for the rule. `dry_run` reports and removes nothing.
///
/// A missing managed root or `runs` directory is not an error (nothing to prune). A `runs`
/// directory that is a link, or that does not resolve to `<managed root>/runs`, is refused.
pub fn prune_runs(
    managed_root: &Path,
    app: Option<&str>,
    dry_run: bool,
) -> Result<RunsRetentionReport> {
    let mut report = RunsRetentionReport {
        dry_run,
        ..Default::default()
    };
    let app = app.map(|a| safe_key(a, "app")).transpose()?;
    let Ok(root) = fs::canonicalize(managed_root) else {
        return Ok(report);
    };
    let runs = root.join("runs");
    match fs::symlink_metadata(&runs) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(report),
        Err(e) => return Err(e.into()),
        Ok(m) if m.file_type().is_symlink() || !m.is_dir() => {
            return err(format!(
                "refusing QA runs root that is not a plain directory: {}",
                runs.display()
            ));
        }
        Ok(_) => {}
    }
    if fs::canonicalize(&runs)? != runs {
        return err(format!(
            "refusing QA runs root that resolves outside the managed root: {}",
            runs.display()
        ));
    }
    let apps: Vec<String> = match app {
        Some(a) => vec![a],
        None => {
            let mut found = vec![];
            for entry in fs::read_dir(&runs)?.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let Ok(ft) = entry.file_type() else { continue };
                if name.starts_with('.') || (!ft.is_dir() && !ft.is_symlink()) {
                    continue;
                }
                if safe_key(&name, "app").is_err() {
                    report
                        .kept
                        .push((entry.path(), "not a valid app directory name".into()));
                    continue;
                }
                found.push(name);
            }
            found.sort();
            found
        }
    };
    for name in apps {
        prune_app(&runs, &name, dry_run, &mut report)?;
    }
    Ok(report)
}

/// The pass that runs right after a job ends. It never fails the job: an error is returned inside
/// the report as a kept entry, and the scheduled `prune-runs` catches up later.
pub fn prune_runs_after_job(managed_root: &Path, app: &str) -> RunsRetentionReport {
    match prune_runs(managed_root, Some(app), false) {
        Ok(r) => r,
        Err(e) => RunsRetentionReport {
            kept: vec![(
                managed_root.join("runs").join(app),
                format!("retention pass failed: {e}"),
            )],
            ..Default::default()
        },
    }
}

struct Candidate {
    dir: PathBuf,
    marker: RunMarker,
}

fn prune_app(
    runs: &Path,
    app: &str,
    dry_run: bool,
    report: &mut RunsRetentionReport,
) -> Result<()> {
    let app_dir = runs.join(app);
    match fs::symlink_metadata(&app_dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
        Ok(m) if m.file_type().is_symlink() || !m.is_dir() => {
            report
                .kept
                .push((app_dir, "app directory is a link or not a directory".into()));
            return Ok(());
        }
        Ok(_) => {}
    }
    if fs::canonicalize(&app_dir).ok().as_deref() != Some(app_dir.as_path()) {
        report
            .kept
            .push((app_dir, "app directory does not resolve to itself".into()));
        return Ok(());
    }
    let _guard = if dry_run {
        None
    } else {
        match try_lock(&app_dir, LOCK_WAIT)? {
            Some(g) => Some(g),
            None => {
                report.kept.push((
                    app_dir,
                    "another retention pass holds the lock; skipped".into(),
                ));
                return Ok(());
            }
        }
    };
    let now = now_ms();
    let mut passing: BTreeMap<RunKind, Vec<Candidate>> = BTreeMap::new();
    let mut doomed: Vec<(Candidate, String)> = vec![];
    for entry in fs::read_dir(&app_dir)?.flatten() {
        let dir = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_symlink() {
            report
                .kept
                .push((dir, "link skipped, never followed".into()));
            continue;
        }
        if !ft.is_dir() || name.starts_with('.') {
            continue;
        }
        let marker_path = dir.join(RUN_MARKER);
        let marker_is_file = fs::symlink_metadata(&marker_path)
            .map(|m| m.is_file())
            .unwrap_or(false);
        let Some(marker) = marker_is_file.then(|| read_marker(&marker_path)).flatten() else {
            report
                .kept
                .push((dir, format!("no valid {RUN_MARKER}; not ours to remove")));
            continue;
        };
        if marker.app != app || marker.run_id != name {
            report
                .kept
                .push((dir, "run marker names another app or run id".into()));
            continue;
        }
        if marker_active(&marker, now) {
            report.kept.push((dir, "active run".into()));
            continue;
        }
        if run_keeps_data(&dir) {
            report
                .kept
                .push((dir, "kept data (the run's owner asked to keep it)".into()));
            continue;
        }
        if crate::storage::in_active_gate_run(&marker) {
            report
                .kept
                .push((dir, "belongs to the active batch run".into()));
            continue;
        }
        let outcome = marker.outcome;
        match outcome {
            None => report
                .kept
                .push((dir, "no recorded outcome (running or older marker)".into())),
            Some(RunOutcome::Failed) => report.kept.push((dir, "failed run".into())),
            Some(RunOutcome::Skipped) => {
                doomed.push((
                    Candidate { dir, marker },
                    "run executed nothing (all scenarios skipped)".into(),
                ));
            }
            Some(RunOutcome::Passed) => passing
                .entry(marker.kind.unwrap_or(RunKind::Functional))
                .or_default()
                .push(Candidate { dir, marker }),
        }
    }
    for (kind, mut list) in passing {
        // Newest first: end time, else start time, then run id so ties are deterministic.
        list.sort_by(|a, b| {
            let key = |c: &Candidate| c.marker.completed_at.unwrap_or(c.marker.created_at);
            key(b)
                .cmp(&key(a))
                .then_with(|| b.marker.run_id.cmp(&a.marker.run_id))
        });
        let mut it = list.into_iter();
        if let Some(newest) = it.next() {
            report
                .kept
                .push((newest.dir, format!("newest passing {kind} run")));
        }
        for old in it {
            doomed.push((old, format!("older passing {kind} run")));
        }
    }
    doomed.sort_by(|a, b| a.0.dir.cmp(&b.0.dir));
    for (c, why) in doomed {
        let bytes = directory_bytes(&c.dir).unwrap_or(0);
        if dry_run {
            report.removed_bytes = report.removed_bytes.saturating_add(bytes);
            report.removed.push((c.dir, bytes, why));
            continue;
        }
        match remove_run_dir(runs, &app_dir, &c) {
            Ok(()) => {
                report.removed_bytes = report.removed_bytes.saturating_add(bytes);
                report.removed.push((c.dir, bytes, why));
            }
            Err(e) => report.kept.push((c.dir, format!("not removed: {e}"))),
        }
    }
    Ok(())
}

/// Remove one run directory after re-checking, at the moment of removal, that it is a plain
/// directory directly inside `<runs>/<app>` that still carries its marker.
fn remove_run_dir(runs: &Path, app_dir: &Path, c: &Candidate) -> std::result::Result<(), String> {
    let dir = &c.dir;
    let real = fs::canonicalize(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if !safe_dir(dir)
        || real != *dir
        || real.parent() != Some(app_dir)
        || app_dir.parent() != Some(runs)
        || !is_inside(runs, &real)
    {
        return Err(format!(
            "{} is not a plain run directory inside the runs root",
            dir.display()
        ));
    }
    let marker_path = real.join(RUN_MARKER);
    let marker_ok = fs::symlink_metadata(&marker_path)
        .map(|m| m.is_file())
        .unwrap_or(false)
        && read_marker(&marker_path)
            .is_some_and(|m| m.app == c.marker.app && m.run_id == c.marker.run_id);
    if !marker_ok {
        return Err("run marker changed or vanished".into());
    }
    // The app home outside the run (bound by the marker) goes first, as the count prune does.
    remove_app_root(&real, &c.marker).map_err(|e| e.0)?;
    // The marker goes last: a removal that stops part way is still ours and is finished later.
    remove_children(&real, Some(std::ffi::OsStr::new(RUN_MARKER)))?;
    remove_children(&real, None)?;
    fs::remove_dir(&real).map_err(|e| format!("{}: {e}", real.display()))
}

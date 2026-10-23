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
//! * a run whose owner asked to keep its data ([`KEEP_DATA_FILE`], written by [`mark_keep_data`])
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
pub use crate::storage::{mark_keep_data, run_keeps_data, KEEP_DATA_FILE};

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage;
    use crate::util::new_id;

    fn temp_root(label: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("rkqa-retention-{label}-{}", new_id()));
        fs::create_dir_all(&p).unwrap();
        fs::canonicalize(p).unwrap()
    }

    /// A finished run under `root` with the given recorded facts. `done_at` is both its start
    /// and its end time, so ordering is the number given.
    fn run(
        root: &Path,
        app: &str,
        id: &str,
        kind: Option<RunKind>,
        outcome: Option<RunOutcome>,
        done_at: u128,
    ) -> PathBuf {
        // Written by hand, not through `storage::create`, whose own count-based prune would
        // remove these fixtures before the test runs.
        let dir = root.join("runs").join(app).join(id);
        fs::create_dir_all(dir.join("evidence")).unwrap();
        fs::write(dir.join("evidence").join("proof.txt"), id).unwrap();
        let marker = RunMarker {
            schema_version: 1,
            app: app.into(),
            run_id: id.into(),
            created_at: done_at,
            owner_pid: 0,
            lease_until: 0,
            completed_at: Some(done_at),
            app_root: None,
            kind,
            outcome,
        };
        storage::write_marker(&dir, &marker).unwrap();
        dir
    }

    fn exists(p: &Path) -> bool {
        fs::symlink_metadata(p).is_ok()
    }

    #[test]
    fn keeps_failed_and_the_newest_pass_per_kind_and_removes_the_rest() {
        let root = temp_root("rule");
        let f = RunKind::Functional;
        let v = RunKind::Visual;
        let pass = Some(RunOutcome::Passed);
        let fail_old = run(
            &root,
            "app",
            "fail-old",
            Some(f),
            Some(RunOutcome::Failed),
            1,
        );
        let f1 = run(&root, "app", "f1", Some(f), pass, 10);
        let f2 = run(&root, "app", "f2", Some(f), pass, 20);
        let f3 = run(&root, "app", "f3", Some(f), pass, 30);
        let v1 = run(&root, "app", "v1", Some(v), pass, 5);
        let v2 = run(&root, "app", "v2", Some(v), pass, 6);
        let fail_visual = run(
            &root,
            "app",
            "fail-visual",
            Some(v),
            Some(RunOutcome::Failed),
            2,
        );
        let running = run(&root, "app", "running", Some(f), None, 40);
        let nothing = run(
            &root,
            "app",
            "nothing",
            Some(f),
            Some(RunOutcome::Skipped),
            50,
        );
        let legacy = run(&root, "app", "legacy", None, pass, 3);
        let other = run(&root, "other", "o1", Some(f), pass, 1);
        let other2 = run(&root, "other", "o2", Some(f), pass, 2);

        let r = prune_runs(&root, Some("app"), false).unwrap();
        // Legacy marker without a kind counts as functional: older than f3, so it goes too.
        for gone in [&f1, &f2, &v1, &nothing, &legacy] {
            assert!(!exists(gone), "should be removed: {}", gone.display());
        }
        for kept in [&fail_old, &f3, &v2, &fail_visual, &running] {
            assert!(exists(kept), "should be kept: {}", kept.display());
        }
        assert_eq!(r.removed.len(), 5);
        assert!(r.removed_bytes > 0);
        // Another app is untouched when pruning one app.
        assert!(exists(&other) && exists(&other2));
        // `None` prunes every app.
        let r = prune_runs(&root, None, false).unwrap();
        assert!(!exists(&other) && exists(&other2));
        assert_eq!(r.removed.len(), 1);
        // A second pass has nothing left to do.
        assert!(prune_runs(&root, None, false).unwrap().removed.is_empty());
    }

    #[test]
    fn a_kept_data_run_survives_a_later_pass_of_the_same_app() {
        // Two runs of one app finish one after the other (the concurrent-runs case): the later one's
        // retention pass must not remove the earlier one when that run's owner kept its data.
        let root = temp_root("keep-data");
        let pass = Some(RunOutcome::Passed);
        let f = Some(RunKind::Functional);
        let kept = run(&root, "app", "kept", f, pass, 10);
        fs::write(kept.join("evidence").join("world-data.txt"), "marker").unwrap();
        mark_keep_data(&kept).unwrap();
        let plain = run(&root, "app", "plain", f, pass, 11);
        let newest = run(&root, "app", "newest", f, pass, 12);
        let r = prune_runs(&root, Some("app"), false).unwrap();
        assert!(exists(&kept), "a kept-data run was removed");
        assert!(
            exists(&kept.join("evidence").join("world-data.txt")),
            "the kept run's data was removed"
        );
        assert!(
            !exists(&plain),
            "an ordinary older pass still goes: {}",
            r.summary()
        );
        assert!(exists(&newest));
        assert!(r
            .kept
            .iter()
            .any(|(p, why)| p.ends_with("kept") && why.contains("kept data")));
        // Whatever its outcome and age, and on every later pass.
        let skipped = run(&root, "app", "skipped", f, Some(RunOutcome::Skipped), 1);
        mark_keep_data(&skipped).unwrap();
        assert!(prune_runs(&root, None, false).unwrap().removed.is_empty());
        assert!(exists(&kept) && exists(&skipped));
        // The request is a plain file in the run directory: a link of that name does not count.
        let linked = run(&root, "app", "linked", f, pass, 2);
        let target = temp_root("keep-target");
        fs::write(target.join("flag"), "x").unwrap();
        let link = linked.join(KEEP_DATA_FILE);
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(target.join("flag"), &link).is_ok();
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_file(target.join("flag"), &link).is_ok();
        if made {
            prune_runs(&root, Some("app"), false).unwrap();
            assert!(!exists(&linked), "a link must not keep a run");
            assert!(exists(&target.join("flag")));
        }
    }

    #[test]
    fn dry_run_reports_and_removes_nothing() {
        let root = temp_root("dry");
        let pass = Some(RunOutcome::Passed);
        let old = run(&root, "app", "old", Some(RunKind::Functional), pass, 1);
        let new = run(&root, "app", "new", Some(RunKind::Functional), pass, 2);
        let r = prune_runs(&root, Some("app"), true).unwrap();
        assert!(r.dry_run);
        assert_eq!(r.removed.len(), 1);
        assert!(exists(&old) && exists(&new));
    }

    #[test]
    fn only_directories_with_a_matching_marker_are_pruned() {
        let root = temp_root("marker");
        let pass = Some(RunOutcome::Passed);
        let f = Some(RunKind::Functional);
        let app_dir = root.join("runs").join("app");
        let newest = run(&root, "app", "newest", f, pass, 100);
        // No marker: never removed, whatever it holds.
        let bare = app_dir.join("not-a-run");
        fs::create_dir_all(&bare).unwrap();
        fs::write(bare.join("keep.txt"), "x").unwrap();
        // Malformed marker.
        let broken = app_dir.join("broken");
        fs::create_dir_all(&broken).unwrap();
        fs::write(broken.join(RUN_MARKER), "{ not json").unwrap();
        // A valid marker that belongs to a different run id than its directory name.
        let moved = run(&root, "app", "moved", f, pass, 1);
        let renamed = app_dir.join("renamed");
        fs::rename(&moved, &renamed).unwrap();
        // A valid marker for another app, copied into this app's tree.
        let alien = run(&root, "alien", "a1", f, pass, 1);
        let planted = app_dir.join("a1");
        fs::create_dir_all(&planted).unwrap();
        fs::copy(alien.join(RUN_MARKER), planted.join(RUN_MARKER)).unwrap();
        // A loose file and a hidden directory are ignored.
        fs::write(app_dir.join("latest.json"), "{}").unwrap();
        let old = run(&root, "app", "old", f, pass, 2);

        let r = prune_runs(&root, Some("app"), false).unwrap();
        assert!(!exists(&old));
        for kept in [&newest, &bare, &broken, &renamed, &planted, &alien] {
            assert!(exists(kept), "must stay: {}", kept.display());
        }
        assert!(exists(&bare.join("keep.txt")));
        assert!(exists(&app_dir.join("latest.json")));
        assert_eq!(r.removed.len(), 1);
    }

    #[test]
    fn an_active_run_is_kept_even_with_a_recorded_pass() {
        let root = temp_root("active");
        let pass = Some(RunOutcome::Passed);
        let f = Some(RunKind::Functional);
        let live = run(&root, "app", "live", f, pass, 1);
        storage::update_marker(&live, |m| m.lease_until = now_ms() + 3_600_000).unwrap();
        let newer = run(&root, "app", "newer", f, pass, 2);
        let r = prune_runs(&root, Some("app"), false).unwrap();
        assert!(r.removed.is_empty());
        assert!(exists(&live) && exists(&newer));
    }

    #[test]
    fn unsafe_names_and_missing_roots_never_reach_the_disk() {
        let root = temp_root("names");
        assert!(prune_runs(&root, Some("../escape"), false).is_err());
        assert!(prune_runs(&root, Some(""), false).is_err());
        assert!(prune_runs(&root.join("absent"), None, false)
            .unwrap()
            .removed
            .is_empty());
        // No `runs` directory yet: nothing to do.
        assert!(prune_runs(&root, None, false).unwrap().removed.is_empty());
    }

    #[test]
    fn a_link_never_leads_a_pass_outside_the_runs_root() {
        let root = temp_root("links");
        let outside = temp_root("outside");
        let pass = Some(RunOutcome::Passed);
        let f = Some(RunKind::Functional);

        // The outside tree looks exactly like a prunable app: if it were followed, it would lose a run.
        let victim_old = run(&outside, "app", "old", f, pass, 1);
        let victim_new = run(&outside, "app", "new", f, pass, 2);

        // 1. `runs` itself is a link to outside.
        if !try_symlink_dir(&outside.join("runs"), &root.join("runs")) {
            return; // this account may not create symlinks (Windows without the privilege)
        }
        let refused = prune_runs(&root, Some("app"), false).unwrap_err();
        assert!(refused.0.contains("runs root"), "{refused}");
        assert!(prune_runs(&root, None, false).is_err());
        assert!(exists(&victim_old) && exists(&victim_new));

        // 2. An app directory is a link to outside.
        let root = temp_root("links2");
        fs::create_dir_all(root.join("runs")).unwrap();
        assert!(try_symlink_dir(
            &outside.join("runs").join("app"),
            &root.join("runs").join("app")
        ));
        let r = prune_runs(&root, Some("app"), false).unwrap();
        assert!(r.removed.is_empty());
        assert!(r.kept.iter().any(|(_, why)| why.contains("link")));
        assert!(prune_runs(&root, None, false).unwrap().removed.is_empty());
        assert!(exists(&victim_old) && exists(&victim_new));

        // 3. A run directory is a link to a prunable run elsewhere; a link inside a run is
        //    removed as itself and what it points at stays.
        let root = temp_root("links3");
        let newest = run(&root, "app", "newest", f, pass, 100);
        assert!(try_symlink_dir(
            &victim_old,
            &root.join("runs").join("app").join("linked")
        ));
        let doomed = run(&root, "app", "doomed", f, pass, 1);
        let precious = temp_root("precious");
        fs::write(precious.join("keep.txt"), "x").unwrap();
        assert!(try_symlink_dir(
            &precious,
            &doomed.join("evidence").join("escape")
        ));
        let r = prune_runs(&root, Some("app"), false).unwrap();
        assert!(exists(&newest));
        assert!(!exists(&doomed));
        assert!(
            exists(&precious.join("keep.txt")),
            "link target was followed"
        );
        assert!(exists(&victim_old) && exists(&victim_new));
        assert!(r
            .kept
            .iter()
            .any(|(p, why)| p.ends_with("linked") && why.contains("link")));
    }

    #[test]
    fn after_job_pass_prunes_and_reports_without_failing() {
        let root = temp_root("after");
        let pass = Some(RunOutcome::Passed);
        let f = Some(RunKind::Functional);
        let old = run(&root, "app", "old", f, pass, 1);
        let new = run(&root, "app", "new", f, pass, 2);
        let r = prune_runs_after_job(&root, "app");
        assert!(!exists(&old) && exists(&new));
        assert_eq!(r.removed.len(), 1);
        assert!(r.summary().starts_with("removed 1"));
        // A bad app name is reported, not raised.
        let bad = prune_runs_after_job(&root, "../x");
        assert!(bad.removed.is_empty() && !bad.kept.is_empty());
    }

    #[test]
    fn managed_root_of_run_needs_the_runs_shape() {
        let p = Path::new("/m/runs/app/id");
        assert_eq!(managed_root_of_run(p), Some(PathBuf::from("/m")));
        assert_eq!(managed_root_of_run(Path::new("/m/other/app/id")), None);
        assert_eq!(managed_root_of_run(Path::new("id")), None);
    }

    fn try_symlink_dir(target: &Path, link: &Path) -> bool {
        #[cfg(unix)]
        let r = std::os::unix::fs::symlink(target, link);
        #[cfg(windows)]
        let r = std::os::windows::fs::symlink_dir(target, link);
        r.is_ok()
    }
}

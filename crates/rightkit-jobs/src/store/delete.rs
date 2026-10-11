//! Owned cleanup for `JobStore::delete`: the store removes the job's artifact files and
//! scratch directory, inside the roots the application configured and nowhere else.
//!
//! Crash safety. A delete runs in three steps, each of which can be repeated:
//! 1. one transaction validates every path, cancels a live job and records the job in the
//!    crate-owned `rk_job_deleting` table (the "deleting" mark);
//! 2. the files are removed (a missing file counts as already gone);
//! 3. one transaction removes the job's rows and the mark.
//!
//! A crash after step 1 leaves the job row, its artifact rows and the mark, so
//! [`JobStore::finish_deletions`] can run steps 2 and 3 again. The job stays visible (terminal,
//! `cancelled` if it was live) until step 3.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use rightkit_db::rusqlite::{params, OptionalExtension};
use rightkit_db::write_txn;

use super::{artifact_row, now, JobStore, PruneReport, Transition, CANCELLED};
use crate::error::{JobError, JobResult};

/// Crate-owned side table: jobs whose deletion has begun and not finished.
pub const DELETING_TABLE: &str = "rk_job_deleting";

pub(super) const DELETING_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS rk_job_deleting (
  job_id TEXT PRIMARY KEY,
  keep_files INTEGER NOT NULL DEFAULT 0,
  started_at TEXT NOT NULL
);
";

/// Stable error code: an artifact path is not inside the configured roots, or reaches outside
/// them through a symlink or `..`. Nothing was deleted.
pub const PATH_OUTSIDE_ROOT: &str = "path_outside_root";

/// Options for [`JobStore::delete_with`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeleteOptions {
    /// Remove the job's rows but leave its artifact files and scratch directory alone (the
    /// behaviour of `delete` before the store owned cleanup).
    pub keep_files: bool,
}

/// What a delete removed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeleteReport {
    /// The rows removed (jobs, events, artifact rows with their paths, submissions).
    pub prune: PruneReport,
    /// Artifact files or directories removed, as recorded on the artifact rows.
    pub removed: Vec<PathBuf>,
    /// Artifact paths that were already missing.
    pub already_gone: Vec<PathBuf>,
    /// Artifact paths left in place: `keep_files` was set, no root is configured on the
    /// store, or the path could not be handled after the rows went (a late artifact).
    pub kept: Vec<PathBuf>,
    /// Artifact paths also recorded by another job: left in place for it.
    pub shared: Vec<PathBuf>,
    /// The per-job scratch directory removed (`<scratch_root>/<job_id>`), if there was one.
    pub scratch_removed: Option<PathBuf>,
}

#[derive(Default)]
struct Plan {
    /// (path as recorded, resolved target) of files owned by this job alone.
    targets: Vec<(PathBuf, PathBuf)>,
    shared: Vec<PathBuf>,
    /// `<artifact_root>/<job_id>` directories, removed only when empty.
    job_dirs: Vec<PathBuf>,
    /// `<scratch_root>/<job_id>`.
    scratch: Option<PathBuf>,
}

enum Removal {
    Removed,
    Gone,
}

/// `p` with its existing ancestors resolved through symlinks and the not-yet-existing tail
/// appended unchanged (the tail contains no `..`: callers reject it first).
fn canonical_deep(p: &Path) -> JobResult<PathBuf> {
    let mut rest: Vec<OsString> = Vec::new();
    let mut cur = p.to_path_buf();
    loop {
        match std::fs::canonicalize(&cur) {
            Ok(mut out) => {
                for part in rest.iter().rev() {
                    out.push(part);
                }
                return Ok(out);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let Some(name) = cur.file_name().map(|n| n.to_owned()) else {
                    return Err(JobError::new(
                        PATH_OUTSIDE_ROOT,
                        format!("cannot resolve {}", p.display()),
                    ));
                };
                rest.push(name);
                if !cur.pop() {
                    return Err(JobError::new(
                        PATH_OUTSIDE_ROOT,
                        format!("cannot resolve {}", p.display()),
                    ));
                }
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// The path to act on for the recorded artifact `p`: its parent resolved through symlinks, the
/// final component kept as is (a symlink there is unlinked, never followed). Refused unless it
/// lies strictly inside one of the canonical `roots`.
fn resolve_inside(p: &Path, roots: &[PathBuf]) -> JobResult<PathBuf> {
    let outside = |why: &str| {
        JobError::new(
            PATH_OUTSIDE_ROOT,
            format!("artifact path {} {why}; nothing was deleted", p.display()),
        )
    };
    if !p.is_absolute() {
        return Err(outside("is not absolute"));
    }
    if p.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(outside("contains .."));
    }
    let name = p.file_name().ok_or_else(|| outside("has no file name"))?;
    let parent = p.parent().ok_or_else(|| outside("has no parent"))?;
    let target = canonical_deep(parent)?.join(name);
    if roots.iter().any(|r| target != *r && target.starts_with(r)) {
        Ok(target)
    } else {
        Err(outside(
            "is outside the configured artifact and scratch roots",
        ))
    }
}

/// `id` as a single plain path component, or `None` (a job id like `../x` gets no directory
/// cleanup).
fn single_component(id: &str) -> Option<&str> {
    let mut comps = Path::new(id).components();
    match (comps.next(), comps.next()) {
        (Some(Component::Normal(n)), None) if n == std::ffi::OsStr::new(id) => Some(id),
        _ => None,
    }
}

fn remove_target(t: &Path) -> JobResult<Removal> {
    match std::fs::symlink_metadata(t) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Removal::Gone),
        Err(e) => Err(e.into()),
        Ok(m) => {
            let ft = m.file_type();
            let res = if ft.is_dir() {
                // Real directory (not a link): std's remove_dir_all does not follow links inside.
                std::fs::remove_dir_all(t)
            } else {
                // File or symlink: unlink only. A directory symlink needs remove_dir on Windows.
                std::fs::remove_file(t).or_else(|e| {
                    if ft.is_symlink() {
                        std::fs::remove_dir(t)
                    } else {
                        Err(e)
                    }
                })
            };
            match res {
                Ok(()) => Ok(Removal::Removed),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Removal::Gone),
                Err(e) => Err(e.into()),
            }
        }
    }
}

impl JobStore {
    fn files_managed(&self) -> bool {
        !self.cfg.artifact_roots.is_empty() || self.cfg.scratch_root.is_some()
    }

    fn artifact_roots_canonical(&self) -> JobResult<Vec<PathBuf>> {
        self.cfg
            .artifact_roots
            .iter()
            .map(|r| canonical_deep(r))
            .collect()
    }

    fn all_roots_canonical(&self) -> JobResult<Vec<PathBuf>> {
        let mut roots = self.artifact_roots_canonical()?;
        if let Some(s) = &self.cfg.scratch_root {
            roots.push(canonical_deep(s)?);
        }
        Ok(roots)
    }

    /// Resolve and validate everything `job_id` owns on disk. Refuses with
    /// [`PATH_OUTSIDE_ROOT`] before anything is touched.
    fn plan_deletion(
        &self,
        conn: &rightkit_db::rusqlite::Connection,
        job_id: &str,
    ) -> JobResult<Plan> {
        let roots = self.all_roots_canonical()?;
        let mut st = conn.prepare(&self.art_sql("WHERE job_id=?1 ORDER BY created_at, rowid"))?;
        let arts = st
            .query_map([job_id], artifact_row)?
            .collect::<Result<Vec<_>, _>>()?;
        let mut plan = Plan::default();
        for a in arts {
            let recorded = PathBuf::from(&a.path);
            let target = resolve_inside(&recorded, &roots)?;
            let others: i64 = conn.query_row(
                &format!(
                    "SELECT COUNT(*) FROM {} WHERE path=?1 AND job_id<>?2",
                    self.cfg.artifacts.table
                ),
                params![a.path, job_id],
                |r| r.get(0),
            )?;
            if others > 0 {
                plan.shared.push(recorded);
            } else if !plan.targets.iter().any(|(_, t)| *t == target) {
                plan.targets.push((recorded, target));
            }
        }
        if let Some(name) = single_component(job_id) {
            for r in self.artifact_roots_canonical()? {
                plan.job_dirs.push(r.join(name));
            }
            if let Some(s) = &self.cfg.scratch_root {
                let dir = canonical_deep(s)?.join(name);
                // Never take a file another job still records down with the scratch directory.
                if !plan.shared.iter().any(|p| p.starts_with(&dir)) {
                    plan.scratch = Some(dir);
                }
            }
        }
        Ok(plan)
    }

    /// Step 1 of a delete: validate paths, cancel a live job and mark the job "deleting".
    /// Nothing on disk is touched. [`JobStore::complete_delete`] (or, after a crash,
    /// [`JobStore::finish_deletions`]) does the rest.
    ///
    /// # Errors
    /// `job_not_found`; `illegal_transition` for a held job; [`PATH_OUTSIDE_ROOT`] when a
    /// recorded artifact path is outside the configured roots (checked unless `keep_files`).
    pub fn begin_delete(&self, job_id: &str, opts: &DeleteOptions) -> JobResult<()> {
        let mut conn = self.c();
        let tx = write_txn(&mut conn)?;
        let before = self.load(&tx, job_id)?;
        let mut was_live = false;
        if !before.job.is_terminal() {
            if self.cfg.statuses.is_held(&before.job.status) {
                return Err(JobError::new(
                    "illegal_transition",
                    format!(
                        "job {job_id} is held in {}; resolve it before deleting",
                        before.job.status
                    ),
                ));
            }
            was_live = true;
        }
        if self.files_managed() && !opts.keep_files {
            self.plan_deletion(&tx, job_id)?;
        }
        if was_live {
            self.exec(&tx, &before, &Transition::to(CANCELLED))?;
        }
        tx.execute(
            "INSERT OR REPLACE INTO rk_job_deleting (job_id, keep_files, started_at) VALUES (?1,?2,?3)",
            params![job_id, opts.keep_files as i64, now()],
        )?;
        tx.commit()?;
        drop(conn);
        self.settle(job_id, was_live);
        Ok(())
    }

    /// Steps 2 and 3 of a delete begun with [`JobStore::begin_delete`]: remove the files, then
    /// the rows and the mark. Safe to call again after a failure or a crash.
    ///
    /// # Errors
    /// `job_not_found` when no deletion is pending for `job_id`; [`PATH_OUTSIDE_ROOT`] if the
    /// configuration changed since step 1; `io` when a file cannot be removed (the mark stays,
    /// so a later call resumes).
    pub fn complete_delete(&self, job_id: &str) -> JobResult<DeleteReport> {
        let keep: Option<i64> = {
            let conn = self.c();
            conn.query_row(
                "SELECT keep_files FROM rk_job_deleting WHERE job_id=?1",
                [job_id],
                |r| r.get(0),
            )
            .optional()?
        };
        let Some(keep) = keep else {
            return Err(JobError::new(
                "job_not_found",
                format!("no deletion pending for job {job_id}"),
            ));
        };
        let exists: bool = {
            let conn = self.c();
            conn.query_row("SELECT 1 FROM jobs WHERE job_id=?1", [job_id], |_| Ok(()))
                .optional()?
                .is_some()
        };
        if !exists {
            // Pruned or removed another way since the mark was written: only the mark is left.
            self.c()
                .execute("DELETE FROM rk_job_deleting WHERE job_id=?1", [job_id])?;
            return Ok(DeleteReport::default());
        }
        let remove_files = keep == 0 && self.files_managed();
        let mut report = DeleteReport::default();
        let mut handled: Vec<PathBuf> = Vec::new();
        if remove_files {
            let plan = {
                let conn = self.c();
                self.plan_deletion(&conn, job_id)?
            };
            for (recorded, target) in &plan.targets {
                match remove_target(target)? {
                    Removal::Removed => report.removed.push(recorded.clone()),
                    Removal::Gone => report.already_gone.push(recorded.clone()),
                }
                handled.push(recorded.clone());
            }
            handled.extend(plan.shared.iter().cloned());
            report.shared = plan.shared.clone();
            for dir in &plan.job_dirs {
                // Empty directories only: a shared file may live inside.
                if std::fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_dir()) {
                    let _ = std::fs::remove_dir(dir);
                }
            }
            if let Some(dir) = &plan.scratch {
                if let Removal::Removed = remove_target(dir)? {
                    report.scratch_removed = Some(dir.clone());
                }
            }
        }
        let mut conn = self.c();
        let tx = write_txn(&mut conn)?;
        let prune = self.remove_jobs(&tx, vec![job_id.to_string()])?;
        tx.commit()?;
        drop(conn);
        // Artifact rows added after the plan was made (a handler still finishing): handle
        // them now that the rows are gone, best effort.
        let roots = if remove_files {
            self.all_roots_canonical().unwrap_or_default()
        } else {
            Vec::new()
        };
        for a in &prune.artifacts {
            let recorded = PathBuf::from(&a.path);
            if handled.contains(&recorded) {
                continue;
            }
            if !remove_files {
                report.kept.push(recorded);
                continue;
            }
            match resolve_inside(&recorded, &roots).and_then(|t| remove_target(&t)) {
                Ok(Removal::Removed) => report.removed.push(recorded),
                Ok(Removal::Gone) => report.already_gone.push(recorded),
                Err(_) => report.kept.push(recorded),
            }
        }
        report.prune = prune;
        Ok(report)
    }

    /// Remove one job and, when the store has artifact or scratch roots configured
    /// ([`crate::StoreConfig::artifact_root`], [`crate::StoreConfig::scratch_root`]), its
    /// files, in the crash-safe order described in the module docs. See [`JobStore::delete`]
    /// for the cancel and held-job rules.
    ///
    /// With no root configured the store cannot tell which paths are the job's own, so no file
    /// is touched (they are listed in [`DeleteReport::kept`]); that is the behaviour before
    /// roots existed.
    ///
    /// # Errors
    /// As [`JobStore::begin_delete`] and [`JobStore::complete_delete`]. After an `io` failure
    /// the deletion is pending: call [`JobStore::finish_deletions`] later.
    pub fn delete_with(&self, job_id: &str, opts: &DeleteOptions) -> JobResult<DeleteReport> {
        self.begin_delete(job_id, opts)?;
        self.complete_delete(job_id)
    }

    /// Ids of jobs whose deletion began and has not finished, oldest first.
    pub fn pending_deletions(&self) -> JobResult<Vec<String>> {
        let conn = self.c();
        let mut st =
            conn.prepare("SELECT job_id FROM rk_job_deleting ORDER BY started_at, rowid")?;
        let ids = st
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ids)
    }

    /// The recovery pass: finish every interrupted deletion. Call it at startup, after opening
    /// the store with the same roots. Each entry is the job id and the outcome of finishing it;
    /// one failure does not stop the others.
    pub fn finish_deletions(&self) -> JobResult<Vec<(String, JobResult<DeleteReport>)>> {
        Ok(self
            .pending_deletions()?
            .into_iter()
            .map(|id| {
                let r = self.complete_delete(&id);
                (id, r)
            })
            .collect())
    }
}

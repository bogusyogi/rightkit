//! Pre-warmed reusable git-worktree pool: acquire/release lifecycle over the
//! ported [`super::db::WorktreeDb`] registry and [`super::sync::WorktreeSync`]
//! sync engine.
//!
//! Original glue, not a donor port. Canon atom VG-004 names only `sync.rs`
//! and `db/mod.rs` as donors (see
//! `docs/canon/daemon-master-canon-atoms.md` row VG-004); the grok-build pool
//! *orchestration* itself (fill scheduling, acquire/release call sites) lives
//! in `worktree/mod.rs`, `worktree/execute.rs`, and `api/gc.rs`, none of
//! which are on this lane's allowlist, so it was not inspected or ported.
//! `bin/pool_perf_bench.rs` (also named in this lane's donor list) is a
//! *benchmark* that exercises the same primitives, not the pool
//! implementation — its doc comment lists the lifecycle this module
//! reproduces: create -> warm -> sync (acquire) -> use -> release -> cleanup.
//!
//! This module composes the two verbatim-ported pieces into that same
//! acquire/release shape with durable pool-readiness state (the DB's
//! `alive`/`dead` status plus a `meta` row per pool marking it warm), kept
//! deliberately small. It is a bounded, honest implementation of the atom's
//! *contract*, not a line-for-line port — flag for Sage review if closer
//! fidelity to the donor's exact fill-worker/parallelism strategy is later
//! required.

use std::path::{Path, PathBuf};
#[cfg(feature = "coding_tools-host")]
use std::sync::Arc;

use anyhow::{Context, Result};

use super::db::{WorktreeDb, WorktreeKind, WorktreeRecord, WorktreeStatus};
use super::sync::WorktreeSync;
#[cfg(feature = "coding_tools-host")]
use rightkit_sandbox::execution::{CommandLaunchSpec, ProcessLaunchAdapter, ProcessLaunchContext};

const META_KEY_POOL_READY_PREFIX: &str = "pool_ready:";
const META_KEY_POOL_LEASE_PREFIX: &str = "pool_lease:";

/// An acquired pool worktree that has not been released yet, and the owner
/// that acquired it. Durable across daemon restart via the pool's meta table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolLease {
    pub worktree_id: String,
    pub owner: String,
    pub path: PathBuf,
}

/// SQLite connections are intentionally confined behind one synchronous lock
/// so async pool operations can borrow `WorktreePool` without making daemon
/// handler futures non-`Send`.
struct SynchronizedWorktreeDb(std::sync::Mutex<WorktreeDb>);

impl SynchronizedWorktreeDb {
    fn with<R>(&self, f: impl FnOnce(&WorktreeDb) -> R) -> R {
        let db = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&db)
    }
    fn register(&self, record: &WorktreeRecord) -> Result<()> {
        self.with(|db| db.register(record))
    }
    fn unregister(&self, id: &str) -> Result<bool> {
        self.with(|db| db.unregister(id))
    }
    #[allow(dead_code)]
    fn unregister_by_path(&self, path: &Path) -> Result<bool> {
        self.with(|db| db.unregister_by_path(path))
    }
    fn touch(&self, id: &str) -> Result<bool> {
        self.with(|db| db.touch(id))
    }
    fn list(&self, filter: &super::db::ListFilter) -> Result<Vec<WorktreeRecord>> {
        self.with(|db| db.list(filter))
    }
    fn stats(&self) -> Result<super::db::DbStats> {
        self.with(|db| db.stats())
    }
    fn sweep_dead(&self) -> Result<u64> {
        self.with(|db| db.sweep_dead())
    }
    fn get_meta(&self, key: &str) -> Result<Option<String>> {
        self.with(|db| db.get_meta(key))
    }
    fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.with(|db| db.set_meta(key, value))
    }
}

/// A pre-warmed worktree pool for one source repository.
pub struct WorktreePool {
    source: PathBuf,
    pool_root: PathBuf,
    db: SynchronizedWorktreeDb,
    #[cfg(feature = "coding_tools-host")]
    process_launch_adapter: Option<Arc<dyn ProcessLaunchAdapter>>,
    #[cfg(feature = "coding_tools-host")]
    process_launch_context: ProcessLaunchContext,
}

impl WorktreePool {
    /// Opens (or creates) the pool registry under `state_dir` for worktrees
    /// created as siblings under `pool_root`.
    pub fn open(source: &Path, pool_root: &Path, state_dir: &Path) -> Result<Self> {
        // `pool_root` is the host app-owned managed worktree state, separate from
        // source files governed by FileService.
        std::fs::create_dir_all(pool_root)
            .with_context(|| format!("failed to create pool root: {}", pool_root.display()))?;
        Ok(Self {
            source: source.to_path_buf(),
            pool_root: pool_root.to_path_buf(),
            db: SynchronizedWorktreeDb(std::sync::Mutex::new(WorktreeDb::open(state_dir)?)),
            #[cfg(feature = "coding_tools-host")]
            process_launch_adapter: None,
            #[cfg(feature = "coding_tools-host")]
            process_launch_context: ProcessLaunchContext {
                workspace_root: source.to_string_lossy().into_owned(),
                sandbox_profile: Some("workspace".to_string()),
                sandbox_required: true,
                network_required: false,
            },
        })
    }

    #[cfg(feature = "coding_tools-host")]
    pub fn with_process_launch(
        mut self,
        adapter: Arc<dyn ProcessLaunchAdapter>,
        context: ProcessLaunchContext,
    ) -> Self {
        self.process_launch_adapter = Some(adapter);
        self.process_launch_context = context;
        self
    }

    #[cfg(feature = "coding_tools-host")]
    pub async fn fill_async(&self, target: usize) -> Result<usize> {
        self.db.sweep_dead()?;
        let existing = self.ready_count()?;
        let mut created = 0usize;
        for _ in existing..target {
            self.create_one_async().await?;
            created += 1;
        }
        Ok(created)
    }

    /// Fills the pool up to `target` warm, alive, unclaimed worktrees for
    /// this source repo. Idempotent: existing alive pool worktrees count
    /// toward `target`.
    pub fn fill(&self, target: usize) -> Result<usize> {
        // A daemon may have been restarted after a pool worktree was removed
        // externally. Reconcile those durable rows before deciding how many
        // slots remain; stale `alive` rows must not make readiness lie.
        self.db.sweep_dead()?;
        let existing = self.ready_count()?;
        let mut created = 0usize;
        for _ in existing..target {
            self.create_one()?;
            created += 1;
        }
        Ok(created)
    }

    #[allow(dead_code)]
    fn warm_count(&self) -> Result<usize> {
        let filter = super::db::ListFilter {
            source_repo: Some(self.source.clone()),
            kind: Some(WorktreeKind::Pool),
            status: Some(WorktreeStatus::Alive),
            ..Default::default()
        };
        Ok(self.db.list(&filter)?.len())
    }

    fn create_one(&self) -> Result<WorktreeRecord> {
        #[cfg(not(test))]
        anyhow::bail!("synchronous worktree creation is test-only; use governed async pool API");

        #[cfg(test)]
        {
            let id = format!("pool-{}", uuid_like());
            let dest = self.pool_root.join(&id);

            let output = rightkit_child_process::command_std("git")
                .current_dir(&self.source)
                .args([
                    "worktree",
                    "add",
                    "--detach",
                    dest.to_str().context("pool worktree path must be UTF-8")?,
                ])
                .output()
                .context("failed to run git worktree add")?;
            if !output.status.success() {
                anyhow::bail!(
                    "git worktree add failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }

            let record = WorktreeRecord {
                id: id.clone(),
                path: dest,
                source_repo: self.source.clone(),
                repo_name: super::db::repo_name_from_path(&self.source),
                kind: WorktreeKind::Pool,
                creation_mode: "linked".to_string(),
                git_ref: None,
                head_commit: None,
                session_id: None,
                creator_pid: Some(std::process::id()),
                created_at: super::db::now_epoch_secs(),
                last_accessed_at: None,
                status: WorktreeStatus::Alive,
                metadata: None,
            };
            self.db.register(&record)?;
            self.db
                .set_meta(&format!("{META_KEY_POOL_READY_PREFIX}{id}"), "1")?;
            Ok(record)
        }
    }

    #[cfg(feature = "coding_tools-host")]
    async fn create_one_async(&self) -> Result<WorktreeRecord> {
        let id = format!("pool-{}", uuid_like());
        let dest = self.pool_root.join(&id);
        let adapter: Arc<dyn ProcessLaunchAdapter> = self
            .process_launch_adapter
            .clone()
            .unwrap_or_else(|| Arc::new(crate::coding_tools::shell::NativeCommandSandboxAdapter));
        let spec = CommandLaunchSpec {
            program: "git".to_string(),
            argv: vec![
                "worktree".to_string(),
                "add".to_string(),
                "--detach".to_string(),
                dest.to_str()
                    .context("pool worktree path must be UTF-8")?
                    .to_string(),
            ],
            cwd: self.source.to_string_lossy().into_owned(),
            context: ProcessLaunchContext {
                workspace_root: self.source.to_string_lossy().into_owned(),
                ..self.process_launch_context.clone()
            },
            env: Vec::new(),
        };
        let mut child = adapter
            .spawn(spec)
            .await
            .map_err(|error| anyhow::anyhow!("git worktree add failed: {error}"))?;
        let mut stderr = child.take_stderr();
        let (status, error_output) = tokio::join!(child.wait(), async {
            let mut bytes = Vec::new();
            if let Some(stream) = stderr.as_mut() {
                let _ = tokio::io::AsyncReadExt::read_to_end(stream, &mut bytes).await;
            }
            bytes
        });
        let status =
            status.map_err(|error| anyhow::anyhow!("git worktree add wait failed: {error}"))?;
        if status != 0 {
            anyhow::bail!(
                "git worktree add failed: {}",
                String::from_utf8_lossy(&error_output)
            );
        }
        let record = WorktreeRecord {
            id: id.clone(),
            path: dest,
            source_repo: self.source.clone(),
            repo_name: super::db::repo_name_from_path(&self.source),
            kind: WorktreeKind::Pool,
            creation_mode: "linked".to_string(),
            git_ref: None,
            head_commit: None,
            session_id: None,
            creator_pid: Some(std::process::id()),
            created_at: super::db::now_epoch_secs(),
            last_accessed_at: None,
            status: WorktreeStatus::Alive,
            metadata: None,
        };
        self.db.register(&record)?;
        self.db
            .set_meta(&format!("{META_KEY_POOL_READY_PREFIX}{id}"), "1")?;
        Ok(record)
    }

    /// Acquires one warm worktree, syncing it to the source's current HEAD
    /// and dirty state before handing it to the caller. Removes it from the
    /// pool registry (the caller now owns its lifecycle) and returns its
    /// path.
    pub fn acquire(&self, copy_dirty: bool) -> Result<PathBuf> {
        self.db.sweep_dead()?;
        let filter = super::db::ListFilter {
            source_repo: Some(self.source.clone()),
            kind: Some(WorktreeKind::Pool),
            status: Some(WorktreeStatus::Alive),
            ..Default::default()
        };
        let mut candidates = self.db.list(&filter)?;
        let record = match candidates.pop() {
            Some(record) => record,
            None => self.create_one()?,
        };

        let sync = WorktreeSync::new(&self.source, &record.path);
        // Pool worktrees are reset+clean'd on release, so `skip_clean=true`
        // on acquire is safe (matches donor pool-path rationale in sync.rs).
        sync.sync_worktree_opts(copy_dirty, /*skip_clean*/ true)
            .with_context(|| format!("failed to sync pool worktree {}", record.path.display()))?;

        self.db.touch(&record.id)?;
        self.db.unregister(&record.id)?;
        let _ = self
            .db
            .set_meta(&format!("{META_KEY_POOL_READY_PREFIX}{}", record.id), "0");
        Ok(record.path)
    }

    #[cfg(feature = "coding_tools-host")]
    pub async fn acquire_async(&self, copy_dirty: bool) -> Result<PathBuf> {
        self.db.sweep_dead()?;
        let filter = super::db::ListFilter {
            source_repo: Some(self.source.clone()),
            kind: Some(WorktreeKind::Pool),
            status: Some(WorktreeStatus::Alive),
            ..Default::default()
        };
        let mut candidates = self.db.list(&filter)?;
        // A pooled record whose directory or `.git` link is gone is stale;
        // drop it rather than let git resolve some enclosing repository.
        let record = loop {
            match candidates.pop() {
                Some(record) if record.path.join(".git").is_file() => break record,
                Some(stale) => {
                    self.db.unregister(&stale.id)?;
                }
                None => break self.create_one_async().await?,
            }
        };
        if !record.path.join(".git").is_file() {
            anyhow::bail!(
                "pooled worktree {} was not created (no .git link)",
                record.path.display()
            );
        }
        // Never reset/clean anything but the pooled worktree itself: if git
        // resolves this path to another repository (e.g. the source repo the
        // pool lives in), refuse before any destructive command runs.
        let toplevel = self
            .run_git_capture_async(&record.path, ["rev-parse", "--show-toplevel"])
            .await?;
        let same_tree = std::path::Path::new(toplevel.trim())
            .canonicalize()
            .ok()
            .zip(record.path.canonicalize().ok())
            .is_some_and(|(top, own)| top == own);
        if !same_tree {
            anyhow::bail!(
                "pooled worktree {} resolves to repository {}; refusing to reset it",
                record.path.display(),
                toplevel.trim()
            );
        }
        let source_head = self
            .run_git_capture_async(&self.source, ["rev-parse", "HEAD"])
            .await?;
        let worktree_head = self
            .run_git_capture_async(&record.path, ["rev-parse", "HEAD"])
            .await?;
        if source_head.trim() != worktree_head.trim() {
            self.run_git_capture_async(&record.path, ["reset", "--hard", source_head.trim()])
                .await?;
        }
        self.run_git_capture_async(&record.path, ["clean", "-fd"])
            .await?;
        if copy_dirty {
            let status = self
                .run_git_bytes_async(
                    &self.source,
                    ["status", "--porcelain=v2", "-z", "--untracked-files=all"],
                )
                .await?;
            let sync = WorktreeSync::new(&self.source, &record.path);
            sync.apply_precomputed_dirty(&super::sync::SourceDirtyState::from_bytes(status))?;
        }
        self.db.touch(&record.id)?;
        self.db.unregister(&record.id)?;
        let _ = self
            .db
            .set_meta(&format!("{META_KEY_POOL_READY_PREFIX}{}", record.id), "0");
        Ok(record.path)
    }

    #[cfg(feature = "coding_tools-host")]
    async fn run_git_capture_async<const N: usize>(
        &self,
        cwd: &Path,
        argv: [&str; N],
    ) -> Result<String> {
        String::from_utf8(self.run_git_bytes_async(cwd, argv).await?)
            .context("git output was not UTF-8")
    }

    #[cfg(feature = "coding_tools-host")]
    async fn run_git_bytes_async<const N: usize>(
        &self,
        cwd: &Path,
        argv: [&str; N],
    ) -> Result<Vec<u8>> {
        let adapter: Arc<dyn ProcessLaunchAdapter> = self
            .process_launch_adapter
            .clone()
            .unwrap_or_else(|| Arc::new(crate::coding_tools::shell::NativeCommandSandboxAdapter));
        let mut child = adapter
            .spawn(CommandLaunchSpec {
                program: "git".into(),
                argv: argv.iter().map(|v| (*v).into()).collect(),
                cwd: cwd.to_string_lossy().into_owned(),
                context: ProcessLaunchContext {
                    // A pool worktree's git admin dir lives in the source
                    // repository's `.git/worktrees`, so confine git to the
                    // source repository (which contains the pool root).
                    workspace_root: self.source.to_string_lossy().into_owned(),
                    ..self.process_launch_context.clone()
                },
                // Git must resolve `cwd` itself, never an enclosing repository.
                env: vec![(
                    "GIT_CEILING_DIRECTORIES".to_string(),
                    super::sync::ceiling_for(cwd).to_string_lossy().into_owned(),
                )],
            })
            .await
            .map_err(|error| anyhow::anyhow!("git launch failed: {error}"))?;
        let mut out = child.take_stdout();
        let mut err = child.take_stderr();
        let (status, bytes, stderr) = tokio::join!(
            child.wait(),
            async {
                let mut bytes = Vec::new();
                if let Some(stream) = out.as_mut() {
                    let _ = tokio::io::AsyncReadExt::read_to_end(stream, &mut bytes).await;
                }
                bytes
            },
            async {
                let mut ignored = Vec::new();
                if let Some(stream) = err.as_mut() {
                    let _ = tokio::io::AsyncReadExt::read_to_end(stream, &mut ignored).await;
                }
                ignored
            }
        );
        let status = status.map_err(|error| anyhow::anyhow!("git wait failed: {error}"))?;
        if status != 0 {
            let stderr = String::from_utf8_lossy(&stderr);
            anyhow::bail!(
                "git {} failed with status {status}: {}",
                argv.first().copied().unwrap_or_default(),
                stderr.trim()
            );
        }
        Ok(bytes)
    }

    #[cfg(feature = "coding_tools-host")]
    #[allow(dead_code)]
    async fn reset_clean_async(&self, worktree: &Path) -> Result<()> {
        let adapter: Arc<dyn ProcessLaunchAdapter> = self
            .process_launch_adapter
            .clone()
            .unwrap_or_else(|| Arc::new(crate::coding_tools::shell::NativeCommandSandboxAdapter));
        for argv in [
            vec![
                "reset".to_string(),
                "--hard".to_string(),
                "HEAD".to_string(),
            ],
            vec!["clean".to_string(), "-fd".to_string()],
        ] {
            let mut child = adapter
                .spawn(CommandLaunchSpec {
                    program: "git".into(),
                    argv,
                    cwd: worktree.to_string_lossy().into_owned(),
                    context: ProcessLaunchContext {
                        workspace_root: self.source.to_string_lossy().into_owned(),
                        ..self.process_launch_context.clone()
                    },
                    // Destructive: never let git walk up into the source repo.
                    env: vec![(
                        "GIT_CEILING_DIRECTORIES".to_string(),
                        super::sync::ceiling_for(worktree)
                            .to_string_lossy()
                            .into_owned(),
                    )],
                })
                .await
                .map_err(|error| anyhow::anyhow!("git pool acquire launch failed: {error}"))?;
            let status = child
                .wait()
                .await
                .map_err(|error| anyhow::anyhow!("git pool acquire wait failed: {error}"))?;
            if status != 0 {
                anyhow::bail!("git pool acquire command failed with status {status}");
            }
        }
        Ok(())
    }

    /// Releases a previously acquired worktree back into the pool: resets it
    /// to the source's HEAD (dropping caller changes) and re-registers it as
    /// warm and alive.
    pub fn release(&self, worktree: &Path) -> Result<()> {
        let sync = WorktreeSync::new(&self.source, worktree);
        sync.sync_worktree_opts(/*copy_dirty*/ false, /*skip_clean*/ false)
            .with_context(|| format!("failed to reset released worktree {}", worktree.display()))?;

        let id = super::db::id_from_path(worktree);
        let record = WorktreeRecord {
            id: id.clone(),
            path: worktree.to_path_buf(),
            source_repo: self.source.clone(),
            repo_name: super::db::repo_name_from_path(&self.source),
            kind: WorktreeKind::Pool,
            creation_mode: "linked".to_string(),
            git_ref: None,
            head_commit: None,
            session_id: None,
            creator_pid: None,
            created_at: super::db::now_epoch_secs(),
            last_accessed_at: Some(super::db::now_epoch_secs()),
            status: WorktreeStatus::Alive,
            metadata: None,
        };
        self.db.register(&record)?;
        self.db
            .set_meta(&format!("{META_KEY_POOL_READY_PREFIX}{id}"), "1")?;
        Ok(())
    }

    #[cfg(feature = "coding_tools-host")]
    pub async fn release_async(&self, worktree: &Path) -> Result<()> {
        let adapter: Arc<dyn ProcessLaunchAdapter> = self
            .process_launch_adapter
            .clone()
            .unwrap_or_else(|| Arc::new(crate::coding_tools::shell::NativeCommandSandboxAdapter));
        for argv in [
            vec![
                "reset".to_string(),
                "--hard".to_string(),
                "HEAD".to_string(),
            ],
            vec!["clean".to_string(), "-fd".to_string()],
        ] {
            let mut child = adapter
                .spawn(CommandLaunchSpec {
                    program: "git".into(),
                    argv,
                    cwd: worktree.to_string_lossy().into_owned(),
                    context: ProcessLaunchContext {
                        workspace_root: self.source.to_string_lossy().into_owned(),
                        ..self.process_launch_context.clone()
                    },
                    // Destructive: never let git walk up into the source repo.
                    env: vec![(
                        "GIT_CEILING_DIRECTORIES".to_string(),
                        super::sync::ceiling_for(worktree)
                            .to_string_lossy()
                            .into_owned(),
                    )],
                })
                .await
                .map_err(|error| anyhow::anyhow!("git pool release launch failed: {error}"))?;
            let status = child
                .wait()
                .await
                .map_err(|error| anyhow::anyhow!("git pool release wait failed: {error}"))?;
            if status != 0 {
                anyhow::bail!("git pool release command failed with status {status}");
            }
        }
        let id = super::db::id_from_path(worktree);
        let record = WorktreeRecord {
            id: id.clone(),
            path: worktree.to_path_buf(),
            source_repo: self.source.clone(),
            repo_name: super::db::repo_name_from_path(&self.source),
            kind: WorktreeKind::Pool,
            creation_mode: "linked".into(),
            git_ref: None,
            head_commit: None,
            session_id: None,
            creator_pid: None,
            created_at: super::db::now_epoch_secs(),
            last_accessed_at: Some(super::db::now_epoch_secs()),
            status: WorktreeStatus::Alive,
            metadata: None,
        };
        self.db.register(&record)?;
        self.db
            .set_meta(&format!("{META_KEY_POOL_READY_PREFIX}{id}"), "1")?;
        Ok(())
    }

    /// Removes a worktree from disk and the registry (pool shutdown / GC).
    #[allow(unused_variables)]
    pub fn discard(&self, worktree: &Path) -> Result<()> {
        #[cfg(not(test))]
        anyhow::bail!("synchronous worktree discard is test-only; use governed async pool API");

        #[cfg(test)]
        {
            let _ = rightkit_child_process::command_std("git")
                .current_dir(&self.source)
                .args([
                    "worktree",
                    "remove",
                    "--force",
                    worktree.to_str().unwrap_or_default(),
                ])
                .output();
            self.db.unregister_by_path(worktree)?;
            Ok(())
        }
    }

    pub fn stats(&self) -> Result<super::db::DbStats> {
        self.db.stats()
    }

    /// Returns the number of pool worktrees that are durably ready for
    /// acquisition. Readiness requires both an alive registry row and its
    /// persisted `pool_ready:*` marker; filesystem liveness is reconciled
    /// before answering so this remains correct after daemon restart/GC.
    pub fn ready_count(&self) -> Result<usize> {
        self.db.sweep_dead()?;
        let filter = super::db::ListFilter {
            source_repo: Some(self.source.clone()),
            kind: Some(WorktreeKind::Pool),
            status: Some(WorktreeStatus::Alive),
            ..Default::default()
        };
        let records = self.db.list(&filter)?;
        let mut ready = 0usize;
        for record in records {
            let key = format!("{META_KEY_POOL_READY_PREFIX}{}", record.id);
            if self.db.get_meta(&key)?.as_deref() == Some("1")
                && std::fs::symlink_metadata(&record.path).is_ok()
            {
                ready += 1;
            }
        }
        Ok(ready)
    }

    /// Whether pool has at least one durable, live, ready checkout.
    pub fn is_ready(&self) -> Result<bool> {
        Ok(self.ready_count()? > 0)
    }

    /// Whether the worktree with this id is durably marked warm (released and
    /// not currently acquired).
    pub fn is_warm(&self, worktree_id: &str) -> Result<bool> {
        Ok(self
            .db
            .get_meta(&format!("{META_KEY_POOL_READY_PREFIX}{worktree_id}"))?
            .as_deref()
            == Some("1"))
    }

    /// Records that `owner` holds an acquired worktree. Returns the worktree id
    /// (the same id `release` will register the checkout under).
    pub fn record_lease(&self, worktree: &Path, owner: &str) -> Result<String> {
        let id = super::db::id_from_path(worktree);
        let value = serde_json::json!({
            "owner": owner,
            "path": worktree.to_string_lossy(),
        })
        .to_string();
        self.db
            .set_meta(&format!("{META_KEY_POOL_LEASE_PREFIX}{id}"), &value)?;
        Ok(id)
    }

    /// The live lease for a worktree id, if the worktree is currently acquired.
    pub fn lease_of(&self, worktree_id: &str) -> Result<Option<PoolLease>> {
        let key = format!("{META_KEY_POOL_LEASE_PREFIX}{worktree_id}");
        let Some(raw) = self.db.get_meta(&key)? else {
            return Ok(None);
        };
        if raw.is_empty() {
            return Ok(None);
        }
        let value: serde_json::Value =
            serde_json::from_str(&raw).with_context(|| format!("corrupt pool lease {key}"))?;
        Ok(Some(PoolLease {
            worktree_id: worktree_id.to_string(),
            owner: value["owner"].as_str().unwrap_or_default().to_string(),
            path: PathBuf::from(value["path"].as_str().unwrap_or_default()),
        }))
    }

    /// Clears a lease once its worktree has been released back into the pool.
    pub fn clear_lease(&self, worktree_id: &str) -> Result<()> {
        self.db
            .set_meta(&format!("{META_KEY_POOL_LEASE_PREFIX}{worktree_id}"), "")
    }
}

fn uuid_like() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}-{}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn require_git() {
        if rightkit_child_process::command_std("git")
            .arg("--version")
            .output()
            .is_err()
        {
            panic!("git binary required for this test");
        }
    }

    fn init_git_repo(path: &Path) {
        let run = |args: &[&str]| {
            let status = rightkit_child_process::command_std("git")
                .current_dir(path)
                .args(args)
                .status()
                .expect("run git");
            assert!(status.success(), "git {args:?} failed");
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test"]);
    }

    fn git_commit_all(path: &Path, message: &str) {
        let run = |args: &[&str]| {
            let status = rightkit_child_process::command_std("git")
                .current_dir(path)
                .args(args)
                .status()
                .expect("run git");
            assert!(status.success(), "git {args:?} failed");
        };
        run(&["add", "-A"]);
        run(&["commit", "-q", "-m", message]);
    }

    #[test]
    fn fill_creates_target_warm_worktrees_and_is_idempotent() {
        require_git();
        let temp = tempfile::tempdir().expect("temp dir");
        let source = temp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        init_git_repo(&source);
        std::fs::write(source.join("file.txt"), "v1").unwrap();
        git_commit_all(&source, "initial");

        let pool_root = temp.path().join("pool");
        let state_dir = temp.path().join("state");
        let pool = WorktreePool::open(&source, &pool_root, &state_dir).expect("open pool");

        let created = pool.fill(2).expect("fill");
        assert_eq!(created, 2);
        assert_eq!(pool.warm_count().unwrap(), 2);

        // Idempotent: filling to the same target again creates nothing new.
        let created_again = pool.fill(2).expect("fill again");
        assert_eq!(created_again, 0);
        assert_eq!(pool.warm_count().unwrap(), 2);
    }

    #[test]
    fn acquire_syncs_and_removes_from_pool_then_release_returns_it() {
        require_git();
        let temp = tempfile::tempdir().expect("temp dir");
        let source = temp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        init_git_repo(&source);
        std::fs::write(source.join("file.txt"), "v1").unwrap();
        git_commit_all(&source, "initial");

        let pool_root = temp.path().join("pool");
        let state_dir = temp.path().join("state");
        let pool = WorktreePool::open(&source, &pool_root, &state_dir).expect("open pool");
        pool.fill(1).expect("fill");
        assert_eq!(pool.warm_count().unwrap(), 1);

        // Source advances after the pool worktree was created.
        std::fs::write(source.join("file.txt"), "v2").unwrap();
        git_commit_all(&source, "second");

        let acquired = pool.acquire(false).expect("acquire");
        assert_eq!(
            pool.warm_count().unwrap(),
            0,
            "acquired worktree leaves the pool"
        );
        assert_eq!(
            std::fs::read_to_string(acquired.join("file.txt")).unwrap(),
            "v2",
            "acquire must sync to source's current HEAD"
        );

        pool.release(&acquired).expect("release");
        assert_eq!(
            pool.warm_count().unwrap(),
            1,
            "released worktree rejoins the pool"
        );
    }
}

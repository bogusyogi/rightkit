//! Sync a pre-created worktree to match a source repo's current state.
//!
//! DIRECT_PORT_RUST (atom VG-004, lane A17-vg-rust-ports): ported from
//! grok-build `crates/codegen/xai-fast-worktree/src/sync.rs`. Donor revision
//! and NOTICE entry are recorded in
//! `docs/canon/daemon-master-canon-atoms.md`.
//!
//! Seam adaptations (the sync algorithm, porcelain-v2 parsing, and every test
//! scenario are verbatim):
//!   - `get_head_commit` used `gix` for HEAD resolution in the donor. `gix`
//!     is not a the host app dependency; this port shells out to
//!     `git rev-parse HEAD` instead (still a single blocking call, same
//!     contract: current commit SHA of the given worktree/repo).
//!   - `git_command()`/`git_reset_hard_command()`/`git_clean_fd()` were
//!     donor helpers in `git/checkout.rs` (not on this port's allowlist);
//!     they are inlined verbatim below, minus the `xai_tty_utils` prompt/
//!     pager-suppression helpers (not a the host app dependency) — the
//!     `GIT_*` auth-suppression env vars are kept.
//!   - `clone_file()` used the `reflink_copy` crate for copy-on-write. That
//!     crate is not a the host app dependency; this port uses `std::fs::copy`
//!     (a plain copy, not CoW) and still propagates source file permissions
//!     the same way. This is a real behavior narrowing (no CoW fast path on
//!     APFS/Btrfs/XFS) flagged here for Sage/Oracle: pool sync will be
//!     correct but not as fast as the donor on CoW-capable filesystems until
//!     `reflink-copy` (or an equivalent) is added as a dependency.
//!   - `copy_git_index` (donor: `git::index::copy_git_index`, used only by
//!     one donor test) is out of this port's allowlist; the one test that
//!     exercised it is omitted here rather than faked.

use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use bytes::Bytes;

/// Environment variables set on every git command to suppress interactive
/// prompts (ported from donor `git/checkout.rs::GIT_AUTH_SUPPRESSION_ENVS`).
const GIT_AUTH_SUPPRESSION_ENVS: [(&str, &str); 4] = [
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GIT_ASKPASS", ""),
    ("GIT_LFS_SKIP_SMUDGE", "1"),
    ("GIT_SSH_COMMAND", "ssh -o BatchMode=yes"),
];

fn git_command() -> Command {
    let mut cmd = rightkit_child_process::command_std("git");
    cmd.stdin(Stdio::null());
    for &(key, val) in &GIT_AUTH_SUPPRESSION_ENVS {
        cmd.env(key, val);
    }
    cmd.arg("--no-optional-locks");
    cmd
}

/// Ceiling that stops git discovery at the worktree itself.
pub(super) fn ceiling_for(worktree_path: &Path) -> std::ffi::OsString {
    worktree_path
        .parent()
        .map(|parent| parent.as_os_str().to_os_string())
        .unwrap_or_default()
}

fn git_reset_hard_command(worktree_path: &Path, target: Option<&str>) -> Result<()> {
    let tgt = target.unwrap_or("HEAD");
    let output = git_command()
        .current_dir(worktree_path)
        // Destructive: git must never walk up into the repository that
        // contains the pool and reset/clean it instead.
        .env("GIT_CEILING_DIRECTORIES", ceiling_for(worktree_path))
        .args(["reset", "--hard", tgt])
        .output()
        .context("failed to run git reset")?;
    if !output.status.success() {
        anyhow::bail!(
            "git reset --hard {} failed: {}",
            tgt,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    tracing::debug!(path = %worktree_path.display(), target = %tgt, "git reset --hard");
    Ok(())
}

fn git_clean_fd(worktree_path: &Path, include_ignored: bool) -> Result<()> {
    let flags = if include_ignored { "-fdx" } else { "-fd" };
    let output = git_command()
        .current_dir(worktree_path)
        // Destructive: git must never walk up into the repository that
        // contains the pool and reset/clean it instead.
        .env("GIT_CEILING_DIRECTORIES", ceiling_for(worktree_path))
        .args(["clean", flags])
        .output()
        .context("failed to run git clean")?;
    if !output.status.success() {
        anyhow::bail!(
            "git clean {} failed: {}",
            flags,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

/// Resolve HEAD's commit SHA (seam: `git rev-parse HEAD` instead of donor's
/// `gix`-based resolution; see module doc).
fn get_head_commit(repo_path: &Path) -> Result<String> {
    let output = git_command()
        .current_dir(repo_path)
        .args(["rev-parse", "HEAD"])
        .output()
        .context("failed to run git rev-parse HEAD")?;
    if !output.status.success() {
        anyhow::bail!(
            "git rev-parse HEAD failed in {}: {}",
            repo_path.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Copy a file (seam: plain `std::fs::copy`, not the donor's CoW reflink;
/// see module doc).
fn clone_file(src: &Path, dest: &Path) -> Result<()> {
    std::fs::copy(src, dest)
        .with_context(|| format!("failed to copy {} -> {}", src.display(), dest.display()))?;
    let perms = std::fs::metadata(src)?.permissions();
    std::fs::set_permissions(dest, perms)?;
    Ok(())
}

fn replace_symlink(target: &Path, dst: &Path) -> std::io::Result<()> {
    let _ = std::fs::remove_file(dst);
    symlink_to(target, dst)
}

#[cfg(unix)]
fn symlink_to(target: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, dst)
}

#[cfg(windows)]
fn symlink_to(target: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(target, dst)
}

/// Pre-collected dirty state from a source repository.
#[derive(Clone, Debug)]
pub struct SourceDirtyState {
    raw: Bytes,
}

impl SourceDirtyState {
    pub(crate) fn from_bytes(raw: Vec<u8>) -> Self {
        Self {
            raw: Bytes::from(raw),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }
}

pub fn collect_source_dirty_state(source: &Path) -> Result<SourceDirtyState> {
    let output = git_command()
        .args(["status", "--porcelain=v2", "-z", "--untracked-files=all"])
        .current_dir(source)
        .output()
        .context("failed to run git status")?;
    if !output.status.success() {
        anyhow::bail!(
            "git status failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(SourceDirtyState {
        raw: Bytes::from(output.stdout),
    })
}

#[derive(Clone, Debug, Default)]
pub struct SyncReport {
    pub head_moved: bool,
    pub dirty_files_copied: u64,
    pub files_deleted: u64,
    pub staged_entries: u64,
    pub clean_skipped: bool,
    pub dirty_skipped: bool,
    pub head_resolve_ms: u64,
    pub reset_hard_ms: u64,
    pub clean_ms: u64,
    pub dirty_sync_ms: u64,
    pub git_status_ms: u64,
    pub file_ops_ms: u64,
    pub staged_replay_ms: u64,
}

/// Sync a pre-created worktree to match a source repo's current state.
pub struct WorktreeSync<'a> {
    pub source: &'a Path,
    pub worktree: &'a Path,
}

impl<'a> WorktreeSync<'a> {
    pub fn new(source: &'a Path, worktree: &'a Path) -> Self {
        Self { source, worktree }
    }

    pub fn sync_worktree(&self, copy_dirty: bool) -> Result<SyncReport> {
        self.sync_worktree_opts(copy_dirty, false)
    }

    #[allow(unused_variables)]
    pub fn sync_worktree_opts(&self, copy_dirty: bool, skip_clean: bool) -> Result<SyncReport> {
        anyhow::bail!("synchronous worktree sync is test-only; use governed async pool API");
    }

    #[allow(unreachable_code)]
    #[allow(unused_variables)]
    pub fn sync_from_precomputed(
        &self,
        dirty_state: Option<&SourceDirtyState>,
        skip_clean: bool,
    ) -> Result<SyncReport> {
        anyhow::bail!("synchronous worktree sync is test-only; use governed async pool API");

        let mut report = SyncReport::default();

        let source_head = get_head_commit(self.source).context("failed to get source HEAD")?;
        let worktree_head =
            get_head_commit(self.worktree).context("failed to get worktree HEAD")?;

        if source_head != worktree_head {
            report.head_moved = true;
            git_reset_hard_command(self.worktree, Some(&source_head))?;
        }

        if skip_clean {
            report.clean_skipped = true;
        } else {
            git_clean_fd(self.worktree, false)?;
        }

        match dirty_state {
            Some(state) if !state.is_empty() => {
                let result = apply_porcelain_v2_entries(&state.raw, self.source, self.worktree)?;
                report.dirty_files_copied = result.copied;
                report.files_deleted = result.deleted;
                if !result.staged_adds.is_empty() || !result.staged_deletes.is_empty() {
                    report.staged_entries = replay_staged_changes(
                        self.worktree,
                        &result.staged_adds,
                        &result.staged_deletes,
                    )?;
                }
            }
            Some(_) => {
                report.dirty_skipped = true;
            }
            None => {
                report.dirty_skipped = true;
            }
        }

        Ok(report)
    }

    /// Applies already-collected source status without launching Git. Host
    /// callers use this after collecting all Git facts through launch adapter.
    pub fn apply_precomputed_dirty(&self, dirty_state: &SourceDirtyState) -> Result<SyncReport> {
        let result = apply_porcelain_v2_entries(&dirty_state.raw, self.source, self.worktree)?;
        let staged_entries = if !result.staged_adds.is_empty() || !result.staged_deletes.is_empty()
        {
            replay_staged_changes(self.worktree, &result.staged_adds, &result.staged_deletes)?
        } else {
            0
        };
        Ok(SyncReport {
            dirty_files_copied: result.copied,
            files_deleted: result.deleted,
            staged_entries,
            ..Default::default()
        })
    }

    pub fn sync_dirty_state(&self) -> Result<SyncReport> {
        self.sync_dirty_state_timed()
    }

    fn sync_dirty_state_timed(&self) -> Result<SyncReport> {
        use std::time::Instant;
        let mut report = SyncReport::default();

        let t = Instant::now();
        let output = git_command()
            .args(["status", "--porcelain=v2", "-z", "--untracked-files=all"])
            .current_dir(self.source)
            .output()
            .context("failed to run git status")?;
        report.git_status_ms = t.elapsed().as_millis() as u64;

        if !output.status.success() {
            anyhow::bail!(
                "git status failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        if !output.stdout.is_empty() {
            let t = Instant::now();
            let result = apply_porcelain_v2_entries(&output.stdout, self.source, self.worktree)?;
            report.file_ops_ms = t.elapsed().as_millis() as u64;
            report.dirty_files_copied = result.copied;
            report.files_deleted = result.deleted;

            if !result.staged_adds.is_empty() || !result.staged_deletes.is_empty() {
                let t = Instant::now();
                report.staged_entries = replay_staged_changes(
                    self.worktree,
                    &result.staged_adds,
                    &result.staged_deletes,
                )?;
                report.staged_replay_ms = t.elapsed().as_millis() as u64;
            }
        }

        Ok(report)
    }
}

struct ApplyResult {
    copied: u64,
    deleted: u64,
    staged_adds: Vec<(String, String, String)>,
    staged_deletes: Vec<String>,
}

fn apply_porcelain_v2_entries(
    stdout: &[u8],
    source: &Path,
    worktree: &Path,
) -> Result<ApplyResult> {
    let mut copied: u64 = 0;
    let mut deleted: u64 = 0;
    let mut staged_adds: Vec<(String, String, String)> = Vec::new();
    let mut staged_deletes: Vec<String> = Vec::new();

    let mut chunks = stdout.split(|&b| b == 0).peekable();

    while let Some(chunk) = chunks.next() {
        if chunk.is_empty() {
            continue;
        }

        let line = std::str::from_utf8(chunk).context("non-UTF-8 path in git status output")?;

        if line.starts_with("1 ") || line.starts_with("u ") {
            if is_submodule_entry(line) {
                tracing::debug!(line = %line, "skipping submodule entry in dirty sync");
                continue;
            }

            let path = extract_ordinary_path(line);
            let xy = &line[2..4];
            apply_file_change(xy, path, source, worktree, &mut copied, &mut deleted)?;

            let x = xy.as_bytes()[0];
            if x != b'.' {
                if x == b'D' {
                    staged_deletes.push(path.to_string());
                } else if let Some((mode, hash)) = extract_index_mode_hash(line) {
                    staged_adds.push((mode, hash, path.to_string()));
                }
            }
        } else if line.starts_with("2 ") {
            if is_submodule_entry(line) {
                tracing::debug!(line = %line, "skipping submodule rename entry in dirty sync");
                let _ = chunks.next();
                continue;
            }

            let path = extract_ordinary_path(line);
            let xy = &line[2..4];

            apply_file_change(xy, path, source, worktree, &mut copied, &mut deleted)?;

            if let Some((mode, hash)) = extract_index_mode_hash(line) {
                staged_adds.push((mode, hash, path.to_string()));
            }

            if let Some(orig_chunk) = chunks.next() {
                let orig_path = std::str::from_utf8(orig_chunk)
                    .context("non-UTF-8 origPath in rename entry")?;
                let old_dest = worktree.join(orig_path);
                if old_dest.exists() {
                    let _ = std::fs::remove_file(&old_dest);
                    deleted += 1;
                }
                staged_deletes.push(orig_path.to_string());
            }
        } else if let Some(path) = line.strip_prefix("? ") {
            copy_file_to_worktree(source, worktree, path)?;
            copied += 1;
        }
    }

    Ok(ApplyResult {
        copied,
        deleted,
        staged_adds,
        staged_deletes,
    })
}

fn is_submodule_entry(line: &str) -> bool {
    line.split(' ')
        .nth(2)
        .is_some_and(|sub| sub.starts_with('S'))
}

fn extract_index_mode_hash(line: &str) -> Option<(String, String)> {
    let fields: Vec<&str> = line.splitn(10, ' ').collect();
    if fields.len() >= 8 {
        Some((fields[4].to_string(), fields[7].to_string()))
    } else {
        None
    }
}

fn extract_ordinary_path(line: &str) -> &str {
    let prefix = if line.starts_with("2 ") {
        9
    } else if line.starts_with("u ") {
        10
    } else {
        8
    };

    let mut spaces_seen = 0;
    for (i, c) in line.char_indices() {
        if c == ' ' {
            spaces_seen += 1;
            if spaces_seen == prefix {
                return &line[i + 1..];
            }
        }
    }
    line
}

fn apply_file_change(
    xy: &str,
    path: &str,
    source: &Path,
    worktree: &Path,
    copied: &mut u64,
    deleted: &mut u64,
) -> Result<()> {
    let y = xy.as_bytes()[1];

    if y == b'D' {
        let dest_file = worktree.join(path);
        if dest_file.exists() {
            std::fs::remove_file(&dest_file)
                .with_context(|| format!("failed to delete {}", dest_file.display()))?;
            *deleted += 1;
        }
    } else {
        copy_file_to_worktree(source, worktree, path)?;
        *copied += 1;
    }

    Ok(())
}

fn replay_staged_changes(
    worktree: &Path,
    staged_adds: &[(String, String, String)],
    staged_deletes: &[String],
) -> Result<u64> {
    let mut count = 0u64;

    if !staged_adds.is_empty() {
        use std::io::Write;
        let mut child = git_command()
            .current_dir(worktree)
            .args(["update-index", "-z", "--index-info"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to spawn git update-index --index-info")?;

        {
            let stdin = child
                .stdin
                .as_mut()
                .context("no stdin for git update-index")?;
            for (mode, hash, path) in staged_adds {
                write!(stdin, "{mode} {hash}\t{path}\0")
                    .context("failed to write to git update-index stdin")?;
            }
        }

        let output = child
            .wait_with_output()
            .context("git update-index --index-info failed")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!(
                worktree = %worktree.display(),
                stderr = %stderr.trim(),
                "git update-index --index-info for staged changes failed (non-fatal)"
            );
        } else {
            count += staged_adds.len() as u64;
        }
    }

    if !staged_deletes.is_empty() {
        let mut cmd = git_command();
        cmd.current_dir(worktree)
            .arg("rm")
            .arg("--cached")
            .arg("--ignore-unmatch")
            .arg("--");
        for path in staged_deletes {
            cmd.arg(path);
        }
        let output = cmd
            .output()
            .context("failed to run git rm --cached for staged deletions")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!(
                worktree = %worktree.display(),
                stderr = %stderr.trim(),
                "git rm --cached for staged deletions failed (non-fatal)"
            );
        } else {
            count += staged_deletes.len() as u64;
        }
    }

    Ok(count)
}

fn copy_file_to_worktree(source: &Path, worktree: &Path, rel_path: &str) -> Result<()> {
    let src_file = source.join(rel_path);
    let dst_file = worktree.join(rel_path);

    let src_meta = match std::fs::symlink_metadata(&src_file) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!(path = %rel_path, "source file vanished before copy, skipping");
            return Ok(());
        }
        Err(e) => {
            return Err(e).with_context(|| format!("failed to stat {}", src_file.display()));
        }
    };

    if let Some(parent) = dst_file.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create parent dirs for {}", dst_file.display()))?;
    }

    if src_meta.file_type().is_symlink() {
        let target = std::fs::read_link(&src_file)
            .with_context(|| format!("failed to read symlink {}", src_file.display()))?;
        return replace_symlink(&target, &dst_file)
            .with_context(|| format!("failed to recreate symlink {}", dst_file.display()));
    }

    if std::fs::symlink_metadata(&dst_file).is_ok() {
        let _ = std::fs::remove_file(&dst_file);
    }

    clone_file(&src_file, &dst_file).with_context(|| {
        format!(
            "failed to copy {} -> {}",
            src_file.display(),
            dst_file.display()
        )
    })?;

    Ok(())
}

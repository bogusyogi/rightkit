//! MBR-502: passive Git/read provenance adapter.
//!
//! Every host-driven read that lands in the data plane is preceded by a
//! call into this module. The adapter is strictly passive: it shells out
//! to three read-only `git` subcommands (`rev-parse --verify HEAD`,
//! `status --porcelain`, and `diff --numstat`), captures the working-tree
//! state, and appends one JSONL row to
//! `<RIGHTKIT_CONTEXT_DATA_ROOT>/provenance.jsonl`. It never reads file bodies,
//! never resolves paths outside the workspace, and never opens a network
//! socket — the only external process it talks to is `git` itself, and
//! only against the workspace the caller hands it.
//!
//! The adapter is intentionally total over the operations the runtime
//! actually performs (read, observe, install, uninstall). The schema
//! version is part of the on-disk contract so a future migration can
//! detect and re-stamp old rows without rewriting the file.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Schema version of the working-tree snapshot. Bump on any change to
/// `WorkingTreeSnapshotV1` field set or shape.
pub const WORKING_TREE_SNAPSHOT_SCHEMA_VERSION: u32 = 1;

/// Schema version of the persisted provenance row. Bump on any change to
/// `ProvenanceRowV1` field set, name, or on-disk layout.
pub const PROVENANCE_ROW_SCHEMA_VERSION: u32 = 1;

/// File name (under the `RIGHTKIT_CONTEXT_DATA_ROOT`) that the adapter appends
/// to. Single canonical name so the doc surface, the CLI, and the
/// retention policy all agree.
pub const PROVENANCE_JOURNAL_NAME: &str = "provenance.jsonl";

/// One snapshot of the workspace's git state at the moment a host-driven
/// read happens. The adapter captures this synchronously; nothing here
/// is allowed to touch the file system beyond the three `git` subcommands
/// (so we never see a file body).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkingTreeSnapshotV1 {
    pub schema_version: u32,
    pub workspace_root: PathBuf,
    /// Short or long SHA from `git rev-parse --verify HEAD`. `None` when
    /// the workspace has no commit yet (fresh `git init`).
    pub git_head: Option<String>,
    /// Paths reported by `git status --porcelain`. Never absolute — git
    /// returns project-relative paths by convention, so they cannot leak
    /// a home directory or filesystem layout.
    pub dirty_paths: Vec<String>,
    /// `git diff --numstat` summed `added` column across all files.
    pub diff_added: u64,
    /// `git diff --numstat` summed `removed` column across all files.
    pub diff_removed: u64,
    /// Wall-clock (`now_unix_ms`) the snapshot was captured at. Kept
    /// here so a delayed append can still be reconciled against the
    /// recorded_at on the parent row.
    pub captured_at_unix_ms: u64,
}

/// One row appended to `<data_root>/provenance.jsonl`. The contract is
/// self-contained per line — every append stands alone so the file can
/// be replayed, grep'd, or shell-piped without re-reading earlier rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProvenanceRowV1 {
    pub schema_version: u32,
    /// Stable per-installation id produced by `installation_identity`.
    pub installation_id: String,
    /// Short verb for what the runtime did: `read`, `observe`, `install`,
    /// `uninstall`, etc. Free-form but always lower-case and ASCII.
    pub operation: String,
    /// Optional client identifier (`claude`, `codex`, `manual`, …). When
    /// the caller cannot identify the client, pass `None`.
    pub client_id: Option<String>,
    /// Optional scope-grant token or digest that authorised the read.
    pub scope_grant: Option<String>,
    pub working_tree: WorkingTreeSnapshotV1,
    pub recorded_at_unix_ms: u64,
}

/// All the ways the adapter can fail. `Git` is the only "expected" mode
/// (workspace not a repo, transient git error); `Io` and `NotARepository`
/// are surfaced verbatim so the runtime can decide whether to retry.
#[derive(Debug, thiserror::Error)]
pub enum ProvenanceError {
    /// A `git` subcommand exited non-zero or produced invalid output.
    #[error("git command failed in {workspace}: {stderr}")]
    Git { workspace: PathBuf, stderr: String },
    /// The caller handed us a path that is not a git working tree
    /// (no `.git` directory at the root or any parent). Distinct from
    /// `Git` so the runtime can skip provenance cheaply when the
    /// workspace is intentionally not versioned.
    #[error("{workspace} is not a git repository")]
    NotARepository { workspace: PathBuf },
    /// A non-git I/O failure (creating the data dir, opening the
    /// journal, writing a row). The runtime already logs these; we
    /// keep the variant so the observe() caller can map a write failure
    /// to a retry decision without losing the path context.
    #[error("provenance I/O at {path}: {reason}")]
    Io { path: PathBuf, reason: String },
}

/// Longest a provenance git call may run before it is killed.
const GIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Default git invocation used in production. Uses `git` from `$PATH`,
/// sets `GIT_TERMINAL_PROMPT=0` so a credential or editor prompt can
/// never block the runtime, and routes stderr through so the caller can
/// surface it.
fn default_git_command(workspace: &Path, args: &[&str]) -> Result<String, String> {
    let mut command = rightkit_child_process::OwnedCommand::new("git");
    command
        .command_mut()
        .args(args)
        .current_dir(workspace)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0");
    // The owner's bounded run: deadline with a tree kill, 16 MiB kept per stream, stdin closed.
    let options = rightkit_child_process::RunOptions::new(GIT_TIMEOUT)
        .stdout_cap(16 * 1024 * 1024)
        .stderr_cap(16 * 1024 * 1024);
    let output = rightkit_child_process::run_command(command, None, &options)
        .map_err(|error| format!("spawn git: {error}"))?;
    if output.timed_out {
        return Err(format!(
            "git {args:?} timed out after {}s",
            GIT_TIMEOUT.as_secs()
        ));
    }
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr.bytes).into_owned();
        let stdout = String::from_utf8_lossy(&output.stdout.bytes).into_owned();
        let combined = if stderr.trim().is_empty() {
            stdout
        } else {
            stderr
        };
        return Err(if combined.trim().is_empty() {
            format!("git {:?} exited with status {}", args, output.status)
        } else {
            combined
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout.bytes).into_owned())
}

/// Detect whether the path is a git working tree. We treat the presence
/// of a `.git` directory (or file, in the case of a submodule) as the
/// signal — cheaper than running `git rev-parse` and matches the
/// documented behaviour of `git` itself.
fn is_repository(workspace: &Path) -> bool {
    if !workspace.exists() {
        return false;
    }
    let mut current = Some(workspace);
    while let Some(dir) = current {
        if dir.join(".git").exists() {
            return true;
        }
        current = dir.parent();
    }
    false
}

/// Parse the `git status --porcelain` output. Each line is two
/// characters (status, optional rename/copy flag) then a space then the
/// path. The adapter only needs the path, and never the file body.
fn parse_status_paths(porcelain: &str) -> Vec<String> {
    porcelain
        .lines()
        .filter(|line| line.len() >= 4 && line.as_bytes()[2] == b' ')
        .filter_map(|line| line.get(3..).map(|rest| rest.trim().to_string()))
        .filter(|rest| !rest.is_empty())
        .collect()
}

/// Parse the `git diff --numstat` output. Each line is
/// `<added>\t<removed>\t<path>`. We sum the two integer columns and
/// ignore the path — the row already carries the dirty-path list, and
/// `numstat` includes both staged and unstaged changes against HEAD.
fn parse_numstat(numstat: &str) -> (u64, u64) {
    let mut added: u64 = 0;
    let mut removed: u64 = 0;
    for line in numstat.lines() {
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split('\t');
        let add = parts.next().unwrap_or("");
        let del = parts.next().unwrap_or("");
        // Binary files report `-` `-` for both columns; treat as zero.
        let add_count = if add == "-" {
            0
        } else {
            add.parse::<u64>().unwrap_or(0)
        };
        let del_count = if del == "-" {
            0
        } else {
            del.parse::<u64>().unwrap_or(0)
        };
        added = added.saturating_add(add_count);
        removed = removed.saturating_add(del_count);
    }
    (added, removed)
}

/// Resolve `HEAD` to a short SHA, or `None` when the workspace has no
/// commit yet. `git rev-parse --verify HEAD` exits non-zero on an empty
/// repo, which we map to `None` rather than an error — that is the
/// "expected" state for a fresh install.
fn parse_head(stdout: &str) -> Option<String> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        None
    } else {
        // Take the first whitespace-separated token; `git rev-parse` may
        // append a newline and (with `--short`) extra diagnostics.
        Some(
            trimmed
                .split_whitespace()
                .next()
                .unwrap_or(trimmed)
                .to_string(),
        )
    }
}

/// Capture the working-tree state by invoking three read-only git
/// subcommands. The `git_command` parameter is injected so unit tests
/// can stub git without requiring a real binary on the test machine.
pub fn capture_working_tree(
    workspace_root: &Path,
    now_unix_ms: u64,
) -> Result<WorkingTreeSnapshotV1, ProvenanceError> {
    capture_working_tree_with(workspace_root, now_unix_ms, default_git_command)
}

/// Same as [`capture_working_tree`] but with an injected git shim. The
/// shim receives the workspace path and the argv vector and returns
/// stdout on success or a stderr-style message on failure. Production
/// code uses `default_git_command`; tests pass a closure that returns
/// canned strings.
pub fn capture_working_tree_with<G>(
    workspace_root: &Path,
    now_unix_ms: u64,
    git_command: G,
) -> Result<WorkingTreeSnapshotV1, ProvenanceError>
where
    G: Fn(&Path, &[&str]) -> Result<String, String>,
{
    if !is_repository(workspace_root) {
        return Err(ProvenanceError::NotARepository {
            workspace: workspace_root.to_path_buf(),
        });
    }

    let head = match git_command(workspace_root, &["rev-parse", "--verify", "HEAD"]) {
        Ok(stdout) => parse_head(&stdout),
        Err(stderr) => {
            // An unborn HEAD is the "no commit yet" signal. Anything
            // else is a real git failure.
            if stderr.contains("unknown revision")
                || stderr.contains("ambiguous argument 'HEAD'")
                || stderr.contains("does not have any commits yet")
                || stderr.contains("not a git repository")
            {
                None
            } else {
                return Err(ProvenanceError::Git {
                    workspace: workspace_root.to_path_buf(),
                    stderr,
                });
            }
        }
    };

    let status = git_command(workspace_root, &["status", "--porcelain"]).map_err(|stderr| {
        ProvenanceError::Git {
            workspace: workspace_root.to_path_buf(),
            stderr,
        }
    })?;
    let dirty_paths = parse_status_paths(&status);

    let numstat = git_command(workspace_root, &["diff", "--numstat"]).map_err(|stderr| {
        ProvenanceError::Git {
            workspace: workspace_root.to_path_buf(),
            stderr,
        }
    })?;
    let (diff_added, diff_removed) = parse_numstat(&numstat);

    Ok(WorkingTreeSnapshotV1 {
        schema_version: WORKING_TREE_SNAPSHOT_SCHEMA_VERSION,
        workspace_root: workspace_root.to_path_buf(),
        git_head: head,
        dirty_paths,
        diff_added,
        diff_removed,
        captured_at_unix_ms: now_unix_ms,
    })
}

/// Append one `ProvenanceRowV1` to `<store_path>/provenance.jsonl`. The
/// parent directory and the file are created on first write. Each line
/// is a self-contained JSON object followed by a single `\n`.
pub fn record_provenance(row: ProvenanceRowV1, store_path: &Path) -> Result<(), ProvenanceError> {
    if let Some(parent) = store_path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            fs::create_dir_all(parent).map_err(|error| ProvenanceError::Io {
                path: parent.to_path_buf(),
                reason: format!("create parent: {error}"),
            })?;
        }
    }
    let json = serde_json::to_string(&row).map_err(|error| ProvenanceError::Io {
        path: store_path.to_path_buf(),
        reason: format!("serialize row: {error}"),
    })?;
    let mut line = json;
    line.push('\n');
    rightkit_fs::append_record(
        store_path,
        line.as_bytes(),
        rightkit_fs::AppendOptions::default(),
    )
    .map_err(|error| ProvenanceError::Io {
        path: store_path.to_path_buf(),
        reason: format!("write row: {error}"),
    })?;
    Ok(())
}

/// Convenience: capture the working tree, record the row, return the
/// row. This is the single entry point the runtime + JS adapter call
/// for every host-driven read. Returns the row on success so the caller
/// can correlate it with the read result.
pub fn observe(
    operation: &str,
    client_id: Option<&str>,
    scope_grant: Option<&str>,
    workspace_root: &Path,
    installation_id: &str,
    store_path: &Path,
    now_unix_ms: u64,
) -> Result<ProvenanceRowV1, ProvenanceError> {
    observe_with(
        operation,
        client_id,
        scope_grant,
        workspace_root,
        installation_id,
        store_path,
        now_unix_ms,
        default_git_command,
    )
}

/// Same as [`observe`] but with an injected git shim. See
/// [`capture_working_tree_with`] for the shim contract.
#[allow(clippy::too_many_arguments)]
pub fn observe_with<G>(
    operation: &str,
    client_id: Option<&str>,
    scope_grant: Option<&str>,
    workspace_root: &Path,
    installation_id: &str,
    store_path: &Path,
    now_unix_ms: u64,
    git_command: G,
) -> Result<ProvenanceRowV1, ProvenanceError>
where
    G: Fn(&Path, &[&str]) -> Result<String, String>,
{
    let working_tree = capture_working_tree_with(workspace_root, now_unix_ms, git_command)?;
    let row = ProvenanceRowV1 {
        schema_version: PROVENANCE_ROW_SCHEMA_VERSION,
        installation_id: installation_id.to_string(),
        operation: operation.to_string(),
        client_id: client_id.map(|value| value.to_string()),
        scope_grant: scope_grant.map(|value| value.to_string()),
        working_tree,
        recorded_at_unix_ms: now_unix_ms,
    };
    record_provenance(row.clone(), store_path)?;
    Ok(row)
}

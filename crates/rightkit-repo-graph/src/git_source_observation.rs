// Ported from blueprint/src/graph/git-source-observation.mjs (JS legacy
// source). Canonical git base-commit + working-tree observation, shared by
// build-time (records what a generation was indexed AT) and freshness-time
// (observes what is on disk NOW) call sites. Both MUST run the identical
// `git status` invocation and hash construction, or an unchanged worktree
// could report drift purely from formatting differences between two
// independently-written git observers — there is exactly one implementation.
//
// No native git helper already existed in this crate or in
// membrane-runtime for this exact `rev-parse HEAD` + bounded `git status
// --porcelain=v1 -z` pair (confirmed via grep across both crates' src/), so
// this is a new module. The legacy implementation used Node's
// `execFileSync` with a 5000ms `timeout` option; the same bound is applied by
// `git_command::run_git`.

use std::io::Read;
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use crate::git_command::run_git;

use xxhash_rust::xxh3::Xxh3;

const GIT_TIMEOUT: Duration = Duration::from_millis(5000);
const MAX_GIT_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
/// A dirty file up to this size is hashed in full; a larger one contributes its length and
/// modification time, so one big dirty artifact cannot stall every freshness read (#555).
const MAX_DIRTY_FILE_HASH_BYTES: u64 = 32 * 1024 * 1024;
/// Bytes read for one fingerprint. Past it the observation is incomplete, never clean.
const MAX_DIRTY_HASH_TOTAL_BYTES: u64 = 256 * 1024 * 1024;

/// Runs `git <args>` in `cwd` with a bounded wall-clock timeout, returning
/// stdout bytes on success (exit code 0), or `None` on any failure: git
/// missing, non-zero exit, or timeout. Mirrors the legacy JS observer's
/// blanket try/catch-to-null behavior.
fn run_git_bounded(cwd: &str, args: &[&str]) -> Option<Vec<u8>> {
    run_git_bounded_timeout(cwd, args, GIT_TIMEOUT)
}

/// `run_git_bounded` with a caller-chosen wall-clock bound. Short retrieval
/// paths (freshness reads under a request deadline) pass a fraction of their
/// remaining budget instead of the full observation timeout, so an expensive
/// or hung status read degrades to an honest incomplete observation instead
/// of consuming the caller's entire window.
fn run_git_bounded_timeout(cwd: &str, args: &[&str], timeout: Duration) -> Option<Vec<u8>> {
    let output = run_git(
        Some(Path::new(cwd)),
        args,
        Some(timeout),
        MAX_GIT_OUTPUT_BYTES,
    )
    .ok()?;
    (!output.truncated).then_some(output.bytes)
}

fn looks_like_sha(value: &str) -> bool {
    let len = value.len();
    if !(40..=64).contains(&len) {
        return false;
    }
    value.chars().all(|c| c.is_ascii_hexdigit())
}

/// HEAD commit, or `None` when unavailable (not a git repo, no commits, git
/// missing).
pub fn git_base_commit(root: &str) -> Option<String> {
    git_base_commit_bounded(root, GIT_TIMEOUT)
}

/// HEAD commit under a caller-chosen wall-clock bound. `rev-parse` is a
/// fast index read; the bound covers process spawn on loaded systems.
pub fn git_base_commit_bounded(root: &str, timeout: Duration) -> Option<String> {
    let bytes = run_git_bounded_timeout(root, &["rev-parse", "HEAD"], timeout)?;
    let text = String::from_utf8(bytes).ok()?;
    let trimmed = text.trim();
    if looks_like_sha(trimmed) {
        Some(trimmed.to_lowercase())
    } else {
        None
    }
}

/// `git rev-list --count base..HEAD` under a caller-chosen bound — how many
/// commits the sealed base lags current HEAD. `None` when unmeasurable
/// (shallow clone, unrelated history, git failure, timeout); callers must
/// treat that as unknown lag, never as zero.
pub fn git_commit_distance_bounded(root: &str, base: &str, timeout: Duration) -> Option<u64> {
    let range = format!("{base}..HEAD");
    let bytes = run_git_bounded_timeout(root, &["rev-list", "--count", &range], timeout)?;
    String::from_utf8(bytes).ok()?.trim().parse().ok()
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitSourceObservation {
    pub head: String,
    pub dirty: bool,
    pub status_digest: String,
}

/// `{ head, dirty, statusDigest }` for `root`, or `None` when git is
/// unavailable. `status_digest` is an xxh3-128 hex digest of the exact
/// porcelain status bytes and the content of each listed path, the bounded
/// worktree fingerprint every freshness comparison in Blueprint is built on.
pub fn git_source_observation(root: &str) -> Option<GitSourceObservation> {
    let head = git_base_commit(root)?;
    let status = run_git_bounded(
        root,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--",
            ".",
            ":(exclude).agent",
            ":(exclude).agent/**",
            ":(exclude)docs/product.md",
            ":(exclude)docs/architecture.md",
            ":(exclude)docs/product/README.md",
            ":(exclude)docs/architecture/membrane.md",
        ],
    )?;
    let digest = worktree_digest(root, &status)?;
    Some(GitSourceObservation {
        head,
        dirty: !status.is_empty(),
        status_digest: format!("{digest:032x}"),
    })
}

/// Worktree fingerprint under a caller-chosen wall-clock bound. `None` means
/// the observation did not complete — timed out, failed, or git absent —
/// which callers must surface as an *incomplete* observation, never as a
/// clean tree. Splitting this from `git_base_commit_bounded` lets freshness
/// reads short-circuit on a HEAD drift before paying for `status`.
pub fn git_worktree_fingerprint_bounded(root: &str, timeout: Duration) -> Option<(bool, String)> {
    let status = run_git_bounded_timeout(
        root,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--",
            ".",
            ":(exclude).agent",
            ":(exclude).agent/**",
            ":(exclude)docs/product.md",
            ":(exclude)docs/architecture.md",
            ":(exclude)docs/product/README.md",
            ":(exclude)docs/architecture/membrane.md",
        ],
        timeout,
    )?;
    let digest = worktree_digest(root, &status)?;
    Some((!status.is_empty(), format!("{digest:032x}")))
}

/// xxh3-128 over the porcelain bytes and the content of every path they name. Porcelain lists a
/// dirty path only, so a second edit to an already-modified file left the status bytes unchanged
/// and the fingerprint stayed equal while the graph went stale (#555). `None` when reading the
/// content would exceed the byte budget; the caller then records an incomplete observation.
fn worktree_digest(root: &str, status: &[u8]) -> Option<u128> {
    let mut hasher = Xxh3::new();
    hasher.update(status);
    let mut budget = MAX_DIRTY_HASH_TOTAL_BYTES;
    // Records are NUL-separated `XY path`, relative to the repository top, which an enrolled root
    // is. A rename or copy is followed by its origin path as a bare record (tracked by state, since an
    // origin may itself have a space as its third byte); every record's path is hashed.
    let mut origin_next = false;
    for record in status
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let path_bytes = if origin_next {
            origin_next = false;
            record
        } else if record.len() > 3 && record[2] == b' ' {
            origin_next = matches!(record[0], b'R' | b'C') || matches!(record[1], b'R' | b'C');
            &record[3..]
        } else {
            record
        };
        hasher.update(b"\0path\0");
        hasher.update(path_bytes);
        let path = path_from_bytes(root, path_bytes);
        hash_path_content(&path, &mut hasher, &mut budget)?;
    }
    Some(hasher.digest128())
}

/// Folds one path's current content into `hasher`. Absent, directory and unreadable entries get
/// their own markers so they cannot collide with a file of the same bytes.
fn hash_path_content(path: &Path, hasher: &mut Xxh3, budget: &mut u64) -> Option<()> {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        hasher.update(b"\0absent");
        return Some(());
    };
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        // A link is its target text, never the content it points at: following it could read a
        // FIFO, a device or a file outside the repository.
        match std::fs::read_link(path) {
            Ok(target) => {
                hasher.update(b"\0symlink");
                hasher.update(&path_os_bytes(&target));
            }
            Err(_) => hasher.update(b"\0unreadable"),
        }
        return Some(());
    }
    if file_type.is_dir() {
        hasher.update(b"\0dir");
        return Some(());
    }
    if !file_type.is_file() {
        // FIFO, device or socket: never opened, so a read cannot block.
        hasher.update(b"\0special");
        return Some(());
    }
    if metadata.len() > MAX_DIRTY_FILE_HASH_BYTES {
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |time| time.as_nanos());
        hasher.update(b"\0large");
        hasher.update(&metadata.len().to_le_bytes());
        hasher.update(&modified.to_le_bytes());
        return Some(());
    }
    let Ok(mut file) = std::fs::File::open(path) else {
        hasher.update(b"\0unreadable");
        return Some(());
    };
    hasher.update(b"\0bytes");
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                *budget = budget.checked_sub(read as u64)?;
                hasher.update(&buffer[..read]);
            }
            Err(_) => {
                hasher.update(b"\0read_error");
                break;
            }
        }
    }
    Some(())
}

#[cfg(unix)]
fn path_os_bytes(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}

#[cfg(not(unix))]
fn path_os_bytes(path: &Path) -> Vec<u8> {
    path.to_string_lossy().into_owned().into_bytes()
}

#[cfg(unix)]
fn path_from_bytes(root: &str, bytes: &[u8]) -> std::path::PathBuf {
    use std::os::unix::ffi::OsStrExt;
    Path::new(root).join(std::ffi::OsStr::from_bytes(bytes))
}

#[cfg(not(unix))]
fn path_from_bytes(root: &str, bytes: &[u8]) -> std::path::PathBuf {
    Path::new(root).join(String::from_utf8_lossy(bytes).as_ref())
}

/// Path-oriented convenience wrapper for Blueprint callers that already hold
/// a confined filesystem root. It intentionally delegates to the one string
/// implementation so build-time and freshness-time status bytes cannot drift.
pub fn git_source_observation_at(root: &std::path::Path) -> Option<GitSourceObservation> {
    git_source_observation(&root.to_string_lossy())
}

//! Where a repository's Blueprint graph lives (#554).
//!
//! One function answers it for every reader and writer, so the engine, doctor, snapshot, federate,
//! hybrid-task and export paths can never disagree. When the host has installed a state root
//! ([`set_state_root`], called once by the host runtime at start), the graph is stored outside
//! the repository at `<state root>/blueprint/<name>-<hash of canonical repo root>/graph/`, so it is
//! never inside a work tree where it could be staged or counted into a repository snapshot. With
//! no state root installed (a library caller that never started the runtime) the old
//! `<repo>/.agent/graph/` location is kept. Two clones of one repository have different canonical
//! roots and therefore different keys.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use xxhash_rust::xxh3::xxh3_128;

static STATE_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Install the state root. Set once; a later call with a different root is ignored and reports
/// `false`.
pub fn set_state_root(state_root: PathBuf) -> bool {
    match STATE_ROOT.set(state_root.clone()) {
        Ok(()) => true,
        Err(_) => STATE_ROOT.get() == Some(&state_root),
    }
}

/// The installed state root, if any.
pub fn state_root() -> Option<&'static Path> {
    STATE_ROOT.get().map(PathBuf::as_path)
}

/// Directory holding this repository's Blueprint state: `<state root>/blueprint/<key>` when a
/// state root is installed, else `<repo>/.agent`.
pub fn repo_state_dir(root: &Path) -> PathBuf {
    match state_root() {
        Some(state) => state.join("blueprint").join(repo_key(root)),
        None => root.join(".agent"),
    }
}

/// The graph directory: holds `graph.db`, `full-constructions.jsonl` and `map.html`.
pub fn graph_dir(root: &Path) -> PathBuf {
    repo_state_dir(root).join("graph")
}

/// The graph database file.
pub fn graph_db_path(root: &Path) -> PathBuf {
    graph_dir(root).join("graph.db")
}

/// The findings-baseline directory, beside the graph (`.agent/blueprint/findings-baselines` in
/// the fallback layout).
pub fn findings_baselines_dir(root: &Path) -> PathBuf {
    match state_root() {
        Some(_) => repo_state_dir(root).join("findings-baselines"),
        None => root
            .join(".agent")
            .join("blueprint")
            .join("findings-baselines"),
    }
}

/// `<sanitized repository name>-<first 16 hex digits of the xxh3-128 of the canonical root>`.
fn repo_key(root: &Path) -> String {
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let text = canonical.to_string_lossy();
    let hash = xxh3_128(text.as_bytes());
    let name: String = canonical
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(40)
        .collect();
    format!("{name}-{:016x}", (hash >> 64) as u64)
}

/// Path of the graph database as shown to a user: absolute in the state-root layout, repo-relative
/// in the fallback layout.
pub fn graph_db_display(root: &Path) -> String {
    match state_root() {
        Some(_) => graph_db_path(root).to_string_lossy().into_owned(),
        None => ".agent/graph/graph.db".into(),
    }
}

/// Remove a stale in-repository graph (#554). Before the state root existed, a repository's graph
/// lived at `<repo>/.agent/graph/`. Once the state-root store for the same repository holds a
/// published generation, the legacy files are redundant and are removed here. The legacy store is
/// never removed while it could be the only copy: the state-root store must open and publish a
/// generation first, and any failure leaves the legacy files in place. Runs before each request,
/// costs one `stat` when no legacy store exists, and does nothing without a state root.
pub fn retire_legacy_graph(root: &Path) {
    if state_root().is_none() {
        return;
    }
    let legacy_dir = root.join(".agent").join("graph");
    let legacy_db = legacy_dir.join("graph.db");
    if !legacy_db.exists() {
        return;
    }
    let current = graph_db_path(root);
    if current == legacy_db || !current.exists() {
        return;
    }
    let published = crate::store::open_store_read_only(&current)
        .ok()
        .and_then(|connection| {
            crate::store::read_generation_envelope(&connection)
                .ok()
                .flatten()
        })
        .is_some();
    if !published {
        return;
    }
    for name in [
        "graph.db",
        "graph.db-wal",
        "graph.db-shm",
        "full-constructions.jsonl",
        "map.html",
    ] {
        let _ = std::fs::remove_file(legacy_dir.join(name));
    }
    // Removes the directory only when nothing else remains in it.
    let _ = std::fs::remove_dir(&legacy_dir);
}

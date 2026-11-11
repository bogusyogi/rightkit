//! Reading and migrating the revisioned layout `Store` wrote before 0.7.0:
//!
//! ```text
//! <root>/<model-id>/state.json              active revision, history, per-revision file records
//! <root>/<model-id>/revisions/<key>/<files> one directory per (revision, quant)
//! <root>/<model-id>/.staging/<key>/         resumable in-flight install
//! <root>/<model-id>/.aside/<key>/           previous same-key revision while a replacement commits
//! <root>/<model-id>/.adopt/<key>/           an adoption being assembled
//! ```
//!
//! Nothing here writes that layout. [`migrate_locked`] turns such an entry into a suite entry,
//! under the model lock, the first time the store takes it:
//!
//! 1. the active revision's directory (`revisions/<key>`, or `.aside/<key>` when an old swap was
//!    interrupted), checked against its recorded sizes, is renamed to
//!    `<root>/.incoming/<id>.migrate`, and its record is written there as the suite
//!    `manifest.json` (version = the revision key);
//! 2. anything else the app kept in `<root>/<id>` (a real pre-store folder, never a link) moves
//!    into it;
//! 3. `<root>/<id>` is renamed to `<root>/.incoming/<id>.old`, the new entry renamed onto
//!    `<root>/<id>`, and the old directory (earlier revisions, `state.json`, old staging) deleted.
//!
//! Every step is a rename or an idempotent write, and a rerun resumes from whichever step a
//! crash interrupted. An entry whose active revision does not verify is left as it is (it was not
//! loadable before either); the next install replaces it.

use crate::error::{Error, ErrorCode, Result};
use crate::store::{is_link, manifest_for, remove_tree, State, Store, VerifyMode};
use std::fs;
use std::path::{Path, PathBuf};

/// Names under `<root>/<id>` that belonged to the revisioned store.
pub(crate) const LEGACY_NAMES: &[&str] =
    &["state.json", "revisions", ".staging", ".aside", ".adopt"];

fn exists(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok()
}

/// An entry still in the old layout: `state.json` and no suite `manifest.json`.
pub(crate) fn is_legacy(dir: &Path) -> bool {
    !exists(&crate::suite::paths::manifest_path(dir))
        && fs::symlink_metadata(dir.join("state.json")).is_ok_and(|m| m.is_file())
}

/// The old `state.json`, or an empty state when there is none.
pub(crate) fn read_state(dir: &Path, id: &str) -> Result<State> {
    let path = dir.join("state.json");
    match fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| Error::io(path.display(), e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State {
            id: id.into(),
            ..State::default()
        }),
        Err(e) => Err(Error::io(path.display(), e)),
    }
}

/// `<root>/<id>/revisions/<key>`.
pub(crate) fn revision_dir(dir: &Path, key: &str) -> PathBuf {
    dir.join("revisions").join(key)
}

/// Old-layout paths the store refuses to follow when they are links.
pub(crate) fn owned_paths(dir: &Path) -> Vec<PathBuf> {
    LEGACY_NAMES.iter().map(|n| dir.join(n)).collect()
}

fn scratch(root: &Path, id: &str) -> (PathBuf, PathBuf) {
    let incoming = root.join(".incoming");
    (
        incoming.join(format!("{id}.migrate")),
        incoming.join(format!("{id}.old")),
    )
}

/// Remove an abandoned migration's scratch (uninstall). Best effort.
pub(crate) fn remove_migration_scratch(root: &Path, id: &str) {
    let (staged, old) = scratch(root, id);
    let _ = remove_tree(&staged);
    let _ = remove_tree(&old);
}

/// Migrate (or finish migrating) `<root>/<id>` from the old layout. Caller holds the model lock
/// and has validated `id`. Returns whether the entry changed.
pub(crate) fn migrate_locked(store: &Store, id: &str) -> Result<bool> {
    let dir = store.model_dir(id)?;
    let (staged, old) = scratch(&store.root, id);
    let rename = |from: &Path, to: &Path| {
        fs::rename(from, to)
            .map_err(|e| Error::io(format!("migrate {} -> {}", from.display(), to.display()), e))
    };
    if exists(&staged) {
        if !exists(&dir) {
            // Interrupted between moving the old directory away and the new one in.
            rename(&staged, &dir)?;
            remove_tree(&old)?;
            let _ = fs::remove_dir(store.root.join(".incoming"));
            return Ok(true);
        }
        if !is_legacy(&dir) {
            // Nothing left to migrate into: the scratch is stale.
            remove_tree(&staged)?;
            remove_tree(&old)?;
            return Ok(false);
        }
    } else {
        if exists(&old) && exists(&dir) {
            remove_tree(&old)?;
        }
        if !is_legacy(&dir) {
            return Ok(false);
        }
    }
    let state = read_state(&dir, id)?;
    let Some((key, record)) = state
        .active
        .as_ref()
        .and_then(|k| state.revisions.get(k).map(|r| (k.clone(), r.clone())))
    else {
        remove_tree(&staged)?;
        return Ok(false);
    };
    if !exists(&staged) {
        // Shared-runtime files live outside the revision; only the revision's own files decide.
        let mut own = record.clone();
        own.files.retain(|f| f.shared.is_none());
        let usable = |p: &PathBuf| {
            !is_link(p)
                && p.is_dir()
                && (own.files.is_empty() || store.record_matches(&own, p, VerifyMode::Size))
        };
        let Some(source) = [revision_dir(&dir, &key), dir.join(".aside").join(&key)]
            .into_iter()
            .find(usable)
        else {
            return Ok(false);
        };
        let incoming = store.root.join(".incoming");
        fs::create_dir_all(&incoming).map_err(|e| Error::io(incoming.display(), e))?;
        rename(&source, &staged)?;
    }
    if !exists(&crate::suite::paths::manifest_path(&staged)) {
        crate::suite::manifest::write_manifest(&staged, &manifest_for(id, &key, &record.files))
            .map_err(|e| Error::new(ErrorCode::Io, e))?;
    }
    carry_foreign(&dir, &staged, &[])?;
    remove_tree(&old)?;
    rename(&dir, &old)?;
    rename(&staged, &dir)?;
    remove_tree(&old)?;
    let _ = fs::remove_dir(store.root.join(".incoming"));
    Ok(true)
}

/// Move every real top-level entry of `from` that the store does not own into `to`, unless `to`
/// already has that name: the old layout's names ([`LEGACY_NAMES`]), `manifest.json`, `owned`
/// (the previous manifest's top-level names) and links (old aliases) stay behind.
pub(crate) fn carry_foreign(from: &Path, to: &Path, owned: &[String]) -> Result<()> {
    let Ok(entries) = fs::read_dir(from) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(text) = name.to_str() else { continue };
        let path = entry.path();
        if LEGACY_NAMES.contains(&text)
            || text == "manifest.json"
            || owned.iter().any(|o| o == text)
            || is_link(&path)
            || exists(&to.join(&name))
        {
            continue;
        }
        fs::rename(&path, to.join(&name))
            .map_err(|e| Error::io(format!("keep {}", path.display()), e))?;
    }
    Ok(())
}

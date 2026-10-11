//! Opaque, revocable media sources.
//!
//! Backend code registers a file once and hands the UI a [`SourceId`]; the UI then refers to the
//! file by that id (or by a path that is already registered) and never supplies a raw path that
//! the host has not vetted. An id is derived from the canonical path plus the file's size and
//! modification time, so an id never silently points at a file that has since changed: a changed
//! file fails closed until it is registered again, which issues a new id.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use crate::PlaybackError;

/// Domain separator so source ids can never collide with derivative fingerprints.
const SOURCE_DOMAIN: &[u8] = b"rightkit-playback/source/v1\0";

/// Identifier for a registered media file: 64 lowercase hex characters (SHA-256).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SourceId(String);

impl SourceId {
    /// Parse the textual form produced by [`fmt::Display`]. Anything else is not an id.
    pub fn parse(value: &str) -> Option<Self> {
        let valid = value.len() == 64
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        valid.then(|| Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// File identity captured at registration: a changed size or mtime means a different source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified_nanos: u128,
}

fn stamp(path: &Path) -> Result<Stamp, PlaybackError> {
    let meta = path.metadata().map_err(|_| PlaybackError::Missing)?;
    if !meta.is_file() || meta.len() == 0 {
        return Err(PlaybackError::Empty);
    }
    let modified = meta
        .modified()
        .map_err(|e| PlaybackError::Runtime(e.to_string()))?
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|e| PlaybackError::Runtime(e.to_string()))?;
    Ok(Stamp {
        len: meta.len(),
        modified_nanos: modified.as_nanos(),
    })
}

fn source_id(path: &Path, stamp: Stamp) -> SourceId {
    let mut material = Vec::new();
    material.extend_from_slice(SOURCE_DOMAIN);
    material.extend_from_slice(path.as_os_str().as_encoded_bytes());
    material.push(0);
    material.extend_from_slice(&stamp.len.to_le_bytes());
    material.extend_from_slice(&stamp.modified_nanos.to_le_bytes());
    SourceId(rightkit_fs::digest::sha256_bytes(&material))
}

/// Paths are compared by canonical form when the file exists, so symlinked or relative spellings
/// of a registered file resolve to the same entry. Unknown paths keep their spelling and miss.
fn lookup_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Host-provided authorisation hook: `true` when the host allows the (canonical) path to become
/// or remain a playback source. It runs while the registry lock is held, so it must not call back
/// into the registry.
pub type Authoriser = Arc<dyn Fn(&Path) -> bool + Send + Sync>;

/// Build an [`Authoriser`] that allows regular files under any of `roots`, recursively.
///
/// Roots and candidate paths are both canonicalised on every call (symlinks resolved, `..`
/// removed, and on Windows the same verbatim `\\?\` form for both), then compared component by
/// component, so `/data/projects-evil` is not under `/data/projects` and a symlink inside a root
/// that points outside it is refused. A root that does not exist allows nothing.
pub fn allowed_roots<I, P>(roots: I) -> impl Fn(&Path) -> bool + Send + Sync + 'static
where
    I: IntoIterator<Item = P>,
    P: Into<PathBuf>,
{
    let roots: Vec<PathBuf> = roots.into_iter().map(Into::into).collect();
    move |path: &Path| {
        let Ok(candidate) = path.canonicalize() else {
            return false;
        };
        roots.iter().any(|root| {
            root.canonicalize()
                .is_ok_and(|root| candidate != root && candidate.starts_with(&root))
        })
    }
}

#[derive(Debug)]
struct Entry {
    path: PathBuf,
    stamp: Stamp,
    /// Registered through the authorisation hook rather than by the host explicitly. Such an
    /// entry is re-authorised every time it is resolved, so revoking the hook revokes the id.
    conditional: bool,
}

/// Registered media sources. Owned by the host (for the Tauri helpers, by `PlaybackState`).
///
/// Invariant: each path has at most one live id. Registering a changed file replaces the old id.
#[derive(Default)]
pub struct SourceRegistry {
    entries: HashMap<SourceId, Entry>,
    by_path: HashMap<PathBuf, SourceId>,
    authoriser: Option<Authoriser>,
}

impl fmt::Debug for SourceRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SourceRegistry")
            .field("entries", &self.entries)
            .field("authoriser", &self.authoriser.is_some())
            .finish()
    }
}

impl SourceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Opt in to host-authorised sources: [`resolve_or_register`](Self::resolve_or_register) will
    /// register any file for which `authoriser` returns `true`. Without a hook (the default) the
    /// registry never registers a path on its own. Replacing the hook re-authorises every
    /// hook-registered id on its next use.
    pub fn set_authoriser(&mut self, authoriser: impl Fn(&Path) -> bool + Send + Sync + 'static) {
        self.authoriser = Some(Arc::new(authoriser));
    }

    /// Remove the hook. Ids that were registered through it stop resolving (`NotAllowed`);
    /// explicitly registered ids are unaffected.
    pub fn clear_authoriser(&mut self) {
        self.authoriser = None;
    }

    /// Whether a hook is set.
    pub fn has_authoriser(&self) -> bool {
        self.authoriser.is_some()
    }

    /// The id for `path`, registering it when the host's hook allows it.
    ///
    /// The path is canonicalised first and the hook sees only the canonical form. Refused with
    /// `NotAllowed` when no hook is set, when the path does not resolve (a path that cannot be
    /// canonicalised cannot be authorised, and this does not reveal whether it exists), or when
    /// the hook says no. Inside the allowed area, a directory or empty file is `Empty`. A path the
    /// host registered explicitly keeps its id without consulting the hook.
    pub fn resolve_or_register(
        &mut self,
        path: impl AsRef<Path>,
    ) -> Result<SourceId, PlaybackError> {
        let authoriser = self.authoriser.clone().ok_or(PlaybackError::NotAllowed)?;
        let path = path
            .as_ref()
            .canonicalize()
            .map_err(|_| PlaybackError::NotAllowed)?;
        if let Some(id) = self.by_path.get(&path) {
            if self.entries.get(id).is_some_and(|e| !e.conditional) {
                let id = id.clone();
                self.resolve(&id)?;
                return Ok(id);
            }
        }
        if !authoriser(&path) {
            return Err(PlaybackError::NotAllowed);
        }
        let stamp = stamp(&path)?;
        let id = source_id(&path, stamp);
        if let Some(entry) = self.entries.get(&id) {
            if entry.conditional || entry.path != path {
                return Ok(id);
            }
        }
        if let Some(old) = self.by_path.insert(path.clone(), id.clone()) {
            if old != id {
                self.entries.remove(&old);
            }
        }
        self.entries.insert(
            id.clone(),
            Entry {
                path,
                stamp,
                conditional: true,
            },
        );
        Ok(id)
    }

    /// Register a media file and return its id. Registering the same unchanged file again returns
    /// the same id. Fails with `Missing` for an absent path and `Empty` for an empty file or a
    /// non-file.
    pub fn register(&mut self, path: impl AsRef<Path>) -> Result<SourceId, PlaybackError> {
        let path = path
            .as_ref()
            .canonicalize()
            .map_err(|_| PlaybackError::Missing)?;
        let stamp = stamp(&path)?;
        let id = source_id(&path, stamp);
        if let Some(entry) = self.entries.get_mut(&id) {
            // The host vouching for the file explicitly outranks the hook.
            entry.conditional = false;
            return Ok(id);
        }
        if let Some(old) = self.by_path.insert(path.clone(), id.clone()) {
            self.entries.remove(&old);
        }
        self.entries.insert(
            id.clone(),
            Entry {
                path,
                stamp,
                conditional: false,
            },
        );
        Ok(id)
    }

    /// Canonical path for a live id.
    ///
    /// Unknown or revoked ids are `InvalidRequest`. If the file has since vanished or changed,
    /// the result is `Missing`: the id no longer matches the file, so register the file again.
    pub fn resolve(&self, id: &SourceId) -> Result<&Path, PlaybackError> {
        let entry = self.entries.get(id).ok_or(PlaybackError::InvalidRequest)?;
        if entry.conditional {
            // Re-authorise at use: the hook may have been revoked, and the path may now resolve
            // elsewhere (a symlink swapped in) or no longer be allowed.
            let authoriser = self.authoriser.as_ref().ok_or(PlaybackError::NotAllowed)?;
            match entry.path.canonicalize() {
                Ok(now) if now == entry.path && authoriser(&now) => {}
                Ok(_) => return Err(PlaybackError::NotAllowed),
                Err(_) => return Err(PlaybackError::Missing),
            }
        }
        let current = stamp(&entry.path)?;
        if current != entry.stamp {
            return Err(PlaybackError::Missing);
        }
        Ok(&entry.path)
    }

    /// Resolve a host-facing reference: either a [`SourceId`] string or a path that is already
    /// registered. Any other path is refused with `InvalidRequest`, even if the file exists.
    pub fn resolve_ref(&self, reference: &str) -> Result<&Path, PlaybackError> {
        if let Some(id) = SourceId::parse(reference) {
            return self.resolve(&id);
        }
        let id = self
            .by_path
            .get(&lookup_path(Path::new(reference)))
            .ok_or(PlaybackError::InvalidRequest)?;
        self.resolve(id)
    }

    /// The live id registered for `path`, if any.
    pub fn id_for_path(&self, path: impl AsRef<Path>) -> Option<SourceId> {
        self.by_path.get(&lookup_path(path.as_ref())).cloned()
    }

    /// Revoke an id. Returns `false` if it was not live. The file itself is never touched.
    pub fn revoke(&mut self, id: &SourceId) -> bool {
        match self.entries.remove(id) {
            Some(entry) => {
                self.by_path.remove(&entry.path);
                true
            }
            None => false,
        }
    }

    /// Revoke by id string or registered path. Returns `false` if nothing matched.
    pub fn revoke_ref(&mut self, reference: &str) -> bool {
        let id = match SourceId::parse(reference) {
            Some(id) => id,
            None => match self.by_path.get(&lookup_path(Path::new(reference))) {
                Some(id) => id.clone(),
                None => return false,
            },
        };
        self.revoke(&id)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

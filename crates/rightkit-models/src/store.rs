//! On-disk layout, durable state, verification, rollback and uninstall.
//!
//! ```text
//! <root>/<model-id>/state.json              active revision, history, per-revision file records
//! <root>/<model-id>/revisions/<key>/<files> one immutable dir per (revision, quant)
//! <root>/<model-id>/.staging/<key>/         resumable in-flight install (swapped in atomically)
//! <root>/<model-id>/.staging/<key>/.dl/paused.json  marker of a paused install
//! <root>/<model-id>/.aside/<key>/          previous same-key revision while a replacement commits (crash-recoverable)
//! <root>/<model-id>/.adopt/<key>/          an adoption being assembled (discarded if interrupted)
//! <root>/<model-id>.lock                    cross-process operation lock
//! <shared root>/<path>                     shared-runtime files (FileSpec::shared), outside the model dir
//! <alias root>/<model-id> -> revisions/<active key>   opt-in legacy flat alias
//! ```
//! Activation is a single atomic write of `state.json`; revision directories
//! are never edited in place, so rollback is a pointer flip.
//!
//! **Resolving a model.** Runtimes call [`Store::resolve`] (active revision,
//! size-verified) or [`Store::resolve_or_legacy`] (falls back to a pre-store
//! flat directory).
//!
//! **Legacy flat alias (opt-in).** With [`Store::with_legacy_alias_root`],
//! `<alias root>/<id>` is kept as a symlink (Windows: directory symlink, else
//! a junction) to the active revision directory. It is re-pointed on every
//! install, adopt and rollback, re-pointed or removed on revision removal, and
//! removed on uninstall and GC, so runtimes that read flat directories keep
//! working. A real file or directory already at the alias path is never
//! touched (status `skipped_real_path`); move it away (e.g. after
//! [`Store::adopt`]) and call [`Store::sync_legacy_alias`]. On unix the link
//! is replaced atomically (rename over); on Windows it is removed and
//! recreated, so a reader can briefly see no alias.
//!
//! **Per-id alias locations.** [`Store::with_legacy_alias_map`] lets the app
//! name each id's alias path (`None` falls back to the alias root, if any), so
//! aliases can sit anywhere an app's runtime expects them: a flat runtime dir
//! (`<qts dir>/<id>`) or inside the store root (`<root>/<id>/r3`). The same
//! maintenance and real-path rules apply, plus:
//! - the path must be absolute-or-relative without `.`/`..` components and lie
//!   under an allowed root: the store root, the alias root, or a root added
//!   with [`Store::with_legacy_alias_allowed_root`] (the explicit opt-in);
//! - inside the store root, an alias must sit in **its own** model directory
//!   (`<root>/<id>/<name>[/...]`), and `<name>` must not be a store-owned
//!   entry (`revisions`, `state.json`/`state.*`, or any dot-name such as
//!   `.staging`, `.aside`, `.adopt`), compared ASCII-case-insensitively; a
//!   top-level `<root>/<name>` is always refused (it is model or registry
//!   namespace). A refused path reports [`AliasStatus::Failed`] and nothing is
//!   written.
//!
//! **URL rewrite (test seam).** [`Store::with_url_rewrite`] sends every file
//! download to a loopback mock server instead of the manifest host. The crate
//! has no build-mode switch, so this is a documented test/dev seam: it is
//! explicit (never read from the environment) and refuses any non-loopback
//! base, so it cannot redirect production downloads to another host.

use crate::error::{Error, ErrorCode, Result};
use crate::hash::sha256_file;
use crate::ids::{ModelId, RevisionKey};
use crate::lock::{self, StoreLock};
use crate::spec::safe_rel_path;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileRecord {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
    /// Shared-runtime kind: `path` is relative to that shared root, not to
    /// the revision directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevisionRecord {
    pub revision: String,
    pub quant: Option<String>,
    pub installed_at_unix: u64,
    pub files: Vec<FileRecord>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    pub id: String,
    pub active: Option<String>,
    /// Previously active keys, most recent last.
    pub history: Vec<String>,
    pub revisions: BTreeMap<String, RevisionRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyMode {
    /// Exists and non-empty.
    Presence,
    /// Presence plus recorded size (fast path for every launch).
    Size,
    /// Size plus sha256 (status / repair).
    Full,
}

#[derive(Debug, Clone)]
pub struct Store {
    pub root: PathBuf,
    /// Identifies the lock holder (an app name).
    pub app: String,
    shared_roots: BTreeMap<String, PathBuf>,
    tools_root: Option<PathBuf>,
    legacy_alias_root: Option<PathBuf>,
    legacy_alias_map: Option<AliasMap>,
    alias_allowed_roots: Vec<PathBuf>,
    url_rewrite: Option<String>,
}

/// App-supplied `id -> alias path` mapping ([`Store::with_legacy_alias_map`]).
type AliasFn = dyn Fn(&str) -> Option<PathBuf> + Send + Sync;

#[derive(Clone)]
struct AliasMap(Arc<AliasFn>);

impl std::fmt::Debug for AliasMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AliasMap(..)")
    }
}

/// The active revision of a model, as runtimes should load it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedRevision {
    pub key: String,
    pub dir: PathBuf,
}

/// Outcome of keeping the legacy flat alias in step with `state.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AliasStatus {
    /// No alias root configured.
    Disabled,
    /// The alias now points at the active revision.
    Linked,
    /// It already did.
    Unchanged,
    /// No active revision; the alias link was removed (or never existed).
    Removed,
    /// A real file/directory occupies the alias path; left untouched.
    SkippedRealPath,
    /// The link could not be written; see the message.
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AliasSync {
    pub status: AliasStatus,
    pub path: Option<PathBuf>,
    pub target: Option<PathBuf>,
    pub message: Option<String>,
}

/// A paused install waiting in `.staging/<key>` (see `InstallOptions::pause`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PausedInstall {
    pub key: String,
    pub revision: String,
    pub quant: Option<String>,
    /// Bytes already on disk in staging (finished files plus `.part`s).
    pub staged_bytes: u64,
    pub paused_at_unix: u64,
}

impl Store {
    pub fn new(root: impl Into<PathBuf>, app: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            app: app.into(),
            shared_roots: BTreeMap::new(),
            tools_root: None,
            legacy_alias_root: None,
            legacy_alias_map: None,
            alias_allowed_roots: Vec::new(),
            url_rewrite: None,
        }
    }

    /// Declare where files of shared-runtime `kind` live
    /// (`FileSpec::shared`). The root is trusted app configuration; manifest
    /// paths under it are still traversal-checked.
    pub fn with_shared_root(mut self, kind: &str, root: impl Into<PathBuf>) -> Result<Self> {
        let kind = crate::spec::parse_shared_kind(kind)?;
        self.shared_roots.insert(kind, root.into());
        Ok(self)
    }

    /// Override the tools root that holds shared-runtime directories
    /// (`<tools root>/<kind>/<version>`) and their `refs` registry. Without
    /// it the root is the sibling `tools` of the store root
    /// (`<suite root>/tools` for `<suite root>/models`), see
    /// [`Store::tools_root`].
    pub fn with_tools_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.tools_root = Some(root.into());
        self
    }

    /// Tools root for shared runtimes: the [`Store::with_tools_root`] override,
    /// else `tools` next to the store root.
    pub fn tools_root(&self) -> PathBuf {
        match &self.tools_root {
            Some(root) => root.clone(),
            None => self.root.parent().unwrap_or(&self.root).join("tools"),
        }
    }

    /// `<tools root>/refs`: per-app shared-runtime reference registry.
    pub fn tool_refs_dir(&self) -> PathBuf {
        crate::suite::paths::tool_refs_dir_at(&self.tools_root())
    }

    /// Configured root for shared-runtime `kind`.
    pub fn shared_root(&self, kind: &str) -> Option<&Path> {
        self.shared_roots.get(kind).map(PathBuf::as_path)
    }

    /// Absolute location of shared file `path` of `kind`; fails for an
    /// unconfigured kind or an unsafe path.
    pub fn shared_path(&self, kind: &str, path: &str) -> Result<PathBuf> {
        let root = self.shared_root(kind).ok_or_else(|| {
            Error::new(
                ErrorCode::InvalidSpec,
                format!("shared-runtime kind {kind:?} has no configured root"),
            )
        })?;
        let rel = safe_rel_path(path).ok_or_else(|| {
            Error::new(
                ErrorCode::InvalidSpec,
                format!("unsafe shared file path {path:?}"),
            )
        })?;
        Ok(root.join(rel))
    }

    /// Opt in to the legacy flat alias `<dir>/<id>` (see the module docs).
    /// `dir` must differ from the store root, where `<root>/<id>` is the
    /// model directory itself.
    pub fn with_legacy_alias_root(mut self, dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        let same = |a: &Path, b: &Path| {
            a == b
                || matches!(
                    (fs::canonicalize(a), fs::canonicalize(b)),
                    (Ok(x), Ok(y)) if x == y
                )
        };
        if same(&dir, &self.root) {
            return Err(Error::new(
                ErrorCode::InvalidSpec,
                "legacy alias root must differ from the store root",
            ));
        }
        self.legacy_alias_root = Some(dir);
        Ok(self)
    }

    /// Per-id alias locations (see the module docs): `map(id)` names where
    /// `id`'s legacy alias lives; `None` falls back to the alias root (if
    /// configured), else no alias. Mapped paths are validated on every use.
    pub fn with_legacy_alias_map(
        mut self,
        map: impl Fn(&str) -> Option<PathBuf> + Send + Sync + 'static,
    ) -> Self {
        self.legacy_alias_map = Some(AliasMap(Arc::new(map)));
        self
    }

    /// [`Store::with_legacy_alias_map`] from a fixed `id -> path` table.
    pub fn with_legacy_alias_paths(self, paths: BTreeMap<String, PathBuf>) -> Self {
        self.with_legacy_alias_map(move |id| paths.get(id).cloned())
    }

    /// Explicitly allow mapped aliases under `dir` (besides the store root and
    /// the alias root). Repeatable.
    pub fn with_legacy_alias_allowed_root(mut self, dir: impl Into<PathBuf>) -> Self {
        self.alias_allowed_roots.push(dir.into());
        self
    }

    /// The legacy alias path for `id`, when one is configured: the mapped
    /// path (validated), else `<alias root>/<id>`. A mapped path that breaks
    /// the alias rules is an `InvalidSpec` error.
    pub fn legacy_alias_path(&self, id: &str) -> Result<Option<PathBuf>> {
        let id = ModelId::parse(id)?;
        if let Some(map) = &self.legacy_alias_map {
            if let Some(p) = (map.0)(id.as_str()) {
                self.check_alias_path(id.as_str(), &p)?;
                return Ok(Some(p));
            }
        }
        Ok(self.legacy_alias_root.as_ref().map(|r| r.join(id.as_str())))
    }

    /// Enforce the mapped-alias placement rules (module docs).
    fn check_alias_path(&self, id: &str, p: &Path) -> Result<()> {
        let refuse = |why: String| {
            Err(Error::new(
                ErrorCode::InvalidSpec,
                format!("legacy alias {} for {id}: {why}", p.display()),
            ))
        };
        if p.components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
            || p.file_name().is_none()
        {
            return refuse("path must name an entry and have no `.`/`..` components".into());
        }
        let abs = crate::alias::absolute(p);
        let root = crate::alias::absolute(&self.root);
        if let Some(rel) = under(&abs, &root) {
            return self.check_in_store(id, &rel).or_else(refuse);
        }
        let allowed = self
            .legacy_alias_root
            .iter()
            .chain(self.alias_allowed_roots.iter())
            .map(|r| crate::alias::absolute(r))
            .any(|r| under(&abs, &r).is_some_and(|rel| rel.components().next().is_some()));
        if !allowed {
            return refuse(
                "outside the store root, the alias root and every allowed root \
                 (opt in with with_legacy_alias_allowed_root)"
                    .into(),
            );
        }
        Ok(())
    }

    /// `rel` is the alias path relative to the store root.
    fn check_in_store(&self, id: &str, rel: &Path) -> std::result::Result<(), String> {
        let parts: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        if parts.len() < 2 {
            return Err("a top-level entry of the store root is model/registry namespace".into());
        }
        if parts[0] != id {
            return Err(format!(
                "inside the store root an alias must sit in its own model directory <root>/{id}/"
            ));
        }
        let name = parts[1].to_ascii_lowercase();
        if name.starts_with('.')
            || name == "revisions"
            || name == "state.json"
            || name.starts_with("state.")
            || name == "state"
        {
            return Err(format!("{:?} is store-owned revision storage", parts[1]));
        }
        Ok(())
    }

    /// Test/dev seam: rewrite every file URL's scheme + authority to `base`
    /// (keeping path and query; a path prefix on `base` is prepended), e.g.
    /// `https://hf.co/org/m/resolve/main/x.onnx` with base
    /// `http://127.0.0.1:4100` downloads `http://127.0.0.1:4100/org/m/resolve/main/x.onnx`.
    /// Refuses any base that is not `http://` on a loopback host
    /// (`127.0.0.0/8`, `localhost`, `[::1]`). Never read from the environment.
    pub fn with_url_rewrite(mut self, base: &str) -> Result<Self> {
        let bad = || {
            Error::new(
                ErrorCode::InvalidSpec,
                format!("url rewrite base {base:?} must be http:// on a loopback host"),
            )
        };
        let rest = base.strip_prefix("http://").ok_or_else(bad)?;
        let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
        let host = if let Some(v6) = authority.strip_prefix('[') {
            v6.split(']').next().unwrap_or("")
        } else {
            authority.rsplit_once(':').map_or(authority, |(h, _)| h)
        };
        let loopback = host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback());
        if authority.contains('@') || !loopback || rest[authority.len()..].contains(['?', '#']) {
            return Err(bad());
        }
        self.url_rewrite = Some(base.trim_end_matches('/').to_string());
        Ok(self)
    }

    /// The URL a file is actually fetched from (see [`Store::with_url_rewrite`]).
    pub fn download_url(&self, url: &str) -> String {
        let Some(base) = &self.url_rewrite else {
            return url.to_string();
        };
        let after_scheme = url.split_once("://").map_or(url, |(_, r)| r);
        let path = after_scheme
            .find(['/', '?', '#'])
            .map_or("/", |i| &after_scheme[i..]);
        let path = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("/{path}")
        };
        format!("{base}{path}")
    }

    /// `<root>/<id>`; fails for any id that is not a single safe path component.
    pub fn model_dir(&self, id: &str) -> Result<PathBuf> {
        Ok(self.root.join(ModelId::parse(id)?.as_str()))
    }
    pub fn revision_dir(&self, id: &str, key: &str) -> Result<PathBuf> {
        Ok(self
            .model_dir(id)?
            .join("revisions")
            .join(RevisionKey::parse(key)?.as_str()))
    }
    pub(crate) fn staging_dir(&self, id: &str, key: &str) -> Result<PathBuf> {
        Ok(self
            .model_dir(id)?
            .join(".staging")
            .join(RevisionKey::parse(key)?.as_str()))
    }
    /// Where the previous same-key revision waits while a replacement commits.
    pub(crate) fn aside_dir(&self, id: &str, key: &str) -> Result<PathBuf> {
        Ok(self
            .model_dir(id)?
            .join(".aside")
            .join(RevisionKey::parse(key)?.as_str()))
    }

    pub fn state(&self, id: &str) -> Result<State> {
        let path = self.model_dir(id)?.join("state.json");
        match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| Error::io(path.display(), e)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State {
                id: id.into(),
                ..State::default()
            }),
            Err(e) => Err(Error::io(path.display(), e)),
        }
    }

    pub(crate) fn save_state(&self, state: &State) -> Result<()> {
        self.reject_links(&state.id)?;
        let path = self.model_dir(&state.id)?.join("state.json");
        let json = serde_json::to_vec_pretty(state).map_err(|e| Error::io("serialize state", e))?;
        write_atomic(&path, &json)
    }

    /// Take the cross-process lock for `id`, then finish any interrupted
    /// same-key swap before the caller mutates anything.
    pub(crate) fn acquire(&self, id: &str, wait: Duration) -> Result<StoreLock> {
        let lock = self.acquire_raw(id, wait)?;
        self.recover_locked(id)?;
        Ok(lock)
    }

    pub(crate) fn acquire_raw(&self, id: &str, wait: Duration) -> Result<StoreLock> {
        let dir = self.model_dir(id)?;
        fs::create_dir_all(&self.root).map_err(|e| Error::io(self.root.display(), e))?;
        let lock = lock::lock_timeout(&dir, &self.app, wait)
            .map_err(|e| Error::new(ErrorCode::Io, e))?
            .ok_or_else(|| Error::new(ErrorCode::Busy, format!("another operation holds {id}")))?;
        Ok(lock)
    }

    /// Refuse to operate through symlinks planted at any directory the store
    /// owns under `<root>/<id>` (a link there would redirect writes/deletes).
    pub(crate) fn reject_links(&self, id: &str) -> Result<()> {
        let dir = self.model_dir(id)?;
        for p in [
            dir.clone(),
            dir.join("revisions"),
            dir.join(".staging"),
            dir.join(".aside"),
            dir.join(".adopt"),
            dir.join("state.json"),
        ] {
            if is_link(&p) {
                return Err(Error::new(
                    ErrorCode::Io,
                    format!("refusing to follow symlink at {}", p.display()),
                ));
            }
        }
        Ok(())
    }

    /// `<root>/refs`: the per-app reference registry shared with the suite
    /// layout ([`crate::suite::refs`]).
    pub fn refs_dir(&self) -> PathBuf {
        crate::suite::paths::model_refs_dir_at(&self.root)
    }

    /// Record that `app` uses model `id` so [`Store::gc`] and other apps'
    /// uninstalls leave it alone. Idempotent.
    pub fn register_app(&self, app: &str, id: &str) -> Result<()> {
        let id = ModelId::parse(id)?;
        crate::suite::refs::register(&self.refs_dir(), app, id.as_str())
            .map_err(|e| Error::new(ErrorCode::Io, e))
    }

    /// Drop `app`'s references; returns model ids no app references any more.
    /// A corrupt ref file aborts without changing anything.
    /// Also drops `app`'s shared-runtime references
    /// ([`Store::unregister_shared`]); the returned ids are model ids only.
    pub fn unregister_app(&self, app: &str) -> Result<Vec<String>> {
        let unreferenced = crate::suite::refs::unregister(&self.refs_dir(), app)
            .map_err(|e| Error::new(ErrorCode::Io, e))?;
        self.unregister_shared(app)?;
        Ok(unreferenced)
    }

    /// Record that `app` uses shared runtime `kind` at `version` (the files
    /// under `<tools root>/<kind>/<version>`) so [`Store::gc_shared`] keeps
    /// it. `kind` and `version` must be single safe path segments. Idempotent.
    pub fn register_shared(&self, app: &str, kind: &str, version: &str) -> Result<()> {
        let (kind, version) = crate::suite::paths::parse_shared_runtime(kind, version)?;
        crate::suite::refs::register(&self.tool_refs_dir(), app, &format!("{kind}@{version}"))
            .map_err(|e| Error::new(ErrorCode::Io, e))
    }

    /// Drop `app`'s shared-runtime references; returns the `(kind, version)`
    /// pairs no app references any more (their files stay until
    /// [`Store::gc_shared`]). A corrupt ref file aborts without changing
    /// anything. A no-op when the tools registry does not exist.
    pub fn unregister_shared(&self, app: &str) -> Result<Vec<(String, String)>> {
        let refs_dir = self.tool_refs_dir();
        if !refs_dir.is_dir() {
            return Ok(Vec::new());
        }
        let ids = crate::suite::refs::unregister(&refs_dir, app)
            .map_err(|e| Error::new(ErrorCode::Io, e))?;
        Ok(ids
            .iter()
            .filter_map(|id| id.split_once('@'))
            .map(|(kind, version)| (kind.to_string(), version.to_string()))
            .collect())
    }

    /// Remove `<tools root>/<kind>/<version>` directories that no app
    /// references, considering only the listed `kinds` (a tool kind never
    /// registered, such as one installed by an app that does not use refs, is
    /// never touched; an empty list removes nothing). Runs under the registry
    /// lock and returns the removed `(kind, version)` pairs. Symlinks are never
    /// followed (a link is skipped), `refs` is never touched, and a corrupt
    /// ref blocks deletion (nothing is removed). Entries whose names are not
    /// safe segments are left alone; an invalid kind is `InvalidSpec`.
    pub fn gc_shared(&self, kinds: &[&str]) -> Result<Vec<(String, String)>> {
        let mut kinds = kinds
            .iter()
            .map(|kind| crate::spec::parse_shared_kind(kind))
            .collect::<Result<Vec<_>>>()?;
        kinds.sort();
        kinds.dedup();
        let tools = self.tools_root();
        if kinds.is_empty() || !tools.is_dir() {
            return Ok(Vec::new());
        }
        let refs_dir = self.tool_refs_dir();
        let _guard = crate::suite::refs::acquire_registry(&refs_dir)
            .map_err(|e| Error::new(ErrorCode::Io, e))?;
        let referenced = crate::suite::refs::referenced_ids_locked(&refs_dir)
            .map_err(|e| Error::new(ErrorCode::Io, e))?;
        let real_dir = |p: &Path| fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_dir());
        let mut removed = Vec::new();
        for kind in kinds {
            let kind_dir = tools.join(&kind);
            if !real_dir(&kind_dir) {
                continue;
            }
            for version_entry in
                fs::read_dir(&kind_dir).map_err(|e| Error::io(kind_dir.display(), e))?
            {
                let version_dir = version_entry
                    .map_err(|e| Error::io(kind_dir.display(), e))?
                    .path();
                let Some(version) = version_dir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(String::from)
                else {
                    continue;
                };
                if ModelId::parse(&version).is_err()
                    || !real_dir(&version_dir)
                    || referenced.contains(&format!("{kind}@{version}"))
                {
                    continue;
                }
                remove_tree(&version_dir)?;
                removed.push((kind.clone(), version));
            }
            // Drop the kind directory once empty; a non-empty one stays.
            let _ = fs::remove_dir(&kind_dir);
        }
        removed.sort();
        Ok(removed)
    }

    /// Remove every model no app references, skipping `keep`, anything under a
    /// live install lock, and everything while any runtime-family lease is
    /// held. A corrupt ref blocks deletion (nothing is removed).
    pub fn gc(&self, keep: &[&str]) -> Result<Vec<String>> {
        let removed = crate::suite::store::prune_unref_entries_with(&self.root, keep, &[])
            .map_err(|e| Error::new(ErrorCode::Io, e))?;
        for id in &removed {
            if let Ok(Some(alias)) = self.legacy_alias_path(id) {
                let _ = crate::alias::remove(&alias);
            }
        }
        Ok(removed)
    }

    /// As [`Store::uninstall`], but refuses with `Busy` while any app still
    /// references the model.
    pub fn uninstall_unreferenced(&self, id: &str) -> Result<()> {
        let id = ModelId::parse(id)?;
        let _guard = crate::suite::refs::acquire_registry(&self.refs_dir())
            .map_err(|e| Error::new(ErrorCode::Io, e))?;
        let referenced = crate::suite::refs::referenced_ids_locked(&self.refs_dir())
            .map_err(|e| Error::new(ErrorCode::Io, e))?;
        if referenced.contains(id.as_str()) {
            return Err(Error::new(
                ErrorCode::Busy,
                format!("{id} is referenced by another app"),
            ));
        }
        self.uninstall(id.as_str())
    }

    /// Public entry for crash recovery: finishes an interrupted same-key swap
    /// for `id` (see [`Store::install`]). Safe to call at startup.
    pub fn recover(&self, id: &str) -> Result<bool> {
        let _lock = self.acquire_raw(id, Duration::ZERO)?;
        self.recover_locked(id)
    }

    /// True when every recorded file of `key` passes `mode`.
    pub fn verify(&self, id: &str, key: &str, mode: VerifyMode) -> bool {
        let Ok(state) = self.state(id) else {
            return false;
        };
        let Ok(dir) = self.revision_dir(id, key) else {
            return false;
        };
        let Some(rec) = state.revisions.get(key) else {
            return false;
        };
        self.record_matches(rec, &dir, mode)
    }

    /// Do the files recorded in `rec` exist (under `dir`, or their shared
    /// root) and pass `mode`? A shared file whose kind has no configured root
    /// fails (closed).
    pub(crate) fn record_matches(
        &self,
        rec: &RevisionRecord,
        dir: &Path,
        mode: VerifyMode,
    ) -> bool {
        !rec.files.is_empty()
            && rec.files.iter().all(|f| {
                let p = match &f.shared {
                    Some(kind) => match self.shared_path(kind, &f.path) {
                        Ok(p) => p,
                        Err(_) => return false,
                    },
                    None => match safe_rel_path(&f.path) {
                        Some(rel) => dir.join(rel),
                        None => return false,
                    },
                };
                file_matches(&p, f, mode)
            })
    }

    /// Directory of the active revision if it passes a size check.
    pub fn active_dir(&self, id: &str) -> Option<PathBuf> {
        self.resolve(id).map(|r| r.dir)
    }

    /// The active revision (key + directory) if it passes a size check:
    /// what a runtime should load for `id`.
    pub fn resolve(&self, id: &str) -> Option<ResolvedRevision> {
        let key = self.state(id).ok()?.active?;
        if self.reject_links(id).is_err() {
            return None;
        }
        if !self.verify(id, &key, VerifyMode::Size) {
            return None;
        }
        let dir = self.revision_dir(id, &key).ok()?;
        Some(ResolvedRevision { key, dir })
    }

    /// [`Store::resolve`], else `legacy` when it is an existing directory
    /// (a pre-store flat install the runtime can still read).
    pub fn resolve_or_legacy(&self, id: &str, legacy: &Path) -> Option<PathBuf> {
        self.active_dir(id)
            .or_else(|| legacy.is_dir().then(|| legacy.to_path_buf()))
    }

    /// Re-point (or remove) the legacy flat alias to match `state.json`.
    pub fn sync_legacy_alias(&self, id: &str) -> Result<AliasSync> {
        let _lock = self.acquire(id, Duration::ZERO)?;
        let state = self.state(id)?;
        Ok(self.sync_alias_locked(id, &state))
    }

    pub(crate) fn sync_alias_locked(&self, id: &str, state: &State) -> AliasSync {
        let path = match self.legacy_alias_path(id) {
            Ok(Some(p)) => p,
            Ok(None) => {
                return AliasSync {
                    status: AliasStatus::Disabled,
                    path: None,
                    target: None,
                    message: None,
                }
            }
            Err(e) => {
                return AliasSync {
                    status: AliasStatus::Failed,
                    path: None,
                    target: None,
                    message: Some(e.to_string()),
                }
            }
        };
        let target = state
            .active
            .as_deref()
            .filter(|k| self.verify_record(state, id, k))
            .and_then(|k| self.revision_dir(id, k).ok())
            .map(|d| crate::alias::absolute(&d));
        let result = match &target {
            Some(t) => crate::alias::point(&path, t),
            None => crate::alias::remove(&path),
        };
        let (status, message) = match result {
            Ok(s) => (s, None),
            Err(e) => (AliasStatus::Failed, Some(e)),
        };
        AliasSync {
            status,
            path: Some(path),
            target,
            message,
        }
    }

    /// Installs paused with `InstallOptions::pause`, resumable by installing
    /// the same revision again.
    pub fn paused(&self, id: &str) -> Result<Vec<PausedInstall>> {
        let root = self.model_dir(id)?.join(".staging");
        let mut out = Vec::new();
        let Ok(entries) = fs::read_dir(&root) else {
            return Ok(out);
        };
        for entry in entries.flatten() {
            let marker = entry.path().join(".dl").join("paused.json");
            if let Some(p) = fs::read(&marker)
                .ok()
                .and_then(|b| serde_json::from_slice::<PausedInstall>(&b).ok())
            {
                out.push(p);
            }
        }
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }

    /// Cancel a paused (or interrupted) install: delete its staging,
    /// including `.part` files. Installed revisions are untouched.
    pub fn discard_staging(&self, id: &str, key: &str) -> Result<()> {
        let key = RevisionKey::parse(key)?;
        let _lock = self.acquire(id, Duration::ZERO)?;
        self.reject_links(id)?;
        remove_tree(&self.staging_dir(id, key.as_str())?)
    }

    /// Re-point `active` at an earlier revision (default: the most recent
    /// history entry that still verifies). Returns the new active key.
    pub fn rollback(&self, id: &str, target: Option<&str>) -> Result<String> {
        let _lock = self.acquire(id, Duration::ZERO)?;
        self.reject_links(id)?;
        let mut state = self.state(id)?;
        let current = state.active.clone();
        let target = match target {
            Some(t) => RevisionKey::parse(t)?.as_str().to_string(),
            None => state
                .history
                .iter()
                .rev()
                .find(|k| Some(*k) != current.as_ref() && self.verify(id, k, VerifyMode::Size))
                .cloned()
                .ok_or_else(|| Error::new(ErrorCode::NoRollback, "no earlier verified revision"))?,
        };
        if !self.verify(id, &target, VerifyMode::Size) {
            return Err(Error::new(
                ErrorCode::NoRollback,
                format!("revision {target} is not installed or fails verification"),
            ));
        }
        if let Some(c) = current.filter(|c| *c != target) {
            state.history.retain(|k| *k != target);
            state.history.push(c);
        }
        state.active = Some(target.clone());
        self.save_state(&state)?;
        self.sync_alias_locked(id, &state);
        Ok(target)
    }

    /// Remove one revision. If it was active, fall back to the newest
    /// verified history entry (or none).
    pub fn uninstall_revision(&self, id: &str, key: &str) -> Result<()> {
        let key = RevisionKey::parse(key)?.as_str().to_string();
        let key = key.as_str();
        let _lock = self.acquire(id, Duration::ZERO)?;
        self.reject_links(id)?;
        let mut state = self.state(id)?;
        if state.revisions.remove(key).is_none() {
            return Err(Error::new(
                ErrorCode::NotFound,
                format!("{id} has no revision {key}"),
            ));
        }
        state.history.retain(|k| k != key);
        if state.active.as_deref() == Some(key) {
            state.active = state
                .history
                .iter()
                .rev()
                .find(|k| self.verify_record(&state, id, k))
                .cloned();
            if let Some(a) = &state.active {
                state.history.retain(|k| k != a);
            }
        }
        self.save_state(&state)?;
        self.sync_alias_locked(id, &state);
        remove_tree(&self.revision_dir(id, key)?)?;
        let _ = remove_tree(&self.staging_dir(id, key)?);
        Ok(())
    }

    fn verify_record(&self, state: &State, id: &str, key: &str) -> bool {
        let Ok(dir) = self.revision_dir(id, key) else {
            return false;
        };
        state
            .revisions
            .get(key)
            .is_some_and(|r| self.record_matches(r, &dir, VerifyMode::Size))
    }

    /// Remove the whole model: every revision, staging, and state, plus the
    /// legacy alias link. A symlink at `<root>/<id>` is unlinked, never
    /// followed. Shared-runtime files stay (other models may load them).
    pub fn uninstall(&self, id: &str) -> Result<()> {
        let _lock = self.acquire_raw(id, Duration::ZERO)?;
        if let Some(alias) = self.legacy_alias_path(id)? {
            crate::alias::remove(&alias).map_err(|e| Error::new(ErrorCode::Io, e))?;
        }
        remove_tree(&self.model_dir(id)?)
    }

    /// Finish an interrupted same-key swap (see `install`): the previous
    /// revision directory waits in `.aside/<key>` while the replacement commits.
    /// - `revisions/<key>` missing: the swap died between the two renames; put
    ///   the previous revision back.
    /// - present and matching the recorded files: the swap committed; drop the aside.
    /// - present but not matching while the aside does: the replacement was
    ///   renamed in but never recorded; restore the aside the ledger still names.
    ///
    /// Caller holds the model lock. Returns whether anything was restored.
    pub(crate) fn recover_locked(&self, id: &str) -> Result<bool> {
        self.reject_links(id)?;
        // An adoption is assembled entirely under the lock; leftovers are
        // from a killed process and were never recorded.
        remove_tree(&self.model_dir(id)?.join(".adopt"))?;
        let aside_root = self.model_dir(id)?.join(".aside");
        let Ok(entries) = fs::read_dir(&aside_root) else {
            return Ok(false);
        };
        let state = self.state(id)?;
        let mut restored = false;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(key) = name.to_str().and_then(|n| RevisionKey::parse(n).ok()) else {
                let _ = remove_tree(&entry.path());
                continue;
            };
            let aside = entry.path();
            let dest = self.revision_dir(id, key.as_str())?;
            let rec = state.revisions.get(key.as_str());
            let restore = if !dest.exists() {
                true
            } else {
                let dest_ok = rec.is_some_and(|r| self.record_matches(r, &dest, VerifyMode::Size));
                let aside_ok =
                    rec.is_some_and(|r| self.record_matches(r, &aside, VerifyMode::Size));
                !dest_ok && aside_ok
            };
            if restore {
                remove_tree(&dest)?;
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent).map_err(|e| Error::io(parent.display(), e))?;
                }
                fs::rename(&aside, &dest)
                    .map_err(|e| Error::io(format!("restore {}", dest.display()), e))?;
                restored = true;
            } else {
                remove_tree(&aside)?;
            }
        }
        let _ = fs::remove_dir(&aside_root);
        Ok(restored)
    }
}

/// `p` relative to `root` when `p` is lexically under it and, where an
/// ancestor of `p` exists, its resolved location is under resolved `root`
/// too (a symlinked parent cannot smuggle an alias elsewhere).
fn under(p: &Path, root: &Path) -> Option<PathBuf> {
    let rel = p.strip_prefix(root).ok()?.to_path_buf();
    let mut anc = p.parent();
    while let Some(a) = anc.filter(|a| a.starts_with(root)) {
        if let Ok(real) = fs::canonicalize(a) {
            let real_root = fs::canonicalize(root).ok()?;
            return real.starts_with(&real_root).then_some(rel);
        }
        anc = a.parent();
    }
    Some(rel)
}

pub(crate) fn is_link(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink())
}

/// Delete `path` without ever following a symlink: a link (at the root of the
/// removal) is unlinked; real trees go through `remove_dir_all`, which does
/// not traverse links either. Missing paths are fine.
pub(crate) fn remove_tree(path: &Path) -> Result<()> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(Error::io(path.display(), e)),
    };
    let result = if meta.file_type().is_symlink() {
        fs::remove_file(path).or_else(|_| fs::remove_dir(path))
    } else if meta.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    result.map_err(|e| Error::io(path.display(), e))
}

fn file_matches(p: &Path, f: &FileRecord, mode: VerifyMode) -> bool {
    let Ok(meta) = fs::metadata(p) else {
        return false;
    };
    if !meta.is_file() || meta.len() == 0 {
        return false;
    }
    if mode != VerifyMode::Presence && meta.len() != f.bytes {
        return false;
    }
    mode != VerifyMode::Full || sha256_file(p).is_ok_and(|h| h.eq_ignore_ascii_case(&f.sha256))
}

/// Durable replacement: temp file beside the target, fsync, atomic rename over it (also on
/// Windows: an existing target is replaced in one step, never deleted first), directory fsync.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::new(ErrorCode::Io, "atomic write needs a parent"))?;
    fs::create_dir_all(parent).map_err(|e| Error::io(parent.display(), e))?;
    rightkit_fs::atomic_write(path, bytes).map_err(|e| Error::io(path.display(), e))
}

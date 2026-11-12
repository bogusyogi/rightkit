//! `Store`: the manifest-driven installer API over the one on-disk layout, the suite layout
//! ([`crate::suite`]). Since 0.7.0 there is no second format: a model `Store` installs is an
//! ordinary suite entry, byte-compatible with what `suite::ensure_ready` writes.
//!
//! ```text
//! <root>/<model-id>/<files>                  the installed model (one revision per id)
//! <root>/<model-id>/manifest.json            suite manifest: id, version = revision key, files + sha256 + size
//! <root>/<model-id>.lock                     cross-process operation lock (the suite entry lock)
//! <root>/.<model-id>.previous-aside/         previous entry while a replacement commits (suite archive name)
//! <root>/.incoming/<model-id>.staging/<key>/ resumable in-flight install; `.dl/paused.json` marks a pause
//! <root>/.incoming/<model-id>.adopt/<key>/   an adoption being assembled (discarded if interrupted)
//! <root>/refs/<app>.json                     per-app reference registry (suite refs)
//! <root>/.legacy-kept/<model-id>/            an old-layout directory a migration replaced (kept)
//! <shared root>/<path>                       shared-runtime files (FileSpec::shared), outside the entry
//! <alias path> -> <root>/<model-id>          opt-in legacy flat alias
//! ```
//! Activation is the rename of a complete staging directory, manifest included, onto
//! `<root>/<id>`; the previous entry waits at the suite's deterministic aside path until the
//! rename lands, and [`crate::suite::archive::recover_orphaned_install`] (run under every lock
//! acquisition) puts it back after a crash in between.
//!
//! **One revision per model.** The suite layout keeps one revision of each id. Installing another
//! revision replaces the entry; [`Store::rollback`] has nothing to roll back to and
//! `InstallOptions::keep_revisions` is ignored. [`Store::state`] reports the installed revision as
//! `active` with an empty `history`.
//!
//! **Earlier layout (0.6 and older).** An entry written by the revisioned store
//! (`<root>/<id>/state.json` + `revisions/<key>/`) is still read as it is, and is migrated to the
//! suite layout the first time the store takes that model's lock ([`Store::recover`],
//! [`Store::resolve`] and every install, adopt, rollback or discard): the active revision's
//! directory becomes the entry, its record becomes `manifest.json`, and the rest of the old
//! directory (earlier revisions, paused installs, `state.json`) is kept, never deleted, under
//! `<root>/.legacy-kept/<id>` ([`Store::legacy_kept`] lists them). See `legacy_layout.rs`.
//!
//! **Replacing a model in use.** Installing over an existing entry refuses with
//! [`ErrorCode::Busy`] ("in use") and leaves the entry exactly as it was in two cases. In the
//! suite store (a store whose parent folder has `runtime/model-use/`, [`crate::suite::lease`]) it
//! first takes every runtime-family lease, as GC does, and refuses while a running app holds one.
//! In every store, on Windows, a program that has the installed files open stops the entry being
//! moved aside; that is reported as in use too. A store outside the suite has no leases, and none
//! are created beside it.
//!
//! **Resolving a model.** Runtimes call [`Store::resolve`] (installed revision,
//! size-verified) or [`Store::resolve_or_legacy`] (falls back to a pre-store
//! flat directory).
//!
//! **Legacy flat alias (opt-in).** With [`Store::with_legacy_alias_root`],
//! `<alias root>/<id>` is kept as a symlink (Windows: directory symlink, else
//! a junction) to the entry directory. It is re-pointed on every install and
//! adopt, and removed on uninstall and GC, so runtimes that read flat directories keep
//! working. A real file or directory already at the alias path is never
//! touched (status `skipped_real_path`); move it away (e.g. after
//! [`Store::adopt`]) and call [`Store::sync_legacy_alias`]. On unix the link
//! is replaced atomically (rename over); on Windows it is removed and
//! recreated, so a reader can briefly see no alias.
//!
//! **Per-id alias locations.** [`Store::with_legacy_alias_map`] lets the app
//! name each id's alias path (`None` falls back to the alias root, if any). The path must have no
//! `.`/`..` components and lie under an allowed root: the store root, the alias root, or a root
//! added with [`Store::with_legacy_alias_allowed_root`] (the explicit opt-in). Inside the store root
//! an alias must sit in **its own** model directory (`<root>/<id>/<name>`, `<name>` not a dot-name,
//! `manifest.json`, `revisions` or `state.*`); since the entry directory is already the flat
//! directory, such an alias is accepted but never written (status `disabled`, with a message). A
//! refused path reports [`AliasStatus::Failed`] and nothing is written.
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

/// What is installed for one model ([`Store::state`]). Since 0.7.0 it is read from the suite
/// entry's `manifest.json`: `active` is the installed revision key, `revisions` holds its record
/// alone and `history` is empty (pre-0.7 entries are read from their `state.json` until migrated).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    pub id: String,
    pub active: Option<String>,
    /// Previously active keys, most recent last (always empty for a suite entry).
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
    address_policy: Option<rightkit_http::AddressPolicy>,
    http_options: Option<crate::http::ModelHttpOptions>,
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

/// Outcome of keeping the legacy flat alias in step with the installed entry.
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
            address_policy: None,
            http_options: None,
        }
    }

    /// Proxy, user agent and timeouts for this store's downloads (see
    /// [`crate::http::ModelHttpOptions`]). `max_redirects` is taken from
    /// `InstallOptions::max_download_redirects`; an address policy set with
    /// [`Store::with_address_policy`] wins over the one in `options`.
    pub fn with_http_options(mut self, options: crate::http::ModelHttpOptions) -> Self {
        self.http_options = Some(options);
        self
    }

    /// Replace the outbound address policy for this store's downloads. The default
    /// is public-web: loopback, private, link-local and metadata destinations are
    /// refused on the first request and on every redirect hop. Use this to reach a
    /// private mirror the app lists explicitly, for example
    /// `AddressPolicy::public_web().allow_if(vec!["models.corp".into()], |ip| ...)`.
    pub fn with_address_policy(mut self, policy: rightkit_http::AddressPolicy) -> Self {
        self.address_policy = Some(policy);
        self
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
            || name == "manifest.json"
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

    /// HTTP client for this store's downloads: public-web address policy, except
    /// that a store with a loopback URL rewrite ([`Store::with_url_rewrite`]) may
    /// also reach that one loopback host, which is what the rewrite is for.
    pub(crate) fn download_client(&self, max_redirects: u32) -> crate::http::ModelHttp {
        let mut options = self.http_options.clone().unwrap_or_default();
        options.max_redirects = max_redirects;
        let mut policy = self
            .address_policy
            .clone()
            .unwrap_or_else(|| options.address_policy.clone());
        if let Some(base) = &self.url_rewrite {
            let authority = base
                .strip_prefix("http://")
                .and_then(|rest| rest.split(['/', '?', '#']).next())
                .unwrap_or("");
            let host = match authority.strip_prefix('[') {
                Some(v6) => v6.split(']').next().unwrap_or("").to_string(),
                None => authority
                    .rsplit_once(':')
                    .map_or(authority, |(h, _)| h)
                    .to_string(),
            };
            if !host.is_empty() {
                policy = policy.allow_if(vec![host], |ip| ip.is_loopback());
            }
        }
        options.address_policy = policy;
        crate::http::http_client_with_options(options)
    }

    /// `<root>/<id>`, the model's suite entry directory; fails for any id that is not a single
    /// safe path component.
    pub fn model_dir(&self, id: &str) -> Result<PathBuf> {
        Ok(self.root.join(ModelId::parse(id)?.as_str()))
    }

    /// Where revision `key` of `id` is installed. The suite layout keeps one revision per model
    /// in the entry directory itself, so this is [`Store::model_dir`] for every valid `key` (the
    /// key is still validated). Before 0.7.0 it was `<root>/<id>/revisions/<key>`.
    pub fn revision_dir(&self, id: &str, key: &str) -> Result<PathBuf> {
        RevisionKey::parse(key)?;
        self.model_dir(id)
    }

    /// `<root>/.incoming`: the suite layout's resumable download scratch.
    fn incoming_dir(&self) -> PathBuf {
        self.root.join(".incoming")
    }

    /// `<root>/.incoming/<id>.staging`: every resumable install of `id`.
    pub(crate) fn staging_root(&self, id: &str) -> Result<PathBuf> {
        Ok(self
            .incoming_dir()
            .join(format!("{}.staging", ModelId::parse(id)?.as_str())))
    }

    pub(crate) fn staging_dir(&self, id: &str, key: &str) -> Result<PathBuf> {
        Ok(self
            .staging_root(id)?
            .join(RevisionKey::parse(key)?.as_str()))
    }

    /// `<root>/.incoming/<id>.adopt`: adoptions being assembled under the lock.
    pub(crate) fn adopt_root(&self, id: &str) -> Result<PathBuf> {
        Ok(self
            .incoming_dir()
            .join(format!("{}.adopt", ModelId::parse(id)?.as_str())))
    }

    /// Where the previous entry waits while a replacement commits: the suite archive install's
    /// deterministic aside path, so either side recovers the other's interrupted swap.
    pub(crate) fn aside_dir(&self, id: &str) -> Result<PathBuf> {
        Ok(crate::suite::archive::previous_aside_path(
            &self.root,
            ModelId::parse(id)?.as_str(),
        ))
    }

    /// What is installed for `id`: the entry's `manifest.json` as a [`State`] whose `active` is
    /// the manifest's revision key (`history` is always empty: one revision per model). An entry
    /// still in the pre-0.7 revisioned layout is read from its `state.json` until it is migrated.
    /// Nothing installed is an empty state.
    pub fn state(&self, id: &str) -> Result<State> {
        let dir = self.model_dir(id)?;
        match crate::suite::manifest::read_manifest(&dir) {
            Ok(Some(manifest)) => Ok(state_from_manifest(id, &dir, &manifest)),
            Ok(None) => crate::legacy_layout::read_state(&dir, id),
            Err(e) => Err(Error::new(ErrorCode::Io, e)),
        }
    }

    /// Directory holding the files of installed revision `key`: the entry, or, for an entry not
    /// yet migrated from the pre-0.7 layout, its `revisions/<key>`.
    pub(crate) fn installed_dir(&self, id: &str, key: &str) -> Result<PathBuf> {
        let dir = self.model_dir(id)?;
        let key = RevisionKey::parse(key)?;
        if crate::legacy_layout::is_legacy(&dir) {
            return Ok(crate::legacy_layout::revision_dir(&dir, key.as_str()));
        }
        Ok(dir)
    }

    /// Take the cross-process lock for `id`, then finish any interrupted swap and migrate a
    /// pre-0.7 entry before the caller mutates anything.
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

    /// Refuse to operate through symlinks planted at any path the store owns for `id` (a link
    /// there would redirect writes/deletes).
    pub(crate) fn reject_links(&self, id: &str) -> Result<()> {
        let dir = self.model_dir(id)?;
        let mut owned = vec![
            dir.clone(),
            crate::suite::paths::manifest_path(&dir),
            self.incoming_dir(),
            self.staging_root(id)?,
            self.adopt_root(id)?,
            self.aside_dir(id)?,
        ];
        owned.extend(crate::legacy_layout::owned_paths(&dir));
        owned.push(self.root.join(crate::legacy_layout::KEPT_DIR));
        for p in owned {
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

    /// Release `app`'s references to the listed model `ids` only (its other references stay) and
    /// return the listed ids no app references any more, sorted. Idempotent: an id the app does
    /// not hold, or one already released, changes nothing. An invalid id is `InvalidSpec` and a
    /// corrupt ref file is `Io`; neither changes anything. Nothing is deleted: removing an
    /// unreferenced model is [`Store::gc`] or [`Store::uninstall_unreferenced`].
    pub fn release_app_model(&self, app: &str, ids: &[&str]) -> Result<Vec<String>> {
        for id in ids {
            ModelId::parse(id)?;
        }
        crate::suite::refs::release(&self.refs_dir(), app, ids)
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

    /// Install shared runtime `kind` at `version` IN PLACE at the one fixed path
    /// `<tools root>/<kind>/current` (see [`crate::suite::paths::shared_runtime_current_dir_at`]) and
    /// return it; load the runtime from there, never from `<kind>/<version>`. The version's files are
    /// copied to `current.staging` (nothing runs from it), the old `current` is renamed to the single
    /// kept `current.previous` (replacing any older one; rename it back to roll back), and the staging
    /// copy is renamed into place. A failure renames `current.previous` back. Idempotent: when
    /// `current` already holds identical files nothing is touched. The version directory must exist
    /// (installed by [`Store::install`] and friends). A platform that still has the runtime loaded
    /// from `current` can refuse the rename; that is returned as `Io` with `current` unchanged.
    pub fn activate_shared_runtime(&self, kind: &str, version: &str) -> Result<PathBuf> {
        let (kind, version) = crate::suite::paths::parse_shared_runtime(kind, version)?;
        let tools = self.tools_root();
        let source = crate::suite::paths::tool_dir_at(&tools, &kind, &version);
        if !fs::symlink_metadata(&source).is_ok_and(|m| m.file_type().is_dir()) {
            return Err(Error::new(
                ErrorCode::NotFound,
                format!(
                    "shared runtime {kind}@{version} is not installed at {}",
                    source.display()
                ),
            ));
        }
        let kind_dir = tools.join(&kind);
        let current = kind_dir.join(crate::suite::paths::SHARED_RUNTIME_CURRENT);
        let previous = kind_dir.join(crate::suite::paths::SHARED_RUNTIME_PREVIOUS);
        let staging = kind_dir.join(crate::suite::paths::SHARED_RUNTIME_STAGING);
        if dirs_identical(&source, &current)? {
            return Ok(current);
        }
        remove_tree(&staging)?;
        copy_tree(&source, &staging)?;
        let moved_aside = fs::symlink_metadata(&current).is_ok();
        let swapped = (|| -> Result<()> {
            if moved_aside {
                remove_tree(&previous)?;
                fs::rename(&current, &previous).map_err(|e| Error::io(current.display(), e))?;
            }
            fs::rename(&staging, &current).map_err(|e| Error::io(current.display(), e))
        })();
        if let Err(error) = swapped {
            if moved_aside && fs::symlink_metadata(&current).is_err() && previous.exists() {
                let _ = fs::rename(&previous, &current);
            }
            let _ = remove_tree(&staging);
            return Err(error);
        }
        Ok(current)
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
                if crate::suite::paths::is_shared_runtime_fixed_name(&version)
                    || ModelId::parse(&version).is_err()
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
    /// held. A corrupt ref blocks deletion (nothing is removed). This is the suite GC
    /// ([`crate::suite::store::prune_unref_entries_with`]); the removed ids' resumable staging
    /// and legacy aliases go with them.
    pub fn gc(&self, keep: &[&str]) -> Result<Vec<String>> {
        let removed = crate::suite::store::prune_unref_entries_with(&self.root, keep, &[])
            .map_err(|e| Error::new(ErrorCode::Io, e))?;
        for id in &removed {
            if let Ok(staging) = self.staging_root(id) {
                let _ = remove_tree(&staging);
            }
            if let Ok(adopt) = self.adopt_root(id) {
                let _ = remove_tree(&adopt);
            }
            self.remove_alias(id);
        }
        let _ = fs::remove_dir(self.incoming_dir());
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

    /// Crash recovery and migration for `id`, safe to call at startup: finishes an interrupted
    /// swap (see [`Store::install`]), discards an interrupted adoption, and migrates an entry
    /// still in the pre-0.7 revisioned layout. Returns whether anything was restored or migrated.
    pub fn recover(&self, id: &str) -> Result<bool> {
        let _lock = self.acquire_raw(id, Duration::ZERO)?;
        self.recover_locked(id)
    }

    /// True when `key` is the installed revision of `id` and every recorded file passes `mode`.
    pub fn verify(&self, id: &str, key: &str, mode: VerifyMode) -> bool {
        let Ok(state) = self.state(id) else {
            return false;
        };
        let Ok(dir) = self.installed_dir(id, key) else {
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

    /// Directory of the installed revision if it passes a size check.
    pub fn active_dir(&self, id: &str) -> Option<PathBuf> {
        self.resolve(id).map(|r| r.dir)
    }

    /// The installed revision (key + directory) if it passes a size check:
    /// what a runtime should load for `id`. An entry in the pre-0.7 layout is migrated first
    /// when its lock is free; when it is not (or the migration fails), the old layout is read
    /// in place.
    pub fn resolve(&self, id: &str) -> Option<ResolvedRevision> {
        let dir = self.model_dir(id).ok()?;
        if crate::legacy_layout::is_legacy(&dir) {
            let _ = self.recover(id);
        }
        let state = self.state(id).ok()?;
        let key = state.active?;
        if self.reject_links(id).is_err() {
            return None;
        }
        if !self.verify(id, &key, VerifyMode::Size) {
            return None;
        }
        let dir = self.installed_dir(id, &key).ok()?;
        Some(ResolvedRevision { key, dir })
    }

    /// [`Store::resolve`], else `legacy` when it is an existing directory
    /// (a pre-store flat install the runtime can still read).
    pub fn resolve_or_legacy(&self, id: &str, legacy: &Path) -> Option<PathBuf> {
        self.active_dir(id)
            .or_else(|| legacy.is_dir().then(|| legacy.to_path_buf()))
    }

    /// Re-point (or remove) the legacy flat alias to match the installed entry.
    pub fn sync_legacy_alias(&self, id: &str) -> Result<AliasSync> {
        let _lock = self.acquire(id, Duration::ZERO)?;
        let state = self.state(id)?;
        Ok(self.sync_alias_locked(id, &state))
    }

    /// Whether `path` lies inside the store root (where the entry is already the flat directory).
    fn alias_in_store(&self, path: &Path) -> bool {
        under(
            &crate::alias::absolute(path),
            &crate::alias::absolute(&self.root),
        )
        .is_some()
    }

    /// Remove `id`'s alias link, if one is configured outside the store root. Best effort.
    fn remove_alias(&self, id: &str) {
        if let Ok(Some(alias)) = self.legacy_alias_path(id) {
            if !self.alias_in_store(&alias) {
                let _ = crate::alias::remove(&alias);
            }
        }
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
        if self.alias_in_store(&path) {
            return AliasSync {
                status: AliasStatus::Disabled,
                path: Some(path),
                target: None,
                message: Some(
                    "inside the store root the entry directory is already the flat directory; \
                     no alias is written"
                        .into(),
                ),
            };
        }
        let target = state
            .active
            .as_deref()
            .filter(|k| self.verify(id, k, VerifyMode::Size))
            .and_then(|k| self.installed_dir(id, k).ok())
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
    /// the same revision again. (Paused installs of the pre-0.7 layout are not listed; its
    /// migration discards them.)
    pub fn paused(&self, id: &str) -> Result<Vec<PausedInstall>> {
        let root = self.staging_root(id)?;
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
    /// including `.part` files. The installed entry is untouched.
    pub fn discard_staging(&self, id: &str, key: &str) -> Result<()> {
        let key = RevisionKey::parse(key)?;
        let _lock = self.acquire(id, Duration::ZERO)?;
        self.reject_links(id)?;
        remove_tree(&self.staging_dir(id, key.as_str())?)?;
        self.tidy_incoming(id);
        Ok(())
    }

    /// Drop `id`'s empty staging root and an empty `.incoming`. Best effort.
    pub(crate) fn tidy_incoming(&self, id: &str) {
        if let Ok(root) = self.staging_root(id) {
            let _ = fs::remove_dir(root);
        }
        let _ = fs::remove_dir(self.incoming_dir());
    }

    /// Roll back to an earlier revision. **Since 0.7.0 the suite layout keeps one revision per
    /// model**, so there is never an earlier one: `target` naming the installed, verifying
    /// revision returns it unchanged; anything else (including `None`) is `NoRollback`.
    /// Reinstall the wanted revision with [`Store::install`] instead.
    pub fn rollback(&self, id: &str, target: Option<&str>) -> Result<String> {
        let _lock = self.acquire(id, Duration::ZERO)?;
        self.reject_links(id)?;
        let state = self.state(id)?;
        let target =
            match target {
                Some(t) => RevisionKey::parse(t)?.as_str().to_string(),
                None => return Err(Error::new(
                    ErrorCode::NoRollback,
                    "the store keeps one revision per model; no earlier revision to roll back to",
                )),
            };
        if state.active.as_deref() != Some(target.as_str())
            || !self.verify(id, &target, VerifyMode::Size)
        {
            return Err(Error::new(
                ErrorCode::NoRollback,
                format!("revision {target} is not installed or fails verification"),
            ));
        }
        Ok(target)
    }

    /// Remove one revision: with one revision per model, removing the installed revision removes
    /// the entry (as [`Store::uninstall`]); its staging, if any, goes too. Any other key is
    /// `NotFound`.
    pub fn uninstall_revision(&self, id: &str, key: &str) -> Result<()> {
        let key = RevisionKey::parse(key)?.as_str().to_string();
        let key = key.as_str();
        {
            let _lock = self.acquire(id, Duration::ZERO)?;
            self.reject_links(id)?;
            if self.state(id)?.active.as_deref() != Some(key) {
                return Err(Error::new(
                    ErrorCode::NotFound,
                    format!("{id} has no revision {key}"),
                ));
            }
        }
        self.uninstall(id)
    }

    /// The old-layout (pre-0.7) directories a migration replaced, kept under
    /// `<root>/.legacy-kept/` instead of deleted: each holds that model's earlier revisions,
    /// `state.json` and old staging, exactly as the old layout left them. Report them to the user,
    /// who may remove them; the store never does. Sorted; empty when there are none.
    pub fn legacy_kept(&self) -> Result<Vec<PathBuf>> {
        let kept = self.root.join(crate::legacy_layout::KEPT_DIR);
        let entries = match fs::read_dir(&kept) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(Error::io(kept.display(), e)),
        };
        let mut out: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
        out.sort();
        Ok(out)
    }

    /// Delete one kept old-layout directory: `kept` must be an entry [`Store::legacy_kept`]
    /// lists (a direct child of `<root>/.legacy-kept/`, not a link); anything else is refused.
    /// Call it when the user chooses to free the space. The folder is removed when it is empty.
    pub fn remove_legacy_kept(&self, kept: &Path) -> Result<()> {
        let dir = self.root.join(crate::legacy_layout::KEPT_DIR);
        let listed = kept.parent() == Some(dir.as_path())
            && kept.file_name().is_some()
            && !is_link(&dir)
            && !is_link(kept);
        if !listed {
            return Err(Error::new(
                ErrorCode::InvalidSpec,
                format!(
                    "{} is not a kept old-layout directory of this store",
                    kept.display()
                ),
            ));
        }
        remove_tree(kept)?;
        let _ = fs::remove_dir(&dir);
        Ok(())
    }

    /// Remove the whole model: the entry, its staging and any interrupted aside or adoption, plus
    /// the legacy alias link. A symlink at `<root>/<id>` is unlinked, never followed.
    /// Shared-runtime files stay (other models may load them).
    pub fn uninstall(&self, id: &str) -> Result<()> {
        let _lock = self.acquire_raw(id, Duration::ZERO)?;
        if let Some(alias) = self.legacy_alias_path(id)? {
            if !self.alias_in_store(&alias) {
                crate::alias::remove(&alias).map_err(|e| Error::new(ErrorCode::Io, e))?;
            }
        }
        remove_tree(&self.model_dir(id)?)?;
        remove_tree(&self.aside_dir(id)?)?;
        remove_tree(&self.staging_root(id)?)?;
        remove_tree(&self.adopt_root(id)?)?;
        crate::legacy_layout::remove_migration_scratch(&self.root, id);
        self.tidy_incoming(id);
        Ok(())
    }

    /// Finish an interrupted swap (the suite's aside recovery: the previous entry is put back
    /// when the replacement never landed, else dropped), discard an interrupted adoption, and
    /// migrate a pre-0.7 entry. Caller holds the model lock. Returns whether anything was
    /// restored or migrated.
    pub(crate) fn recover_locked(&self, id: &str) -> Result<bool> {
        self.reject_links(id)?;
        let id = ModelId::parse(id)?;
        let restored = crate::suite::archive::recover_orphaned_install(&self.root, id.as_str())
            .map_err(|e| Error::new(ErrorCode::Io, e))?;
        remove_tree(&self.adopt_root(id.as_str())?)?;
        let migrated = crate::legacy_layout::migrate_locked(self, id.as_str())?;
        Ok(restored || migrated)
    }
}

/// The [`State`] of a suite entry: its manifest's version is the installed revision key.
fn state_from_manifest(id: &str, dir: &Path, manifest: &crate::suite::Manifest) -> State {
    let mut state = State {
        id: id.into(),
        ..State::default()
    };
    let Ok(key) = RevisionKey::parse(&manifest.version) else {
        return state;
    };
    let (revision, quant) = decode_revision_key(key.as_str());
    let installed_at_unix = fs::metadata(crate::suite::paths::manifest_path(dir))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let files = manifest
        .files
        .iter()
        .map(|f| FileRecord {
            path: f.path.clone(),
            bytes: f.size.unwrap_or(0),
            sha256: f.sha256.clone().unwrap_or_default(),
            shared: f.shared.clone(),
        })
        .collect();
    state.revisions.insert(
        key.as_str().to_string(),
        RevisionRecord {
            revision,
            quant,
            installed_at_unix,
            files,
        },
    );
    state.active = Some(key.as_str().to_string());
    state
}

/// The suite manifest that records `records` as revision `key` of `id`.
pub(crate) fn manifest_for(id: &str, key: &str, records: &[FileRecord]) -> crate::suite::Manifest {
    let mut files: Vec<crate::suite::ManifestFile> = records
        .iter()
        .map(|r| crate::suite::ManifestFile {
            path: r.path.clone(),
            sha256: Some(r.sha256.to_ascii_lowercase()),
            size: Some(r.bytes),
            shared: r.shared.clone(),
        })
        .collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    crate::suite::Manifest {
        id: id.into(),
        version: key.into(),
        files,
    }
}

/// Inverse of [`crate::ids::encode_revision_key`]: `(revision, quant)`.
fn decode_revision_key(key: &str) -> (String, Option<String>) {
    fn part(s: &str) -> String {
        let b = s.as_bytes();
        let mut out = Vec::with_capacity(b.len());
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'_' {
                if let Some(v) = s
                    .get(i + 1..i + 3)
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
                {
                    out.push(v);
                    i += 3;
                    continue;
                }
            }
            out.push(b[i]);
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    }
    match key.split_once('@') {
        Some((revision, quant)) => (part(revision), Some(part(quant))),
        None => (part(key), None),
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
/// Copy a directory tree (regular files and directories only; anything else is refused).
fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to).map_err(|e| Error::io(to.display(), e))?;
    for entry in fs::read_dir(from).map_err(|e| Error::io(from.display(), e))? {
        let entry = entry.map_err(|e| Error::io(from.display(), e))?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        let kind = entry.file_type().map_err(|e| Error::io(src.display(), e))?;
        if kind.is_dir() {
            copy_tree(&src, &dst)?;
        } else if kind.is_file() {
            fs::copy(&src, &dst).map_err(|e| Error::io(src.display(), e))?;
        } else {
            return Err(Error::new(
                ErrorCode::InvalidSpec,
                format!(
                    "shared runtime tree holds a non-regular entry: {}",
                    src.display()
                ),
            ));
        }
    }
    Ok(())
}

/// True when `b` is a real directory holding exactly the entries of `a` with identical bytes.
fn dirs_identical(a: &Path, b: &Path) -> Result<bool> {
    if !fs::symlink_metadata(b).is_ok_and(|m| m.file_type().is_dir()) {
        return Ok(false);
    }
    let entries = |dir: &Path| -> Result<BTreeMap<std::ffi::OsString, bool>> {
        let mut out = BTreeMap::new();
        for entry in fs::read_dir(dir).map_err(|e| Error::io(dir.display(), e))? {
            let entry = entry.map_err(|e| Error::io(dir.display(), e))?;
            let is_dir = entry
                .file_type()
                .map_err(|e| Error::io(dir.display(), e))?
                .is_dir();
            out.insert(entry.file_name(), is_dir);
        }
        Ok(out)
    };
    let (left, right) = (entries(a)?, entries(b)?);
    if left != right {
        return Ok(false);
    }
    for (name, is_dir) in left {
        let (x, y) = (a.join(&name), b.join(&name));
        let same = if is_dir {
            dirs_identical(&x, &y)?
        } else {
            match (sha256_file(&x), sha256_file(&y)) {
                (Ok(h1), Ok(h2)) => h1 == h2,
                _ => false,
            }
        };
        if !same {
            return Ok(false);
        }
    }
    Ok(true)
}

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

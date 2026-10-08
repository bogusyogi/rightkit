//! Install / upgrade: disk preflight, resumable verified downloads into a
//! persistent staging dir, archive extraction, then an atomic swap +
//! `state.json` activation. Installing a newer revision *is* the upgrade; the
//! previous revision stays on disk for rollback up to `keep_revisions`.
//!
//! - **Pause vs cancel.** `InstallOptions::pause` stops at the next chunk and
//!   keeps staging (finished files and `.part`s) with a `paused.json` marker;
//!   installing the same revision again resumes (HTTP `Range` when the server
//!   proves continuity with a strong ETag, otherwise a clean restart of that
//!   file). `InstallOptions::cancel` stops and discards the staging.
//! - **Preflight credit.** Files already staged, already present in the
//!   same-key revision (hard-linked into staging, no copy), or already at
//!   their shared-runtime location with matching pins need no space; `.part`
//!   files are credited by their length.
//! - **Adoption.** [`Store::adopt`] records an existing install (verified
//!   against the pins) without downloading. [`Store::adopt_then_install`]
//!   adopts whatever verifies and downloads only the rest, in one swap.

use crate::archive;
use crate::disk;
use crate::download::{
    download_resumable_with_identity, http_agent_with_redirects, DownloadRequest,
};
use crate::error::{Error, ErrorCode, Result};
use crate::hash::sha256_file;
use crate::ids::ModelId;
use crate::platform::Platform;
use crate::spec::{safe_rel_path, Extract, FileSpec, ModelSpec, RevisionSpec};
use crate::store::{
    is_link, remove_tree, write_atomic, AliasStatus, FileRecord, PausedInstall, RevisionRecord,
    State, Store, VerifyMode,
};
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// Bytes that need no new disk space: staged/installed files whose pins
    /// already match, plus resumable `.part` lengths. Sent before `Preflight`.
    PreflightCredit {
        credited_bytes: u64,
    },
    Preflight {
        needed_bytes: u64,
        free_bytes: Option<u64>,
    },
    /// A paused install picked up again.
    Resumed {
        key: String,
        staged_bytes: u64,
    },
    FileStart {
        path: String,
        index: usize,
        total_files: usize,
    },
    Progress {
        path: String,
        file_done: u64,
        file_total: u64,
        overall_done: u64,
        overall_total: u64,
    },
    FileVerified {
        path: String,
        sha256: String,
    },
    Extracted {
        path: String,
    },
    /// [`Store::adopt_with`]: an existing file was placed (hard link when
    /// `linked`, else a copy) and verified against its pins.
    Adopted {
        path: String,
        linked: bool,
    },
    AlreadyInstalled {
        key: String,
    },
    Activated {
        key: String,
        previous: Option<String>,
    },
    Pruned {
        key: String,
    },
    /// The install stopped on `pause`; staging is kept.
    Paused {
        key: String,
        staged_bytes: u64,
    },
    /// Legacy flat alias maintenance (only when an alias root is configured).
    LegacyAlias {
        status: AliasStatus,
        path: Option<PathBuf>,
        target: Option<PathBuf>,
        message: Option<String>,
    },
}

#[derive(Clone)]
pub struct InstallOptions {
    /// `None` = newest.
    pub revision: Option<String>,
    /// `None` = any quantization.
    pub quant: Option<String>,
    pub platform: Platform,
    /// `Authorization` header value for gated hosts.
    pub authorization: Option<String>,
    /// Stable immutable identity for rotating signed download URLs. Only used
    /// with SHA-256-pinned files; path & digest also bind resume metadata.
    pub resume_identity: Option<String>,
    /// Maximum download redirects. Zero keeps scoped capabilities on their origin.
    pub max_download_redirects: u32,
    /// Set to `true` from another thread to cancel: the install stops with
    /// `Cancelled` and its staging (including `.part` files) is discarded.
    pub cancel: Arc<AtomicBool>,
    /// Set to `true` from another thread to pause: the install stops with
    /// `Paused`, keeps staging and `.part` files, and records a
    /// [`PausedInstall`] marker. Installing the same revision again resumes.
    /// `cancel` wins when both are set.
    pub pause: Arc<AtomicBool>,
    /// Revisions kept besides the active one (for rollback).
    pub keep_revisions: usize,
    /// Test/override hook for the free-space probe.
    pub free_bytes_override: Option<u64>,
    /// Trusted-catalog mode: refuse any file without a pinned sha256 and byte
    /// size (and, for archive members, pinned installed-payload hash/size).
    pub require_pinned: bool,
    /// How long to wait for another process holding the model lock.
    pub lock_wait: Duration,
    /// Test hook: simulate the process dying right after the named durable
    /// step of a same-key swap (no cleanup runs, exactly like a kill).
    #[doc(hidden)]
    pub crash_after: Option<CrashPoint>,
}

/// Durable steps of replacing an existing same-key revision directory.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashPoint {
    /// Previous revision moved to `.aside`, replacement not yet renamed in.
    AfterAside,
    /// Replacement renamed in, ledger (`state.json`) not yet written.
    AfterSwap,
    /// Ledger written, aside copy not yet deleted.
    AfterState,
}

impl Default for InstallOptions {
    fn default() -> Self {
        Self {
            revision: None,
            quant: None,
            platform: Platform::current(),
            authorization: None,
            resume_identity: None,
            max_download_redirects: 10,
            cancel: Arc::new(AtomicBool::new(false)),
            pause: Arc::new(AtomicBool::new(false)),
            keep_revisions: 1,
            free_bytes_override: None,
            require_pinned: false,
            lock_wait: Duration::ZERO,
            crash_after: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct InstallOutcome {
    pub key: String,
    pub dir: PathBuf,
    pub already_installed: bool,
    pub previous: Option<String>,
}

/// Where an existing install keeps a manifest file's payload (adoption).
type ExistingFn<'a> = dyn Fn(&FileSpec) -> Option<PathBuf> + 'a;

/// sha256 of files hashed during one locked operation, so preflight credit
/// and the fetch pass never hash the same multi-GB file twice.
#[derive(Default)]
struct HashCache(HashMap<PathBuf, String>);

impl HashCache {
    fn sha(&mut self, p: &Path) -> Result<String> {
        if let Some(h) = self.0.get(p) {
            return Ok(h.clone());
        }
        let h = sha256_file(p).map_err(|e| Error::new(ErrorCode::Io, e))?;
        self.0.insert(p.to_path_buf(), h.clone());
        Ok(h)
    }
    fn forget(&mut self, p: &Path) {
        self.0.remove(p);
    }
}

/// Size first (cheap), then the cached hash. At least one pin is required.
fn pins_match(path: &Path, bytes: Option<u64>, sha: Option<&str>, cache: &mut HashCache) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() || bytes.is_some_and(|b| b != meta.len()) {
        return false;
    }
    if bytes.is_none() && sha.is_none() {
        return false;
    }
    sha.is_none_or(|s| cache.sha(path).is_ok_and(|h| h.eq_ignore_ascii_case(s)))
}

/// Pins of the payload as it sits on disk after install.
fn final_pins(f: &FileSpec) -> (Option<u64>, Option<&str>) {
    match f.extract {
        Some(Extract::Member(_)) => (f.installed_bytes, f.installed_sha256.as_deref()),
        _ => (f.bytes, f.sha256.as_deref()),
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Store {
    /// Revision + platform files for `opts`, validated.
    fn select_files<'a>(
        &self,
        spec: &'a ModelSpec,
        opts: &'a InstallOptions,
        require_pinned: bool,
    ) -> Result<(&'a RevisionSpec, Vec<&'a FileSpec>)> {
        let rev = spec
            .select(opts.revision.as_deref(), opts.quant.as_deref())
            .ok_or_else(|| {
                Error::new(
                    ErrorCode::NotFound,
                    format!("{}: no matching revision/quant", spec.id),
                )
            })?;
        let files: Vec<&FileSpec> = rev.files_for(&opts.platform).collect();
        if files.is_empty() {
            return Err(Error::new(
                ErrorCode::NoFiles,
                format!("{} {}: no files for this platform", spec.id, rev.key()),
            ));
        }
        for f in &files {
            if safe_rel_path(&f.path).is_none() {
                return Err(Error::new(
                    ErrorCode::InvalidSpec,
                    format!("unsafe file path {:?}", f.path),
                ));
            }
            let archive_pinned = f.sha256.is_some() && f.bytes.is_some();
            let member_pinned = !matches!(f.extract, Some(Extract::Member(_)))
                || (f.installed_sha256.is_some() && f.installed_bytes.is_some());
            if let Some(kind) = &f.shared {
                self.shared_path(kind, &f.path)?;
                if matches!(f.extract, Some(Extract::TarGz)) {
                    return Err(Error::new(
                        ErrorCode::InvalidSpec,
                        format!(
                            "{}: shared file {:?} cannot be a TarGz tree",
                            spec.id, f.path
                        ),
                    ));
                }
                if !archive_pinned || !member_pinned {
                    return Err(Error::new(
                        ErrorCode::InvalidSpec,
                        format!(
                            "{}: shared file {:?} is not hash- and size-pinned",
                            spec.id, f.path
                        ),
                    ));
                }
            }
            if require_pinned && (!archive_pinned || !member_pinned) {
                return Err(Error::new(
                    ErrorCode::InvalidSpec,
                    format!(
                        "{}: file {:?} is not hash- and size-pinned",
                        spec.id, f.path
                    ),
                ));
            }
        }
        Ok((rev, files))
    }

    pub fn install(
        &self,
        spec: &ModelSpec,
        opts: &InstallOptions,
        sink: impl FnMut(Event),
    ) -> Result<InstallOutcome> {
        self.install_inner(spec, opts, None, sink)
    }

    /// Partial adoption: one locked install that first adopts every existing
    /// file whose pins match (hard link into staging, falling back to a
    /// copy), then downloads only the missing or mismatching ones, then does
    /// the one atomic swap + activation of [`Store::install`].
    ///
    /// - `existing` maps a manifest file to where its installed payload lives
    ///   today (`Extract::Member`: the extracted member). Returning `None`, a
    ///   missing path, or a file that fails its size/sha256 pins means
    ///   "download it"; a mismatching file is never linked or copied.
    /// - Fails closed like [`Store::adopt`]: every file must be hash- and
    ///   size-pinned (`require_pinned` is forced), and the revision is
    ///   recorded only after every file is verified. Adopted files wait in the
    ///   ordinary resumable staging dir, so a crash or pause at any point
    ///   leaves nothing recorded; the next call re-verifies and resumes.
    /// - Preflight counts only bytes still needed: hard-linked files need no
    ///   space, copies count their installed size, downloads their artifact.
    /// - `TarGz` trees are always downloaded (they have no per-file pins).
    ///
    /// Emits [`Event::Adopted`] per adopted file before the download events.
    pub fn adopt_then_install(
        &self,
        spec: &ModelSpec,
        opts: &InstallOptions,
        existing: impl Fn(&FileSpec) -> Option<PathBuf>,
        sink: impl FnMut(Event),
    ) -> Result<InstallOutcome> {
        let mut pinned = opts.clone();
        pinned.require_pinned = true;
        self.install_inner(spec, &pinned, Some(&existing), sink)
    }

    fn install_inner(
        &self,
        spec: &ModelSpec,
        opts: &InstallOptions,
        existing: Option<&ExistingFn>,
        mut sink: impl FnMut(Event),
    ) -> Result<InstallOutcome> {
        let (rev, files) = self.select_files(spec, opts, opts.require_pinned)?;
        let key = rev.checked_key()?.as_str().to_string();
        let id = ModelId::parse(&spec.id)?.as_str().to_string();
        let id = id.as_str();
        let _lock = self.acquire(id, opts.lock_wait)?;
        self.reject_links(id)?;
        let mut state = self.state(id)?;
        state.id = id.to_string();

        if let Some(out) = self.reactivate_if_installed(&mut state, &key, opts, &mut sink)? {
            return Ok(out);
        }

        let staging = self.staging_dir(id, &key)?;
        let marker = staging.join(".dl").join("paused.json");
        if let Some(p) = fs::read(&marker)
            .ok()
            .and_then(|b| serde_json::from_slice::<PausedInstall>(&b).ok())
        {
            let _ = fs::remove_file(&marker);
            sink(Event::Resumed {
                key: key.clone(),
                staged_bytes: p.staged_bytes,
            });
        }
        fs::create_dir_all(staging.join(".dl")).map_err(|e| Error::io(staging.display(), e))?;
        let mut cache = HashCache::default();
        self.seed_from_installed(id, &key, &files, &staging, &mut cache);
        // Partial adoption: link verified existing files into staging now;
        // files that cannot be hard-linked are copied after preflight.
        let mut to_copy: Vec<(&FileSpec, PathBuf)> = Vec::new();
        if let Some(existing) = existing {
            for f in &files {
                let (fb, fsha) = final_pins(f);
                let shared_ready = f.shared.as_ref().is_some_and(|k| {
                    self.shared_path(k, &f.path)
                        .is_ok_and(|p| pins_match(&p, fb, fsha, &mut cache))
                });
                if shared_ready {
                    continue;
                }
                if let Some(src) = adoptable(f, &staging, existing, &mut cache) {
                    let dest = stage_dest(&staging, f);
                    match try_hard_link(&src, &dest) {
                        Ok(true) => {
                            if let Ok(h) = cache.sha(&src) {
                                cache.0.insert(dest, h);
                            }
                            sink(Event::Adopted {
                                path: f.path.clone(),
                                linked: true,
                            });
                        }
                        _ => to_copy.push((f, src)),
                    }
                }
            }
        }

        // Disk preflight: what is still missing + 10% headroom. A pending
        // copy needs its installed size, not its download.
        // (`adoptable` cleared their `.part`/archive, so preflight counted
        // the full download + unpack for each.)
        let (mut needed, credited) = self.preflight(&files, &staging, &mut cache);
        for (f, _) in &to_copy {
            let counted = f.bytes.unwrap_or(0) + f.installed_bytes.unwrap_or(0);
            needed = needed.saturating_sub(counted) + final_pins(f).0.unwrap_or(0);
        }
        let free = opts
            .free_bytes_override
            .or_else(|| disk::free_bytes(&self.root));
        sink(Event::PreflightCredit {
            credited_bytes: credited,
        });
        sink(Event::Preflight {
            needed_bytes: needed,
            free_bytes: free,
        });
        if let Some(free) = free {
            if needed > 0 && (free as f64) < needed as f64 * 1.1 {
                return Err(Error::new(
                    ErrorCode::DiskSpace,
                    format!(
                        "{id}: need ~{:.1} GB free, {:.1} GB available",
                        needed as f64 / 1e9,
                        free as f64 / 1e9
                    ),
                ));
            }
        }

        let cancel = || opts.cancel.load(Ordering::Relaxed);
        let stop = || cancel() || opts.pause.load(Ordering::Relaxed);
        for (f, src) in to_copy {
            if stop() {
                return Err(self.interrupted(rev, &key, &staging, cancel(), &mut sink));
            }
            let dest = stage_dest(&staging, f);
            let (bytes, sha) = final_pins(f);
            let _ = fs::remove_file(&dest);
            // Best effort: a failed or mismatching copy is simply downloaded.
            if link_or_copy(&src, &dest).is_ok() {
                cache.forget(&dest);
                if pins_match(&dest, bytes, sha, &mut cache) {
                    sink(Event::Adopted {
                        path: f.path.clone(),
                        linked: false,
                    });
                } else {
                    let _ = fs::remove_file(&dest);
                    cache.forget(&dest);
                }
            }
        }
        let agent = http_agent_with_redirects(opts.max_download_redirects);
        let overall_total: u64 = files.iter().filter_map(|f| f.bytes).sum();
        let mut overall_base = 0u64;
        let mut records: Vec<FileRecord> = Vec::new();
        for (index, f) in files.iter().enumerate() {
            if stop() {
                return Err(self.interrupted(rev, &key, &staging, cancel(), &mut sink));
            }
            sink(Event::FileStart {
                path: f.path.clone(),
                index,
                total_files: files.len(),
            });
            let fetched = self.fetch_file(
                &agent,
                f,
                &staging,
                opts,
                &stop,
                &mut cache,
                &mut |done, total| {
                    sink(Event::Progress {
                        path: f.path.clone(),
                        file_done: done,
                        file_total: total,
                        overall_done: overall_base + done,
                        overall_total,
                    });
                },
            );
            let recs = match fetched {
                Ok(r) => r,
                Err(e) if e.code == ErrorCode::Cancelled || stop() => {
                    return Err(self.interrupted(rev, &key, &staging, cancel(), &mut sink));
                }
                Err(e) => return Err(e),
            };
            for r in &recs {
                sink(Event::FileVerified {
                    path: r.path.clone(),
                    sha256: r.sha256.clone(),
                });
            }
            records.extend(recs);
            overall_base += f.bytes.unwrap_or(0);
        }
        self.place_shared(&files, &staging, &mut cache)?;
        self.commit_staged(&mut state, rev, &key, &staging, records, opts, &mut sink)
    }

    /// Verify an existing install (e.g. a pre-store flat directory) against
    /// the manifest pins and record it as installed, without downloading.
    /// `existing` maps each manifest file to where its installed payload lives
    /// today (`Extract::Member` files: the extracted member). Each file is
    /// hard-linked (same volume) or copied into a private `.adopt/<key>`
    /// tree, verified there (size + sha256), and the tree is swapped in and
    /// activated exactly like a download. Fails closed: every platform file
    /// must be supplied (shared-runtime files may instead already sit at their
    /// shared location) and pinned, `TarGz` trees cannot be adopted, and any
    /// mismatch aborts with nothing recorded.
    pub fn adopt(
        &self,
        spec: &ModelSpec,
        opts: &InstallOptions,
        existing: impl Fn(&FileSpec) -> Option<PathBuf>,
    ) -> Result<InstallOutcome> {
        self.adopt_with(spec, opts, existing, |_| {})
    }

    /// [`Store::adopt`] with progress events.
    pub fn adopt_with(
        &self,
        spec: &ModelSpec,
        opts: &InstallOptions,
        existing: impl Fn(&FileSpec) -> Option<PathBuf>,
        mut sink: impl FnMut(Event),
    ) -> Result<InstallOutcome> {
        let (rev, files) = self.select_files(spec, opts, true)?;
        if let Some(f) = files
            .iter()
            .find(|f| matches!(f.extract, Some(Extract::TarGz)))
        {
            return Err(Error::new(
                ErrorCode::InvalidSpec,
                format!(
                    "{}: {:?} is an unpacked archive tree with no per-file pins; it cannot be adopted",
                    spec.id, f.path
                ),
            ));
        }
        let key = rev.checked_key()?.as_str().to_string();
        let id = ModelId::parse(&spec.id)?.as_str().to_string();
        let id = id.as_str();
        let _lock = self.acquire(id, opts.lock_wait)?;
        self.reject_links(id)?;
        let mut state = self.state(id)?;
        state.id = id.to_string();

        if let Some(out) = self.reactivate_if_installed(&mut state, &key, opts, &mut sink)? {
            return Ok(out);
        }

        let work = self.model_dir(id)?.join(".adopt").join(&key);
        remove_tree(&work)?;
        fs::create_dir_all(&work).map_err(|e| Error::io(work.display(), e))?;
        let mut cache = HashCache::default();
        let assembled = (|| -> Result<Vec<FileRecord>> {
            let mut records = Vec::new();
            for f in &files {
                let (bytes, sha) = final_pins(f);
                if let Some(kind) = &f.shared {
                    let at = self.shared_path(kind, &f.path)?;
                    if pins_match(&at, bytes, sha, &mut cache) {
                        records.push(record(&at, &f.path, Some(kind), &mut cache)?);
                        continue;
                    }
                }
                let src = existing(f).ok_or_else(|| {
                    Error::new(
                        ErrorCode::NotFound,
                        format!("adopt {id}: no existing file for {:?}", f.path),
                    )
                })?;
                let dest = stage_dest(&work, f);
                let linked = link_or_copy(&src, &dest)?;
                let len = fs::metadata(&dest)
                    .map_err(|e| Error::io(dest.display(), e))?
                    .len();
                if bytes.is_some_and(|b| b != len) {
                    return Err(Error::new(
                        ErrorCode::SizeMismatch,
                        format!(
                            "adopt {id}: {} is {len} bytes, pinned {}",
                            src.display(),
                            bytes.unwrap_or(0)
                        ),
                    ));
                }
                if !pins_match(&dest, bytes, sha, &mut cache) {
                    return Err(Error::new(
                        ErrorCode::HashMismatch,
                        format!("adopt {id}: {} fails its sha256 pin", src.display()),
                    ));
                }
                records.push(record(&dest, &f.path, f.shared.as_deref(), &mut cache)?);
                sink(Event::Adopted {
                    path: f.path.clone(),
                    linked,
                });
            }
            self.place_shared(&files, &work, &mut cache)?;
            Ok(records)
        })();
        let adopt_root = self.model_dir(id)?.join(".adopt");
        let records = match assembled {
            Ok(r) => r,
            Err(e) => {
                let _ = remove_tree(&work);
                let _ = fs::remove_dir(&adopt_root);
                return Err(e);
            }
        };
        // A failure from here on leaves `.adopt` for the next lock holder's
        // recovery to discard; nothing is recorded until `state.json` is.
        let out = self.commit_staged(&mut state, rev, &key, &work, records, opts, &mut sink)?;
        let _ = fs::remove_dir(&adopt_root);
        Ok(out)
    }

    /// Already installed and intact: just (re)activate.
    fn reactivate_if_installed(
        &self,
        state: &mut State,
        key: &str,
        opts: &InstallOptions,
        sink: &mut impl FnMut(Event),
    ) -> Result<Option<InstallOutcome>> {
        let id = state.id.clone();
        if !(state.revisions.contains_key(key) && self.verify(&id, key, VerifyMode::Full)) {
            return Ok(None);
        }
        sink(Event::AlreadyInstalled { key: key.into() });
        let previous = self.activate(state, key)?;
        sink(Event::Activated {
            key: key.into(),
            previous: previous.clone(),
        });
        self.prune(state, opts.keep_revisions, sink)?;
        self.alias_event(state, sink);
        Ok(Some(InstallOutcome {
            dir: self.revision_dir(&id, key)?,
            key: key.into(),
            already_installed: true,
            previous,
        }))
    }

    /// Stop on pause (keep staging + marker) or cancel (discard staging).
    fn interrupted(
        &self,
        rev: &RevisionSpec,
        key: &str,
        staging: &Path,
        cancelled: bool,
        sink: &mut impl FnMut(Event),
    ) -> Error {
        if cancelled {
            return match remove_tree(staging) {
                Ok(()) => Error::new(
                    ErrorCode::Cancelled,
                    "install cancelled; staged files discarded",
                ),
                Err(e) => Error::new(
                    ErrorCode::Cancelled,
                    format!("install cancelled; discarding staging failed: {e}"),
                ),
            };
        }
        let staged_bytes = tree_bytes(staging);
        let marker = PausedInstall {
            key: key.into(),
            revision: rev.revision.clone(),
            quant: rev.quant.clone(),
            staged_bytes,
            paused_at_unix: unix_now(),
        };
        let written = serde_json::to_vec_pretty(&marker)
            .map_err(|e| Error::io("serialize pause marker", e))
            .and_then(|b| write_atomic(&staging.join(".dl").join("paused.json"), &b));
        sink(Event::Paused {
            key: key.into(),
            staged_bytes,
        });
        match written {
            Ok(()) => Error::new(
                ErrorCode::Paused,
                "install paused; staged files kept for resume",
            ),
            Err(e) => Error::new(
                ErrorCode::Paused,
                format!("install paused; marker not written: {e}"),
            ),
        }
    }

    /// Hard-link files of the existing same-key revision (a repair) whose
    /// pins still match into staging, so they are neither downloaded nor
    /// counted against free space. Best effort: anything not linked is fetched.
    fn seed_from_installed(
        &self,
        id: &str,
        key: &str,
        files: &[&FileSpec],
        staging: &Path,
        cache: &mut HashCache,
    ) {
        let Ok(dir) = self.revision_dir(id, key) else {
            return;
        };
        if !dir.is_dir() {
            return;
        }
        for f in files {
            if f.shared.is_some() || matches!(f.extract, Some(Extract::TarGz)) {
                continue;
            }
            let Some(rel) = safe_rel_path(&f.path) else {
                continue;
            };
            let (src, dest) = (dir.join(&rel), staging.join(&rel));
            let (bytes, sha) = final_pins(f);
            if dest.exists() || is_link(&src) || !pins_match(&src, bytes, sha, cache) {
                continue;
            }
            if let Some(parent) = dest.parent() {
                if fs::create_dir_all(parent).is_err() {
                    continue;
                }
            }
            if fs::hard_link(&src, &dest).is_ok() {
                if let Ok(h) = cache.sha(&src) {
                    cache.0.insert(dest, h);
                }
            }
        }
    }

    /// (still needed, credited) bytes for the remaining work.
    fn preflight(&self, files: &[&FileSpec], staging: &Path, cache: &mut HashCache) -> (u64, u64) {
        let (mut needed, mut credited) = (0u64, 0u64);
        for f in files {
            let download = f.bytes.unwrap_or(0);
            let installed = f.installed_bytes.unwrap_or(0);
            let total = download + installed;
            let (fb, fs_) = final_pins(f);
            let shared_ready = f.shared.as_ref().is_some_and(|k| {
                self.shared_path(k, &f.path)
                    .is_ok_and(|p| pins_match(&p, fb, fs_, cache))
            });
            let dest = stage_dest(staging, f);
            let staged_ready = match f.extract {
                Some(Extract::TarGz) => dest.is_dir(),
                _ => pins_match(&dest, fb, fs_, cache),
            };
            if shared_ready || staged_ready {
                credited += total;
                continue;
            }
            let part = fs::metadata(partial_path(staging, f))
                .map(|m| m.len())
                .unwrap_or(0)
                .min(download);
            let archive_ready = f.extract.is_some()
                && pins_match(
                    &archive_path(staging, f),
                    f.bytes,
                    f.sha256.as_deref(),
                    cache,
                );
            let (need, credit) = if archive_ready {
                (installed, download)
            } else {
                (total - part, part)
            };
            needed += need;
            credited += credit;
        }
        (needed, credited)
    }

    /// Move staged shared-runtime files to their shared location without ever
    /// clobbering an existing file: identical bytes are accepted, different
    /// bytes are a `Conflict` (staging is kept).
    fn place_shared(
        &self,
        files: &[&FileSpec],
        staging: &Path,
        cache: &mut HashCache,
    ) -> Result<()> {
        for f in files {
            let Some(kind) = &f.shared else { continue };
            let staged = stage_dest(staging, f);
            if !staged.is_file() {
                continue; // already in place (verified when recorded)
            }
            let (bytes, sha) = final_pins(f);
            let dst = self.shared_path(kind, &f.path)?;
            place_no_clobber(&staged, &dst, bytes, sha, cache)?;
        }
        let _ = remove_tree(&staging.join(".shared"));
        Ok(())
    }

    /// Atomic swap of `staging` into `revisions/<key>`, ledger write,
    /// activation, pruning and alias maintenance. A same-key directory is
    /// never deleted before its replacement is durable: it is renamed to
    /// `.aside/<key>`, the new tree renamed in, the ledger written, and only
    /// then the aside dropped. `Store::recover` (run under every lock
    /// acquisition) finishes or undoes whichever step a crash interrupted.
    #[allow(clippy::too_many_arguments)]
    fn commit_staged(
        &self,
        state: &mut State,
        rev: &RevisionSpec,
        key: &str,
        staging: &Path,
        mut records: Vec<FileRecord>,
        opts: &InstallOptions,
        sink: &mut impl FnMut(Event),
    ) -> Result<InstallOutcome> {
        let id = state.id.clone();
        let _ = fs::remove_dir_all(staging.join(".dl"));
        let _ = remove_tree(&staging.join(".shared"));
        let dest = self.revision_dir(&id, key)?;
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::io(dest.display(), e))?;
        }
        let crash = |p: CrashPoint| -> Result<()> {
            if opts.crash_after == Some(p) {
                return Err(Error::new(ErrorCode::Io, format!("injected crash {p:?}")));
            }
            Ok(())
        };
        let aside = self.aside_dir(&id, key)?;
        let had_previous = fs::symlink_metadata(&dest).is_ok();
        if had_previous {
            if let Some(parent) = aside.parent() {
                fs::create_dir_all(parent).map_err(|e| Error::io(aside.display(), e))?;
            }
            remove_tree(&aside)?;
            fs::rename(&dest, &aside)
                .map_err(|e| Error::io(format!("move aside {}", dest.display()), e))?;
            crash(CrashPoint::AfterAside)?;
        }
        if let Err(e) = fs::rename(staging, &dest) {
            if had_previous {
                let _ = fs::rename(&aside, &dest);
            }
            return Err(Error::io(format!("activate {}", dest.display()), e));
        }
        crash(CrashPoint::AfterSwap)?;
        records.sort_by(|a, b| a.path.cmp(&b.path));
        state.revisions.insert(
            key.to_string(),
            RevisionRecord {
                revision: rev.revision.clone(),
                quant: rev.quant.clone(),
                installed_at_unix: unix_now(),
                files: records,
            },
        );
        let previous = self.activate(state, key)?;
        crash(CrashPoint::AfterState)?;
        if had_previous {
            remove_tree(&aside)?;
        }
        sink(Event::Activated {
            key: key.to_string(),
            previous: previous.clone(),
        });
        self.prune(state, opts.keep_revisions, sink)?;
        self.alias_event(state, sink);
        Ok(InstallOutcome {
            dir: dest,
            key: key.to_string(),
            already_installed: false,
            previous,
        })
    }

    fn alias_event(&self, state: &State, sink: &mut impl FnMut(Event)) {
        let a = self.sync_alias_locked(&state.id, state);
        if a.status != AliasStatus::Disabled {
            sink(Event::LegacyAlias {
                status: a.status,
                path: a.path,
                target: a.target,
                message: a.message,
            });
        }
    }

    /// Flip `active` to `key` (recording the old one in history) and persist.
    fn activate(&self, state: &mut State, key: &str) -> Result<Option<String>> {
        let previous = state.active.clone().filter(|a| a != key);
        if let Some(p) = &previous {
            state.history.retain(|k| k != p);
            state.history.push(p.clone());
        }
        state.history.retain(|k| k != key);
        state.active = Some(key.to_string());
        self.save_state(state)?;
        Ok(previous)
    }

    /// Keep the active revision plus the `keep` most recent history entries.
    fn prune(&self, state: &mut State, keep: usize, sink: &mut impl FnMut(Event)) -> Result<()> {
        let id = state.id.clone();
        let retained: Vec<String> = state.history.iter().rev().take(keep).cloned().collect();
        let doomed: Vec<String> = state
            .revisions
            .keys()
            .filter(|k| Some(*k) != state.active.as_ref() && !retained.contains(k))
            .cloned()
            .collect();
        if doomed.is_empty() {
            return Ok(());
        }
        for k in &doomed {
            state.revisions.remove(k);
            state.history.retain(|h| h != k);
        }
        self.save_state(state)?;
        for k in doomed {
            if let Ok(dir) = self.revision_dir(&id, &k) {
                let _ = remove_tree(&dir);
            }
            sink(Event::Pruned { key: k });
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn fetch_file(
        &self,
        agent: &ureq::Agent,
        f: &FileSpec,
        staging: &Path,
        opts: &InstallOptions,
        stop: &dyn Fn() -> bool,
        cache: &mut HashCache,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<Vec<FileRecord>> {
        let shared = f.shared.as_deref();
        if let Some(kind) = shared {
            let at = self.shared_path(kind, &f.path)?;
            let (bytes, sha) = final_pins(f);
            if pins_match(&at, bytes, sha, cache) {
                return Ok(vec![record(&at, &f.path, shared, cache)?]);
            }
        }
        let dest = stage_dest(staging, f);
        let url = self.download_url(&f.url);
        let download = |dest_path: &Path,
                        part: &Path,
                        progress: &mut dyn FnMut(u64, u64),
                        cache: &mut HashCache|
         -> Result<String> {
            let identity = opts
                .resume_identity
                .as_ref()
                .zip(f.sha256.as_ref())
                .map(|(identity, sha)| format!("{identity}:{}:{sha}", f.path));
            let res = download_resumable_with_identity(
                agent,
                DownloadRequest {
                    url: &url,
                    part_path: part,
                    dest_path,
                    expected_sha256: f.sha256.as_deref(),
                    authorization: opts.authorization.as_deref(),
                    cancelled: stop,
                },
                identity.as_deref(),
                &mut *progress,
            )?;
            cache.forget(dest_path);
            let len = fs::metadata(dest_path).map(|m| m.len()).unwrap_or(0);
            if f.bytes.is_some_and(|b| b != len) {
                let _ = fs::remove_file(dest_path);
                return Err(Error::new(
                    ErrorCode::SizeMismatch,
                    format!("{}: expected {} bytes, got {len}", f.path, f.bytes.unwrap()),
                ));
            }
            cache.0.insert(dest_path.to_path_buf(), res.sha256.clone());
            Ok(res.sha256)
        };

        match &f.extract {
            None => {
                if !pins_match(&dest, f.bytes, f.sha256.as_deref(), cache) {
                    download(&dest, &partial_path(staging, f), progress, cache)?;
                }
                Ok(vec![record(&dest, &f.path, shared, cache)?])
            }
            Some(Extract::Member(member)) => {
                if pins_match(
                    &dest,
                    f.installed_bytes,
                    f.installed_sha256.as_deref(),
                    cache,
                ) {
                    return Ok(vec![record(&dest, &f.path, shared, cache)?]);
                }
                let archive_path = archive_path(staging, f);
                if !pins_match(&archive_path, f.bytes, f.sha256.as_deref(), cache) {
                    download(&archive_path, &partial_path(staging, f), progress, cache)?;
                }
                archive::extract_member(&archive_path, member, &dest)?;
                cache.forget(&dest);
                if !pins_match(
                    &dest,
                    f.installed_bytes,
                    f.installed_sha256.as_deref(),
                    cache,
                ) {
                    let _ = fs::remove_file(&dest);
                    return Err(Error::new(
                        ErrorCode::HashMismatch,
                        format!("{}: extracted {member} fails its pins", f.path),
                    ));
                }
                let _ = fs::remove_file(&archive_path);
                Ok(vec![record(&dest, &f.path, shared, cache)?])
            }
            Some(Extract::TarGz) => {
                if !dest.is_dir() {
                    let archive_path = archive_path(staging, f);
                    if !pins_match(&archive_path, f.bytes, f.sha256.as_deref(), cache) {
                        download(&archive_path, &partial_path(staging, f), progress, cache)?;
                    }
                    let tmp = dest.with_extension("unpacking");
                    let _ = fs::remove_dir_all(&tmp);
                    archive::unpack_wrapped(&archive_path, &tmp)?;
                    fs::rename(&tmp, &dest).map_err(|e| Error::io(dest.display(), e))?;
                    let _ = fs::remove_file(&archive_path);
                }
                let mut out = Vec::new();
                walk(&dest, &dest, &mut out)?;
                out.into_iter()
                    .map(|(abs, rel)| record(&abs, &format!("{}/{rel}", f.path), None, cache))
                    .collect()
            }
        }
    }
}

fn record(
    path: &Path,
    rel: &str,
    shared: Option<&str>,
    cache: &mut HashCache,
) -> Result<FileRecord> {
    let sha256 = cache.sha(path)?;
    let bytes = fs::metadata(path)
        .map_err(|e| Error::io(path.display(), e))?
        .len();
    Ok(FileRecord {
        path: rel.to_string(),
        bytes,
        sha256,
        shared: shared.map(String::from),
    })
}

/// Where `f`'s installed payload sits in a staging (or adoption) tree:
/// `<staging>/<path>`, or `<staging>/.shared/<kind>/<path>` for shared files.
fn stage_dest(staging: &Path, f: &FileSpec) -> PathBuf {
    let rel = safe_rel_path(&f.path).unwrap_or_default();
    match &f.shared {
        Some(kind) => staging.join(".shared").join(kind).join(rel),
        None => staging.join(rel),
    }
}

fn archive_path(staging: &Path, f: &FileSpec) -> PathBuf {
    let name: String = f
        .path
        .chars()
        .map(|c| if c == '/' { '_' } else { c })
        .collect();
    match &f.shared {
        Some(kind) => staging.join(".dl").join("shared").join(kind).join(name),
        None => staging.join(".dl").join(name),
    }
}

/// `.part` location for the artifact of `f` (matches `download_resumable`'s
/// dest + ".part" convention).
fn partial_path(staging: &Path, f: &FileSpec) -> PathBuf {
    let dest = match f.extract {
        None => stage_dest(staging, f),
        Some(_) => archive_path(staging, f),
    };
    crate::download::part_path_for(&dest)
}

/// Partial adoption candidate: the existing file for `f` when it is a
/// regular file matching `f`'s final size + sha256 pins and the staged copy
/// does not already verify. A stale staged file (or `.part`) is cleared so the
/// verified source replaces it.
fn adoptable(
    f: &FileSpec,
    staging: &Path,
    existing: &ExistingFn,
    cache: &mut HashCache,
) -> Option<PathBuf> {
    if matches!(f.extract, Some(Extract::TarGz)) {
        return None;
    }
    let (bytes, sha) = final_pins(f);
    let (bytes, sha) = (bytes?, sha?);
    let dest = stage_dest(staging, f);
    if pins_match(&dest, Some(bytes), Some(sha), cache) {
        return None;
    }
    let src = fs::canonicalize(existing(f)?).ok()?;
    if !src.is_file() || !pins_match(&src, Some(bytes), Some(sha), cache) {
        return None;
    }
    let _ = fs::remove_file(&dest);
    cache.forget(&dest);
    crate::download::discard_partial(&partial_path(staging, f));
    if f.extract.is_some() {
        let _ = fs::remove_file(archive_path(staging, f));
    }
    Some(src)
}

/// Hard-link `src` to `dest` (creating parents); `Ok(false)` when the volume
/// does not allow it.
fn try_hard_link(src: &Path, dest: &Path) -> Result<bool> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| Error::io(parent.display(), e))?;
    }
    Ok(fs::hard_link(src, dest).is_ok())
}

/// Place `src` at `dest` (which must not exist): hard link when the volume
/// allows it, else a copy written beside `dest` and renamed in. Returns
/// whether a hard link was made.
fn link_or_copy(src: &Path, dest: &Path) -> Result<bool> {
    let src = fs::canonicalize(src).map_err(|e| {
        Error::new(
            ErrorCode::NotFound,
            format!("existing file {}: {e}", src.display()),
        )
    })?;
    if !src.is_file() {
        return Err(Error::new(
            ErrorCode::NotFound,
            format!("existing path {} is not a file", src.display()),
        ));
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| Error::io(parent.display(), e))?;
    }
    if fs::hard_link(&src, dest).is_ok() {
        return Ok(true);
    }
    let tmp = crate::download::part_path_for(dest);
    let copied = (|| -> std::io::Result<()> {
        fs::copy(&src, &tmp)?;
        fs::File::open(&tmp)?.sync_all()?;
        fs::rename(&tmp, dest)
    })();
    if let Err(e) = copied {
        let _ = fs::remove_file(&tmp);
        return Err(Error::io(format!("copy {}", src.display()), e));
    }
    Ok(false)
}

/// Put verified `src` at `dst` without replacing anything already there.
fn place_no_clobber(
    src: &Path,
    dst: &Path,
    bytes: Option<u64>,
    sha: Option<&str>,
    cache: &mut HashCache,
) -> Result<()> {
    let existing_ok = |cache: &mut HashCache| -> Result<()> {
        if is_link(dst) {
            return Err(Error::new(
                ErrorCode::Io,
                format!("refusing to follow symlink at {}", dst.display()),
            ));
        }
        if pins_match(dst, bytes, sha, cache) {
            let _ = fs::remove_file(src);
            Ok(())
        } else {
            Err(Error::new(
                ErrorCode::Conflict,
                format!(
                    "shared runtime file {} exists with different bytes; not overwritten",
                    dst.display()
                ),
            ))
        }
    };
    if fs::symlink_metadata(dst).is_ok() {
        return existing_ok(cache);
    }
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).map_err(|e| Error::io(parent.display(), e))?;
    }
    match fs::hard_link(src, dst) {
        Ok(()) => {
            let _ = fs::remove_file(src);
            return Ok(());
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return existing_ok(cache),
        Err(_) => {}
    }
    // Different volume (or no hard links): copy beside the target, then link
    // or rename it in.
    let name = dst
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dst.with_file_name(format!(".{name}.tmp-{}", std::process::id()));
    let copied = (|| -> std::io::Result<()> {
        fs::copy(src, &tmp)?;
        fs::File::open(&tmp)?.sync_all()
    })();
    if let Err(e) = copied {
        let _ = fs::remove_file(&tmp);
        return Err(Error::io(format!("copy to {}", dst.display()), e));
    }
    if !pins_match(&tmp, bytes, sha, cache) {
        let _ = fs::remove_file(&tmp);
        return Err(Error::new(
            ErrorCode::HashMismatch,
            format!("copy to {} fails its pins", dst.display()),
        ));
    }
    let placed = match fs::hard_link(&tmp, dst) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let _ = fs::remove_file(&tmp);
            return existing_ok(cache);
        }
        Err(_) => fs::rename(&tmp, dst),
    };
    let _ = fs::remove_file(&tmp);
    placed.map_err(|e| Error::io(format!("place {}", dst.display()), e))?;
    let _ = fs::remove_file(src);
    Ok(())
}

/// Bytes of every file under `dir` (staged files and `.part`s).
fn tree_bytes(dir: &Path) -> u64 {
    let mut out = Vec::new();
    if walk(dir, dir, &mut out).is_err() {
        return 0;
    }
    out.iter()
        .filter(|(_, rel)| !rel.ends_with("paused.json") && !rel.ends_with(".meta"))
        .filter_map(|(abs, _)| fs::metadata(abs).ok().map(|m| m.len()))
        .sum()
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<(PathBuf, String)>) -> Result<()> {
    for entry in fs::read_dir(dir).map_err(|e| Error::io(dir.display(), e))? {
        let p = entry.map_err(|e| Error::io(dir.display(), e))?.path();
        if p.is_dir() {
            walk(root, &p, out)?;
        } else if p.is_file() {
            let rel = p
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            out.push((p.clone(), rel));
        }
    }
    Ok(())
}

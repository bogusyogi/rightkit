//! Install / upgrade: disk preflight, resumable verified downloads into a
//! persistent staging dir, archive extraction, then an atomic swap +
//! `state.json` activation. Installing a newer revision *is* the upgrade; the
//! previous revision stays on disk for rollback up to `keep_revisions`.

use crate::archive;
use crate::disk;
use crate::download::{download_resumable, http_agent, DownloadRequest};
use crate::error::{Error, ErrorCode, Result};
use crate::hash::sha256_file;
use crate::platform::Platform;
use crate::spec::{safe_rel_path, Extract, FileSpec, ModelSpec};
use crate::store::{FileRecord, RevisionRecord, Store, VerifyMode};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Preflight { needed_bytes: u64, free_bytes: Option<u64> },
    FileStart { path: String, index: usize, total_files: usize },
    Progress { path: String, file_done: u64, file_total: u64, overall_done: u64, overall_total: u64 },
    FileVerified { path: String, sha256: String },
    Extracted { path: String },
    AlreadyInstalled { key: String },
    Activated { key: String, previous: Option<String> },
    Pruned { key: String },
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
    /// Set to `true` from another thread to cancel; `.part` files stay resumable.
    pub cancel: Arc<AtomicBool>,
    /// Revisions kept besides the active one (for rollback).
    pub keep_revisions: usize,
    /// Test/override hook for the free-space probe.
    pub free_bytes_override: Option<u64>,
    /// How long to wait for another process holding the model lock.
    pub lock_wait: Duration,
}

impl Default for InstallOptions {
    fn default() -> Self {
        Self {
            revision: None,
            quant: None,
            platform: Platform::current(),
            authorization: None,
            cancel: Arc::new(AtomicBool::new(false)),
            keep_revisions: 1,
            free_bytes_override: None,
            lock_wait: Duration::ZERO,
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

impl Store {
    pub fn install(
        &self,
        spec: &ModelSpec,
        opts: &InstallOptions,
        mut sink: impl FnMut(Event),
    ) -> Result<InstallOutcome> {
        let rev = spec
            .select(opts.revision.as_deref(), opts.quant.as_deref())
            .ok_or_else(|| Error::new(ErrorCode::NotFound, format!("{}: no matching revision/quant", spec.id)))?;
        let files: Vec<&FileSpec> = rev.files_for(&opts.platform).collect();
        if files.is_empty() {
            return Err(Error::new(ErrorCode::NoFiles, format!("{} {}: no files for this platform", spec.id, rev.key())));
        }
        for f in &files {
            if safe_rel_path(&f.path).is_none() {
                return Err(Error::new(ErrorCode::InvalidSpec, format!("unsafe file path {:?}", f.path)));
            }
        }
        let key = rev.key();
        let id = spec.id.as_str();
        let _lock = self.acquire(id, opts.lock_wait)?;
        let mut state = self.state(id)?;
        state.id = id.to_string();
        let cancelled = || opts.cancel.load(Ordering::Relaxed);

        // Already installed and intact: just (re)activate.
        let installed = state.revisions.contains_key(&key) && self.verify(id, &key, VerifyMode::Full);
        if installed {
            sink(Event::AlreadyInstalled { key: key.clone() });
            let previous = self.activate(&mut state, &key)?;
            sink(Event::Activated { key: key.clone(), previous: previous.clone() });
            self.prune(&mut state, opts.keep_revisions, &mut sink)?;
            return Ok(InstallOutcome { dir: self.revision_dir(id, &key), key, already_installed: true, previous });
        }

        let staging = self.staging_dir(id, &key);
        fs::create_dir_all(staging.join(".dl")).map_err(|e| Error::io(staging.display(), e))?;

        // Disk preflight: remaining payload (minus resumable partials) + 10% headroom.
        let resumable: u64 = files
            .iter()
            .filter_map(|f| fs::metadata(partial_path(&staging, f)).ok().map(|m| m.len()))
            .sum();
        let needed = rev.required_disk_bytes(&opts.platform).saturating_sub(resumable);
        let free = opts.free_bytes_override.or_else(|| disk::free_bytes(&self.root));
        sink(Event::Preflight { needed_bytes: needed, free_bytes: free });
        if let Some(free) = free {
            if needed > 0 && (free as f64) < needed as f64 * 1.1 {
                return Err(Error::new(
                    ErrorCode::DiskSpace,
                    format!("{id}: need ~{:.1} GB free, {:.1} GB available", needed as f64 / 1e9, free as f64 / 1e9),
                ));
            }
        }

        let agent = http_agent();
        let overall_total: u64 = files.iter().filter_map(|f| f.bytes).sum();
        let mut overall_base = 0u64;
        let mut records: Vec<FileRecord> = Vec::new();
        for (index, f) in files.iter().enumerate() {
            if cancelled() {
                return Err(Error::new(ErrorCode::Cancelled, "install cancelled"));
            }
            sink(Event::FileStart { path: f.path.clone(), index, total_files: files.len() });
            let recs = self.fetch_file(&agent, f, &staging, opts, &cancelled, &mut |done, total| {
                sink(Event::Progress {
                    path: f.path.clone(),
                    file_done: done,
                    file_total: total,
                    overall_done: overall_base + done,
                    overall_total,
                });
            })?;
            for r in &recs {
                sink(Event::FileVerified { path: r.path.clone(), sha256: r.sha256.clone() });
            }
            records.extend(recs);
            overall_base += f.bytes.unwrap_or(0);
        }

        // Atomic swap: move any broken same-key dir aside, rename staging in.
        let _ = fs::remove_dir_all(staging.join(".dl"));
        let dest = self.revision_dir(id, &key);
        fs::create_dir_all(dest.parent().unwrap()).map_err(|e| Error::io(dest.display(), e))?;
        if dest.exists() {
            fs::remove_dir_all(&dest).map_err(|e| Error::io(dest.display(), e))?;
        }
        fs::rename(&staging, &dest).map_err(|e| Error::io(format!("activate {}", dest.display()), e))?;
        records.sort_by(|a, b| a.path.cmp(&b.path));
        state.revisions.insert(
            key.clone(),
            RevisionRecord {
                revision: rev.revision.clone(),
                quant: rev.quant.clone(),
                installed_at_unix: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
                files: records,
            },
        );
        let previous = self.activate(&mut state, &key)?;
        sink(Event::Activated { key: key.clone(), previous: previous.clone() });
        self.prune(&mut state, opts.keep_revisions, &mut sink)?;
        Ok(InstallOutcome { dir: dest, key, already_installed: false, previous })
    }

    /// Flip `active` to `key` (recording the old one in history) and persist.
    fn activate(&self, state: &mut crate::store::State, key: &str) -> Result<Option<String>> {
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
    fn prune(&self, state: &mut crate::store::State, keep: usize, sink: &mut impl FnMut(Event)) -> Result<()> {
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
            let _ = fs::remove_dir_all(self.revision_dir(&id, &k));
            sink(Event::Pruned { key: k });
        }
        Ok(())
    }

    fn fetch_file(
        &self,
        agent: &ureq::Agent,
        f: &FileSpec,
        staging: &Path,
        opts: &InstallOptions,
        cancelled: &dyn Fn() -> bool,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<Vec<FileRecord>> {
        let dest = staging.join(safe_rel_path(&f.path).unwrap());
        let download = |dest_path: &Path, part: &Path, progress: &mut dyn FnMut(u64, u64)| -> Result<String> {
            let res = download_resumable(
                agent,
                DownloadRequest {
                    url: &f.url,
                    part_path: part,
                    dest_path,
                    expected_sha256: f.sha256.as_deref(),
                    authorization: opts.authorization.as_deref(),
                    cancelled,
                },
                |d, t| progress(d, t),
            )?;
            let len = fs::metadata(dest_path).map(|m| m.len()).unwrap_or(0);
            if f.bytes.is_some_and(|b| b != len) {
                let _ = fs::remove_file(dest_path);
                return Err(Error::new(
                    ErrorCode::SizeMismatch,
                    format!("{}: expected {} bytes, got {len}", f.path, f.bytes.unwrap()),
                ));
            }
            Ok(res.sha256)
        };
        let record = |path: &Path, rel: &str, known: Option<String>| -> Result<FileRecord> {
            let sha256 = match known {
                Some(s) => s,
                None => sha256_file(path).map_err(|e| Error::new(ErrorCode::Io, e))?,
            };
            let bytes = fs::metadata(path).map_err(|e| Error::io(path.display(), e))?.len();
            Ok(FileRecord { path: rel.to_string(), bytes, sha256 })
        };

        match &f.extract {
            None => {
                if dest.is_file() && pins_match(&dest, f.bytes, f.sha256.as_deref()) {
                    return Ok(vec![record(&dest, &f.path, None)?]);
                }
                let sha = download(&dest, &partial_path(staging, f), progress)?;
                Ok(vec![record(&dest, &f.path, Some(sha))?])
            }
            Some(Extract::Member(member)) => {
                if dest.is_file() && pins_match(&dest, f.installed_bytes, f.installed_sha256.as_deref()) {
                    return Ok(vec![record(&dest, &f.path, None)?]);
                }
                let archive_path = archive_path(staging, f);
                if !(archive_path.is_file() && pins_match(&archive_path, f.bytes, f.sha256.as_deref())) {
                    download(&archive_path, &partial_path(staging, f), progress)?;
                }
                archive::extract_member(&archive_path, member, &dest)?;
                if !pins_match(&dest, f.installed_bytes, f.installed_sha256.as_deref()) {
                    let _ = fs::remove_file(&dest);
                    return Err(Error::new(ErrorCode::HashMismatch, format!("{}: extracted {member} fails its pins", f.path)));
                }
                let _ = fs::remove_file(&archive_path);
                Ok(vec![record(&dest, &f.path, None)?])
            }
            Some(Extract::TarGz) => {
                if !dest.is_dir() {
                    let archive_path = archive_path(staging, f);
                    if !(archive_path.is_file() && pins_match(&archive_path, f.bytes, f.sha256.as_deref())) {
                        download(&archive_path, &partial_path(staging, f), progress)?;
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
                    .map(|(abs, rel)| record(&abs, &format!("{}/{rel}", f.path), None))
                    .collect()
            }
        }
    }
}

fn archive_path(staging: &Path, f: &FileSpec) -> PathBuf {
    let name: String = f.path.chars().map(|c| if c == '/' { '_' } else { c }).collect();
    staging.join(".dl").join(name)
}

/// `.part` location for the artifact of `f` (matches `download_resumable`'s
/// dest + ".part" convention).
fn partial_path(staging: &Path, f: &FileSpec) -> PathBuf {
    let dest = match f.extract {
        None => staging.join(safe_rel_path(&f.path).unwrap_or_default()),
        Some(_) => archive_path(staging, f),
    };
    crate::download::part_path_for(&dest)
}

fn pins_match(path: &Path, bytes: Option<u64>, sha: Option<&str>) -> bool {
    let Ok(meta) = fs::metadata(path) else { return false };
    if bytes.is_some_and(|b| b != meta.len()) {
        return false;
    }
    if bytes.is_none() && sha.is_none() {
        return false;
    }
    sha.is_none_or(|s| sha256_file(path).is_ok_and(|h| h.eq_ignore_ascii_case(s)))
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<(PathBuf, String)>) -> Result<()> {
    for entry in fs::read_dir(dir).map_err(|e| Error::io(dir.display(), e))? {
        let p = entry.map_err(|e| Error::io(dir.display(), e))?.path();
        if p.is_dir() {
            walk(root, &p, out)?;
        } else if p.is_file() {
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            out.push((p.clone(), rel));
        }
    }
    Ok(())
}

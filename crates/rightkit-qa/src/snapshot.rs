//! Snapshot and restore of the owner's real app data around a journey run (rightkit#1437,
//! GATE-CONTRACT GC-A10).
//!
//! Adrian, 2026-11-03: "dev mode, is mine. im the fucking owner, my data. no dummy data, no fucking
//! made up shit. real data. snapshot and restore." A journey runs on the installed dev app and its
//! real data. This module is what keeps that safe:
//!
//! 1. `arm` refuses to start while the app runs, then claims one restore marker per app
//!    (`<managed root>/snapshots/<app>/restore-marker.json`, created with `create_new`) **before it
//!    touches anything**. The marker names the snapshot id, app, owner process, live paths and, once
//!    the copy is verified, the sha256 of the manifest.
//! 2. It copies every data root (resolved from `rightkit-fs` `AppDirs` for the app name, or declared
//!    in `[snapshot] extra_paths`) into `<managed root>/snapshots/<app>/<id>/roots/<n>`, hashes the
//!    copy against the source, re-reads the source to prove it did not change while it was copied,
//!    and exports the declared credential items through `rightkit-credentials` (the real OS store)
//!    into an owner-only file in the same directory. Any failure before the marker reads `armed`
//!    deletes only the snapshot's own files and changes nothing live.
//! 3. `Armed::restore` (also run from `Drop`, so a failure, a panic or an early return restores)
//!    puts every root back to the manifest exactly (content, kind, symlink targets, unix modes,
//!    nothing extra), re-puts or deletes each credential item, verifies all of it against the
//!    manifest, and only then removes the marker and the snapshot.
//!    The same applies to the suite shared folder (`suite_shared = true`: `Orthic Labs/shared`, the
//!    shared model store and service records) and to an Android app's package data on a device
//!    (`[snapshot.android]`, see [`crate::snapshot_android`]), which is copied and restored with `adb`
//!    and `run-as` and verified against a path and file-hash listing.
//! 4. A kill or crash leaves the marker. `recover_all` runs at the next `rightkit-qa` start, before
//!    anything else: state `snapshotting` discards the half-built snapshot, `armed` or `restoring`
//!    restores. A marker whose owner process is still alive is left alone.
//!
//! Nothing live is deleted unless a verified snapshot exists: the manifest hash is checked against
//! the marker, and the snapshot copy is re-hashed against the manifest, before the first live write.
//! Credential values are held only in the snapshot directory (owner-only, removed after restore).

use crate::config::SnapshotConfig;
use crate::snapshot_android::AndroidRecord;
use crate::util::{base64_decode, base64_encode, err, new_id, now_iso, sha256_hex, Error, Result};
use rightkit_credentials::{Config as CredConfig, Namespace, Secret, Store, WindowsTarget};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

/// The restore marker's file name, one per app under `<managed root>/snapshots/<app>/`.
pub const MARKER_FILE: &str = "restore-marker.json";
const SNAPSHOTS_DIR: &str = "snapshots";
const MANIFEST_FILE: &str = "manifest.json";
const CREDENTIALS_FILE: &str = "credentials.json";
const ROOTS_DIR: &str = "roots";
const SCHEMA: u32 = 1;
const STATE_SNAPSHOTTING: &str = "snapshotting";
const STATE_ARMED: &str = "armed";
const STATE_RESTORING: &str = "restoring";

mod owner;
pub use owner::OwnerConfig;

// ------------------------------------------------------------------------------------ records

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    /// Path under the root, `/`-separated.
    rel: String,
    /// `dir`, `file`, `symlink` or `other` (a socket or pipe: never restored, never compared).
    kind: String,
    size: u64,
    sha256: Option<String>,
    target: Option<String>,
    /// Unix permission bits; `None` elsewhere.
    mode: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RootRecord {
    index: usize,
    live_path: String,
    /// Whether the folder existed when the snapshot was taken. A folder that did not exist is
    /// removed again by the restore.
    existed: bool,
    /// The root is one file (`extra_paths` entry that is a file), not a folder. Its copy is a
    /// folder holding that file under its own name.
    #[serde(default)]
    file: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CredRecord {
    service: String,
    windows_target: Option<String>,
    windows_prefix: Option<String>,
    accounts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Marker {
    schema: u32,
    snapshot_id: String,
    app: String,
    state: String,
    owner_pid: u32,
    owner_command: Option<String>,
    created_at: String,
    snapshot_dir: String,
    roots: Vec<RootRecord>,
    credentials: Vec<CredRecord>,
    manifest_sha256: Option<String>,
    credentials_sha256: Option<String>,
    /// Installed credential owner & transport, retained for crash recovery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    credential_owner: Option<OwnerConfig>,
    #[serde(default)]
    process_names: Vec<String>,
    /// The Android package whose data dir the snapshot also holds (`[snapshot.android]`).
    #[serde(default)]
    android: Option<AndroidRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    /// Root index (decimal text) to its entries, sorted by `rel`.
    roots: BTreeMap<String, Vec<Entry>>,
    /// The hashes of the Android data copy, so the manifest's own hash binds it too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    android: Option<AndroidDigest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AndroidDigest {
    package: String,
    serial: String,
    tar_sha256: String,
    listing_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
struct CredItem {
    /// Index into the marker's `credentials`.
    set: usize,
    account: String,
    present: bool,
    /// Standard base64 of the secret bytes.
    value: Option<String>,
    sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
struct CredFile {
    items: Vec<CredItem>,
}

/// What a restore (or a recovery) did.
#[derive(Debug, Clone, Default)]
pub struct RestoreReport {
    pub app: String,
    pub snapshot_id: String,
    pub roots_restored: usize,
    pub credentials_restored: usize,
    /// The Android app's data dir was restored and its listing verified.
    pub android_restored: bool,
    /// sha256 of the snapshot's `manifest.json` that the live data was verified against.
    pub manifest_sha256: String,
    /// sha256 of the credential snapshot file, when the app declares credential items.
    pub credentials_sha256: Option<String>,
    /// The marker read `snapshotting`: nothing live had been changed, the half-built snapshot was
    /// discarded.
    pub discarded: bool,
}

impl RestoreReport {
    pub fn describe(&self) -> String {
        if self.discarded {
            format!(
                "snapshot {} of {}: discarded an unfinished snapshot, live data was never touched",
                self.snapshot_id, self.app
            )
        } else {
            format!(
                "snapshot {} of {}: restored {} data folder(s), {} credential item(s){}, verified against the manifest",
                self.snapshot_id,
                self.app,
                self.roots_restored,
                self.credentials_restored,
                if self.android_restored {
                    " and the Android app data"
                } else {
                    ""
                }
            )
        }
    }
}

// ------------------------------------------------------------------------------------ armed registry

/// The environment variable through which a runner tells a scenario worker which armed snapshot it
/// runs under: the path of the restore marker.
pub const MARKER_ENV: &str = "RIGHTKIT_QA_SNAPSHOT_MARKER";

/// Markers of the snapshots armed in this process.
static ARMED: Mutex<Vec<(PathBuf, String)>> = Mutex::new(Vec::new());

fn armed_list() -> std::sync::MutexGuard<'static, Vec<(PathBuf, String)>> {
    ARMED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The marker path an armed snapshot of this process holds, for a child the runner starts
/// ([`MARKER_ENV`]).
pub fn armed_marker_path() -> Option<PathBuf> {
    armed_list().first().map(|(p, _)| p.clone())
}

/// The id of the snapshot armed in this process, for a child the runner starts
/// (`RIGHTKIT_JOURNEYS_SNAPSHOT_ID`).
pub fn armed_snapshot_id() -> Option<String> {
    armed_list().first().map(|(_, id)| id.clone())
}

/// Refuse a launch of an app or its engine on the owner's real data unless a verified snapshot is
/// armed: in this process, or (a scenario worker) named by [`MARKER_ENV`] and still `armed`. This
/// replaces the old `userdata::Guard`: nothing launches on real data without a snapshot to restore.
pub fn require_armed() -> Result<()> {
    if !armed_list().is_empty() {
        return Ok(());
    }
    if let Ok(path) = std::env::var(MARKER_ENV) {
        if let Ok(m) = read_marker(Path::new(&path)) {
            if m.state == STATE_ARMED && owner_alive(&m) {
                return Ok(());
            }
        }
    }
    err("refusing to launch: no verified snapshot is armed. QA runs on the owner's real data (rightkit#1437) and must snapshot and restore it: declare [snapshot] in rightkit-qa.toml and run through rightkit-qa, or arm one with rightkit_qa::snapshot::arm (Harness::with_snapshot)")
}

// ------------------------------------------------------------------------------------ helpers

fn ctx<T>(r: std::io::Result<T>, what: &str, p: &Path) -> Result<T> {
    r.map_err(|e| Error(format!("{what} {}: {e}", p.display())))
}

fn fs_ctx<T>(r: rightkit_fs::Result<T>, what: &str, p: &Path) -> Result<T> {
    r.map_err(|e| Error(format!("{what} {}: {e}", p.display())))
}

fn utf8(p: &Path) -> Result<&str> {
    match p.to_str() {
        Some(s) => Ok(s),
        None => err(format!("{} is not valid UTF-8", p.display())),
    }
}

fn rel_path(rel: &str) -> PathBuf {
    rel.split('/').collect()
}

#[cfg(windows)]
fn refuse_reparse(path: &Path, md: &fs::Metadata) -> Result<()> {
    use std::os::windows::fs::MetadataExt;
    if md.file_attributes() & 0x400 != 0 {
        return err(format!(
            "{} is a symbolic link, junction or other reparse point; the snapshot does not follow or recreate those on Windows, so it refuses to start",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(not(windows))]
fn refuse_reparse(_path: &Path, _md: &fs::Metadata) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn mode_of(md: &fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    Some(md.permissions().mode() & 0o7777)
}

#[cfg(not(unix))]
fn mode_of(_md: &fs::Metadata) -> Option<u32> {
    None
}

#[cfg(unix)]
fn make_symlink(target: &str, to: &Path) -> Result<()> {
    ctx(
        std::os::unix::fs::symlink(target, to),
        "cannot create symlink",
        to,
    )
}

#[cfg(not(unix))]
fn make_symlink(_target: &str, to: &Path) -> Result<()> {
    err(format!(
        "cannot create symlink {}: not supported on this platform",
        to.display()
    ))
}

/// Windows: a read-only file cannot be replaced or deleted until the flag is cleared. Unix needs
/// nothing (the directory's mode decides), and clearing the flag there would widen the mode.
#[cfg(windows)]
fn clear_readonly(p: &Path) {
    if let Ok(md) = fs::symlink_metadata(p) {
        let mut perm = md.permissions();
        if perm.readonly() {
            #[allow(clippy::permissions_set_readonly_false)]
            perm.set_readonly(false);
            let _ = fs::set_permissions(p, perm);
        }
    }
}

#[cfg(not(windows))]
fn clear_readonly(_p: &Path) {}

fn remove_tree(path: &Path) -> Result<()> {
    if fs::remove_dir_all(path).is_ok() {
        return Ok(());
    }
    // A read-only file or folder (Windows) blocks the delete: clear the flags and retry once.
    fn clear_all(dir: &Path) {
        clear_readonly(dir);
        if let Ok(rd) = fs::read_dir(dir) {
            for item in rd.flatten() {
                let p = item.path();
                match fs::symlink_metadata(&p) {
                    Ok(md) if md.is_dir() && !md.file_type().is_symlink() => clear_all(&p),
                    _ => clear_readonly(&p),
                }
            }
        }
    }
    clear_all(path);
    ctx(fs::remove_dir_all(path), "cannot remove", path)
}

fn remove_any(path: &Path, md: &fs::Metadata) -> Result<()> {
    if md.is_dir() && !md.file_type().is_symlink() {
        remove_tree(path)
    } else {
        clear_readonly(path);
        ctx(fs::remove_file(path), "cannot remove", path)
    }
}

fn kind_of(md: &fs::Metadata) -> &'static str {
    let ft = md.file_type();
    if ft.is_symlink() {
        "symlink"
    } else if ft.is_dir() {
        "dir"
    } else if ft.is_file() {
        "file"
    } else {
        "other"
    }
}

// ------------------------------------------------------------------------------------ scanning

/// Every entry under `root`, sorted by `rel` (a parent always sorts before its children). Never
/// follows a symlink.
fn scan(root: &Path) -> Result<Vec<Entry>> {
    let mut out = Vec::new();
    scan_dir(root, "", &mut out)?;
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(out)
}

fn scan_dir(dir: &Path, prefix: &str, out: &mut Vec<Entry>) -> Result<()> {
    for item in ctx(fs::read_dir(dir), "cannot read", dir)? {
        let item = ctx(item, "cannot read", dir)?;
        let path = item.path();
        let name = item.file_name();
        let Some(name) = name.to_str() else {
            return err(format!(
                "{} has a name that is not valid UTF-8",
                path.display()
            ));
        };
        let rel = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}/{name}")
        };
        let md = ctx(fs::symlink_metadata(&path), "cannot stat", &path)?;
        refuse_reparse(&path, &md)?;
        let kind = kind_of(&md);
        match kind {
            "symlink" => {
                let target = ctx(fs::read_link(&path), "cannot read link", &path)?;
                let target = utf8(&target)?.to_string();
                out.push(Entry {
                    rel,
                    kind: kind.into(),
                    size: 0,
                    sha256: None,
                    target: Some(target),
                    mode: None,
                });
            }
            "dir" => {
                out.push(Entry {
                    rel: rel.clone(),
                    kind: kind.into(),
                    size: 0,
                    sha256: None,
                    target: None,
                    mode: mode_of(&md),
                });
                scan_dir(&path, &rel, out)?;
            }
            "file" => {
                let sha = ctx(
                    rightkit_fs::digest::sha256_file(&path),
                    "cannot hash",
                    &path,
                )?;
                out.push(Entry {
                    rel,
                    kind: kind.into(),
                    size: md.len(),
                    sha256: Some(sha),
                    target: None,
                    mode: mode_of(&md),
                });
            }
            _ => out.push(Entry {
                rel,
                kind: kind.into(),
                size: 0,
                sha256: None,
                target: None,
                mode: None,
            }),
        }
    }
    Ok(())
}

/// Differences between `expected` and `actual` (at most 20 lines). `other` entries are ignored on
/// both sides. Modes are compared only when `modes` is set.
fn diff(expected: &[Entry], actual: &[Entry], modes: bool) -> Vec<String> {
    let index = |v: &'_ [Entry]| -> BTreeMap<String, Entry> {
        v.iter()
            .filter(|e| e.kind != "other")
            .map(|e| (e.rel.clone(), e.clone()))
            .collect()
    };
    let ex = index(expected);
    let ac = index(actual);
    let mut out = Vec::new();
    for (rel, e) in &ex {
        match ac.get(rel) {
            None => out.push(format!("missing {rel}")),
            Some(a) => {
                if a.kind != e.kind
                    || a.size != e.size
                    || a.sha256 != e.sha256
                    || a.target != e.target
                    || (modes && a.mode != e.mode)
                {
                    out.push(format!("differs {rel}"));
                }
            }
        }
        if out.len() >= 20 {
            return out;
        }
    }
    for rel in ac.keys() {
        if !ex.contains_key(rel) {
            out.push(format!("extra {rel}"));
            if out.len() >= 20 {
                break;
            }
        }
    }
    out
}

fn copy_tree(src: &Path, dst: &Path, entries: &[Entry]) -> Result<()> {
    ctx(fs::create_dir_all(dst), "cannot create", dst)?;
    for e in entries {
        let from = src.join(rel_path(&e.rel));
        let to = dst.join(rel_path(&e.rel));
        match e.kind.as_str() {
            "dir" => ctx(fs::create_dir_all(&to), "cannot create", &to)?,
            "file" => {
                ctx(fs::copy(&from, &to), "cannot copy", &from)?;
            }
            "symlink" => make_symlink(e.target.as_deref().unwrap_or(""), &to)?,
            _ => {}
        }
    }
    Ok(())
}

// ------------------------------------------------------------------------------------ restore of one root

fn restore_file(from: &Path, to: &Path) -> Result<()> {
    if let Ok(md) = fs::symlink_metadata(to) {
        if md.is_dir() || md.file_type().is_symlink() {
            remove_any(to, &md)?;
        } else {
            clear_readonly(to);
        }
    }
    let Some(name) = to.file_name().and_then(|n| n.to_str()) else {
        return err(format!("{} has no usable file name", to.display()));
    };
    let tmp = to.with_file_name(format!("{name}.rkqa-restore"));
    ctx(fs::copy(from, &tmp), "cannot copy", from)?;
    ctx(fs::rename(&tmp, to), "cannot put back", to)
}

/// Delete everything under `dir` that the manifest does not list (children before parents).
fn prune_extras(dir: &Path, prefix: &str, expected: &BTreeMap<&str, &Entry>) -> Result<()> {
    for item in ctx(fs::read_dir(dir), "cannot read", dir)? {
        let item = ctx(item, "cannot read", dir)?;
        let path = item.path();
        let name = item.file_name().to_string_lossy().into_owned();
        let rel = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let md = ctx(fs::symlink_metadata(&path), "cannot stat", &path)?;
        let keep = expected
            .get(rel.as_str())
            .is_some_and(|e| e.kind == kind_of(&md));
        if !keep {
            remove_any(&path, &md)?;
        } else if md.is_dir() && !md.file_type().is_symlink() {
            prune_extras(&path, &rel, expected)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn apply_modes(live: &Path, entries: &[Entry]) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    for e in entries.iter().rev() {
        if e.kind != "dir" && e.kind != "file" {
            continue;
        }
        let Some(mode) = e.mode else { continue };
        let p = live.join(rel_path(&e.rel));
        let md = ctx(fs::symlink_metadata(&p), "cannot stat", &p)?;
        if md.permissions().mode() & 0o7777 != mode {
            ctx(
                fs::set_permissions(&p, fs::Permissions::from_mode(mode)),
                "cannot set the mode of",
                &p,
            )?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn apply_modes(_live: &Path, _entries: &[Entry]) -> Result<()> {
    Ok(())
}

/// The one entry of a file root: the file under its own name.
fn file_entry(path: &Path) -> Result<Entry> {
    let md = ctx(fs::symlink_metadata(path), "cannot stat", path)?;
    refuse_reparse(path, &md)?;
    if !md.file_type().is_file() {
        return err(format!(
            "{} is not a plain file (a symbolic link or folder cannot be a file root); refusing it",
            path.display()
        ));
    }
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Error(format!("{} has no usable file name", path.display())))?
        .to_string();
    Ok(Entry {
        rel: name,
        kind: "file".into(),
        size: md.len(),
        sha256: Some(ctx(
            rightkit_fs::digest::sha256_file(path),
            "cannot hash",
            path,
        )?),
        target: None,
        mode: mode_of(&md),
    })
}

fn restore_file_root(
    live: &Path,
    snap_root: &Path,
    existed: bool,
    entries: &[Entry],
) -> Result<()> {
    if !existed {
        // The file did not exist when the snapshot was taken: whatever is there was made by the run.
        if let Ok(md) = fs::symlink_metadata(live) {
            remove_any(live, &md)?;
        }
        return Ok(());
    }
    let Some(e) = entries.first() else {
        return err("the snapshot manifest holds no entry for a file root");
    };
    let from = snap_root.join(rel_path(&e.rel));
    let same = fs::symlink_metadata(live)
        .map(|m| m.file_type().is_file() && m.len() == e.size)
        .unwrap_or(false)
        && rightkit_fs::digest::sha256_file(live).ok() == e.sha256;
    if !same {
        if let Some(parent) = live.parent() {
            ctx(fs::create_dir_all(parent), "cannot create", parent)?;
        }
        restore_file(&from, live)?;
    }
    #[cfg(unix)]
    if let Some(mode) = e.mode {
        use std::os::unix::fs::PermissionsExt;
        ctx(
            fs::set_permissions(live, fs::Permissions::from_mode(mode)),
            "cannot set the mode of",
            live,
        )?;
    }
    let after = file_entry(live)?;
    let problems = diff(entries, &[after], true);
    if !problems.is_empty() {
        return err(format!(
            "{} does not match the snapshot after the restore: {}",
            live.display(),
            problems.join("; ")
        ));
    }
    Ok(())
}

fn restore_root(live: &Path, snap_root: &Path, existed: bool, entries: &[Entry]) -> Result<()> {
    if !existed {
        // The folder did not exist when the snapshot was taken: whatever is there now was made
        // during the run.
        if let Ok(md) = fs::symlink_metadata(live) {
            remove_any(live, &md)?;
        }
        return Ok(());
    }
    match fs::symlink_metadata(live) {
        Ok(md) if md.is_dir() && !md.file_type().is_symlink() => {}
        Ok(md) => {
            remove_any(live, &md)?;
            ctx(fs::create_dir_all(live), "cannot create", live)?;
        }
        Err(_) => ctx(fs::create_dir_all(live), "cannot create", live)?,
    }
    // A: put back everything that is missing or different (parents first).
    for e in entries {
        let to = live.join(rel_path(&e.rel));
        let from = snap_root.join(rel_path(&e.rel));
        let md = fs::symlink_metadata(&to).ok();
        match e.kind.as_str() {
            "dir" => match md {
                Some(m) if m.is_dir() && !m.file_type().is_symlink() => {}
                Some(m) => {
                    remove_any(&to, &m)?;
                    ctx(fs::create_dir(&to), "cannot create", &to)?;
                }
                None => ctx(fs::create_dir_all(&to), "cannot create", &to)?,
            },
            "file" => {
                let same = md
                    .as_ref()
                    .is_some_and(|m| m.file_type().is_file() && m.len() == e.size)
                    && rightkit_fs::digest::sha256_file(&to).ok() == e.sha256;
                if !same {
                    restore_file(&from, &to)?;
                }
            }
            "symlink" => {
                let want = e.target.as_deref().unwrap_or("");
                let same = md.as_ref().is_some_and(|m| m.file_type().is_symlink())
                    && fs::read_link(&to)
                        .ok()
                        .and_then(|t| t.to_str().map(str::to_string))
                        .as_deref()
                        == Some(want);
                if !same {
                    if let Some(m) = md {
                        remove_any(&to, &m)?;
                    }
                    make_symlink(want, &to)?;
                }
            }
            _ => {}
        }
    }
    // B: delete what the run added.
    let expected: BTreeMap<&str, &Entry> = entries
        .iter()
        .filter(|e| e.kind != "other")
        .map(|e| (e.rel.as_str(), e))
        .collect();
    prune_extras(live, "", &expected)?;
    // C: modes, then D: verify against the manifest.
    apply_modes(live, entries)?;
    let after = scan(live)?;
    let problems = diff(entries, &after, true);
    if !problems.is_empty() {
        return err(format!(
            "{} does not match the snapshot after the restore: {}",
            live.display(),
            problems.join("; ")
        ));
    }
    Ok(())
}

// ------------------------------------------------------------------------------------ credentials

fn cred_err(e: rightkit_credentials::Error) -> Error {
    Error(e.to_string())
}

fn store_for(c: &CredRecord) -> Result<Store> {
    let ns = Namespace::new(c.service.clone()).map_err(cred_err)?;
    let ns = match c.windows_target.as_deref() {
        None | Some("slash") => match &c.windows_prefix {
            Some(prefix) => ns
                .try_windows_target(WindowsTarget::Slash {
                    prefix: Some(prefix.clone()),
                })
                .map_err(cred_err)?,
            None => ns,
        },
        Some("keyring-compat") => ns
            .try_windows_target(WindowsTarget::KeyringCompat)
            .map_err(cred_err)?,
        Some(other) => {
            return err(format!(
                "[snapshot.credentials] windows_target {other:?} is not slash or keyring-compat"
            ))
        }
    };
    // Non-interactive by default: a keychain that wants a prompt answers an error, and the
    // snapshot refuses to start instead of waiting on a dialog.
    Ok(Store::open(CredConfig::new(ns)))
}

fn read_credentials(sets: &[CredRecord]) -> Result<CredFile> {
    let mut items = Vec::new();
    for (set, c) in sets.iter().enumerate() {
        let store = store_for(c)?;
        for account in &c.accounts {
            match store.get(account) {
                Ok(Some(secret)) => {
                    if secret.is_empty() {
                        return err(format!(
                            "credential {}/{account} is stored empty; the snapshot cannot put an empty value back, so it refuses to start",
                            c.service
                        ));
                    }
                    items.push(CredItem {
                        set,
                        account: account.clone(),
                        present: true,
                        value: Some(base64_encode(secret.expose_bytes())),
                        sha256: Some(sha256_hex(secret.expose_bytes())),
                    });
                }
                Ok(None) => items.push(CredItem {
                    set,
                    account: account.clone(),
                    present: false,
                    value: None,
                    sha256: None,
                }),
                Err(e) => {
                    return err(format!(
                        "cannot read credential {}/{account} for the snapshot: {e}. Nothing was changed.",
                        c.service
                    ))
                }
            }
        }
    }
    Ok(CredFile { items })
}

// Validate every item before ANY live data or credential mutation. A hash alone does not
// prove that absent items, accounts, set indices or base64 payloads are safe to restore.
fn validate_credentials(sets: &[CredRecord], file: &CredFile) -> Result<()> {
    let expected: Vec<_> = sets
        .iter()
        .enumerate()
        .flat_map(|(set, record)| {
            record
                .accounts
                .iter()
                .map(move |account| (set, account.as_str()))
        })
        .collect();
    let unique: BTreeSet<_> = expected.iter().copied().collect();
    if unique.len() != expected.len() || file.items.len() != expected.len() {
        return err("credential snapshot item count or declared account uniqueness is invalid");
    }
    for (item, (set, account)) in file.items.iter().zip(expected) {
        if item.set != set || item.account != account {
            return err("credential snapshot set/account list differs from the declared list");
        }
        if item.present {
            let encoded = item
                .value
                .as_deref()
                .ok_or_else(|| Error("credential snapshot lacks a present value".into()))?;
            let bytes = base64_decode(encoded)
                .ok_or_else(|| Error("credential snapshot has invalid base64".into()))?;
            if bytes.is_empty()
                || base64_encode(&bytes) != encoded
                || item.sha256.as_deref() != Some(sha256_hex(&bytes).as_str())
            {
                return err(
                    "credential snapshot has an empty, noncanonical or damaged present value",
                );
            }
        } else if item.value.is_some() || item.sha256.is_some() {
            return err("credential snapshot absent item carries a value or hash");
        }
    }
    Ok(())
}

fn load_credentials(
    marker: &Marker,
    snap_dir: &Path,
    expected: Option<&str>,
) -> Result<(CredFile, String)> {
    let path = snap_dir.join(CREDENTIALS_FILE);
    fs_ctx(
        rightkit_fs::private::verify_owner_only(&path),
        "credential file is not private",
        &path,
    )?;
    let md = ctx(
        fs::symlink_metadata(&path),
        "cannot stat credential file",
        &path,
    )?;
    refuse_reparse(&path, &md)?;
    if !md.is_file() || md.len() > 16 * 1024 * 1024 {
        return err("credential snapshot must be a regular file within the size limit");
    }
    let bytes = ctx(fs::read(&path), "cannot read credential file", &path)?;
    let hash = sha256_hex(&bytes);
    if expected.is_some_and(|want| want != hash) {
        return err(
            "credential snapshot does not match the restore marker; live data was NOT touched",
        );
    }
    let file = parse_credentials(&bytes, &marker.credentials)?;
    Ok((file, hash))
}

fn parse_credentials(bytes: &[u8], sets: &[CredRecord]) -> Result<CredFile> {
    // Nullable fields remain mandatory: Option<T> alone would accept a missing JSON key.
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|_| Error("credential snapshot JSON has an invalid shape".into()))?;
    let object = value
        .as_object()
        .ok_or_else(|| Error("credential snapshot must be an object".into()))?;
    let items = object
        .get("items")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| Error("credential snapshot must hold an item array".into()))?;
    if object.len() != 1
        || items.iter().any(|item| {
            item.as_object().is_none_or(|object| {
                object.len() != 5
                    || ["set", "account", "present", "value", "sha256"]
                        .iter()
                        .any(|key| !object.contains_key(*key))
            })
        })
    {
        return err("credential snapshot JSON fields differ from its schema");
    }
    let file: CredFile = serde_json::from_slice(bytes)
        .map_err(|_| Error("credential snapshot JSON has an invalid shape".into()))?;
    validate_credentials(sets, &file)?;
    Ok(file)
}

fn single_component(value: &str) -> bool {
    let mut parts = Path::new(value).components();
    matches!(parts.next(), Some(Component::Normal(_))) && parts.next().is_none()
}

fn ensure_stopped(marker: &Marker) -> Result<()> {
    if marker
        .process_names
        .iter()
        .all(|name| name.trim().is_empty())
    {
        return Ok(());
    }
    let running = running_processes(&marker.process_names)?;
    if !running.is_empty() {
        return err(format!(
            "snapshot refuses while configured app/helper processes run ({}); marker retained",
            running.join(", ")
        ));
    }
    Ok(())
}

/// The restore side's check: a run's own app and its helpers may still be exiting when the restore begins (hr-r2-ultra-3: the QA-staged
/// HeardRightMenuBar was gone moments after the check refused), so it waits a bounded time for them to leave. It never stops a process; one
/// still there after the bound refuses exactly as `ensure_stopped` does. The snapshot side (arm) keeps the instant check: a process alive while
/// live data was copied must refuse the snapshot, not be waited out.
fn ensure_stopped_before_restore(marker: &Marker) -> Result<()> {
    let deadline = std::time::Instant::now() + STOPPED_WAIT;
    loop {
        match ensure_stopped(marker) {
            Ok(()) => return Ok(()),
            Err(e) if std::time::Instant::now() >= deadline => return Err(e),
            Err(_) => std::thread::sleep(Duration::from_millis(250)),
        }
    }
}

/// How long the restore side waits for configured app/helper processes that are already exiting.
const STOPPED_WAIT: Duration = Duration::from_secs(15);

fn restore_credentials(sets: &[CredRecord], file: &CredFile) -> Result<usize> {
    validate_credentials(sets, file)?;
    let mut count = 0;
    for item in &file.items {
        let Some(c) = sets.get(item.set) else {
            return err("the credential snapshot names a set the marker does not have");
        };
        let store = store_for(c)?;
        if item.present {
            let bytes = item
                .value
                .as_deref()
                .and_then(base64_decode)
                .ok_or_else(|| {
                    Error("a credential value in the snapshot is not valid base64".into())
                })?;
            if Some(sha256_hex(&bytes)) != item.sha256 {
                return err(format!(
                    "credential {}/{} in the snapshot fails its hash; it was not restored",
                    c.service, item.account
                ));
            }
            let secret = Secret::from_bytes(bytes);
            store.set(&item.account, &secret).map_err(|e| {
                Error(format!(
                    "cannot restore credential {}/{}: {e}",
                    c.service, item.account
                ))
            })?;
            match store.get(&item.account) {
                Ok(Some(now)) if now.ct_eq(&secret) => {}
                Ok(_) => {
                    return err(format!(
                        "credential {}/{} does not hold the snapshot value after the restore",
                        c.service, item.account
                    ))
                }
                Err(e) => {
                    return err(format!(
                        "cannot verify credential {}/{}: {e}",
                        c.service, item.account
                    ))
                }
            }
        } else {
            store.delete(&item.account).map_err(|e| {
                Error(format!(
                    "cannot remove credential {}/{} the run created: {e}",
                    c.service, item.account
                ))
            })?;
            match store.get(&item.account) {
                Ok(None) => {}
                Ok(Some(_)) => {
                    return err(format!(
                        "credential {}/{} still exists after the restore; it was absent in the snapshot",
                        c.service, item.account
                    ))
                }
                Err(e) => {
                    return err(format!(
                        "cannot verify credential {}/{}: {e}",
                        c.service, item.account
                    ))
                }
            }
        }
        count += 1;
    }
    Ok(count)
}

// ------------------------------------------------------------------------------------ app running

fn norm_name(n: &str) -> String {
    let base = n.rsplit(['/', '\\']).next().unwrap_or(n).trim();
    let lower = base.to_ascii_lowercase();
    match lower.strip_suffix(".exe") {
        Some(stem) => stem.to_string(),
        None => lower,
    }
}

#[cfg(unix)]
fn list_process_names() -> Result<Vec<String>> {
    let out = crate::process::output_bounded(
        Command::new("ps").args(["-axo", "comm="]),
        Duration::from_secs(15),
    )?
    .ok_or_else(|| {
        Error("ps timed out; the snapshot cannot prove the app is not running".into())
    })?;
    if !out.status.success() {
        return err("ps failed; the snapshot cannot prove the app is not running");
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

#[cfg(windows)]
fn list_process_names() -> Result<Vec<String>> {
    use std::os::windows::process::CommandExt;
    let mut cmd = Command::new("tasklist");
    cmd.args(["/FO", "CSV", "/NH"]).creation_flags(0x0800_0000);
    let out =
        crate::process::output_bounded(&mut cmd, Duration::from_secs(30))?.ok_or_else(|| {
            Error("tasklist timed out; the snapshot cannot prove the app is not running".into())
        })?;
    if !out.status.success() {
        return err("tasklist failed; the snapshot cannot prove the app is not running");
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            l.trim()
                .strip_prefix('"')
                .and_then(|rest| rest.split('"').next())
                .map(str::to_string)
        })
        .collect())
}

#[cfg(not(any(unix, windows)))]
fn list_process_names() -> Result<Vec<String>> {
    err("the snapshot cannot list processes on this platform")
}

/// The running processes whose image name is one of `names`.
fn running_processes(names: &[String]) -> Result<Vec<String>> {
    let want: Vec<String> = names.iter().map(|n| norm_name(n)).collect();
    Ok(list_process_names()?
        .into_iter()
        .filter(|p| want.contains(&norm_name(p)))
        .collect())
}

// ------------------------------------------------------------------------------------ roots

/// A data root must be an absolute, normal path deep enough not to be a home or a system folder,
/// and must not overlap the managed QA root that holds the snapshot.
fn check_root_sane(path: &Path, managed: &Path, file: bool) -> Result<()> {
    if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        return err(format!(
            "data root {} is not an absolute path without `..`",
            path.display()
        ));
    }
    let normal = path
        .components()
        .filter(|c| matches!(c, Component::Normal(_)))
        .count();
    if normal < 3 {
        return err(format!(
            "data root {} is too shallow to be an app folder; refusing it",
            path.display()
        ));
    }
    if let Ok(home) = rightkit_fs::account_home() {
        // A file may sit directly in the home (`~/.zshrc`); a folder may not be the home, an
        // ancestor of it or a direct child of it.
        let direct_child = !file && path.parent() == Some(home.as_path());
        if path == home || home.starts_with(path) || direct_child {
            return err(format!(
                "data root {} is the user's home, one of its ancestors or a direct child of it; refusing it",
                path.display()
            ));
        }
    }
    if path.starts_with(managed) || managed.starts_with(path) {
        return err(format!(
            "data root {} overlaps the managed QA root {}; refusing it",
            path.display(),
            managed.display()
        ));
    }
    Ok(())
}

fn resolve_app_dirs(name: &str, flat: bool) -> Result<rightkit_fs::AppDirs> {
    // The id is built once per run from the config; the leak is a few bytes.
    let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
    let mut id = rightkit_fs::AppId::new(leaked);
    if flat {
        id = id.without_org();
    }
    let dirs = rightkit_fs::AppDirs::resolve(id)
        .map_err(|e| Error(format!("cannot resolve the data folders of {name}: {e}")))?;
    Ok(dirs)
}

/// The app's real data folder (`AppDirs` data for `app_name`, else the first of `extra_paths`).
/// Scenarios and steps read the app's logs and state from here.
pub fn app_data_dir(cfg: &SnapshotConfig) -> Result<PathBuf> {
    if let Some(name) = cfg.app_name.as_deref().filter(|n| !n.trim().is_empty()) {
        return Ok(resolve_app_dirs(name, cfg.flat)?.data);
    }
    match cfg.extra_paths.first() {
        Some(p) => Ok(extra_entry(p)?.0),
        None => err("[snapshot] names no data folder"),
    }
}

/// The suite shared folder (`Orthic Labs/shared`): the one `rightkit_daemon::Suite` resolves for the
/// account (not from the environment), which is where the suite's apps keep the shared model store
/// and the service records.
fn suite_shared_dir() -> Result<PathBuf> {
    let suite = rightkit_daemon::Suite::per_user_default()
        .map_err(|e| Error(format!("cannot resolve the suite shared folder: {e}")))?;
    Ok(suite.root().to_path_buf())
}

/// An `extra_paths` entry as a path: `~/x` is under the account home (`rightkit_fs::account_home`,
/// never the environment). The entry names a file when it exists as one, or, when absent, when its
/// last component has an extension and it does not end with `/` (end a folder with `/` to force it).
fn extra_entry(raw: &str) -> Result<(PathBuf, bool)> {
    let forced_dir = raw.ends_with('/') || raw.ends_with('\\');
    let path = if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        let home = rightkit_fs::account_home()
            .map_err(|e| Error(format!("cannot resolve the account home for {raw:?}: {e}")))?;
        home.join(rest.trim_end_matches(['/', '\\']))
    } else {
        PathBuf::from(raw.trim_end_matches(['/', '\\']))
    };
    let file = match fs::symlink_metadata(&path) {
        Ok(md) => md.file_type().is_file(),
        Err(_) => !forced_dir && path.extension().is_some(),
    };
    Ok((path, file))
}

fn resolve_roots(cfg: &SnapshotConfig, managed: &Path) -> Result<Vec<(PathBuf, bool)>> {
    let mut paths: Vec<(PathBuf, bool)> = Vec::new();
    if let Some(name) = &cfg.app_name {
        let dirs = resolve_app_dirs(name, cfg.flat)?;
        paths.push((dirs.data.clone(), false));
        paths.push((dirs.config.clone(), false));
        paths.push((dirs.local.clone(), false));
        if cfg.include_cache {
            paths.push((dirs.cache.clone(), false));
        }
    }
    if cfg.suite_shared {
        paths.push((suite_shared_dir()?, false));
    }
    for p in &cfg.extra_paths {
        paths.push(extra_entry(p)?);
    }
    let mut resolved: Vec<(PathBuf, bool)> = Vec::new();
    for (p, file) in paths {
        check_root_sane(&p, managed, file)?;
        let p = match fs::symlink_metadata(&p) {
            Ok(md) if md.file_type().is_symlink() => {
                return err(format!(
                    "data root {} is a symbolic link; refusing it",
                    p.display()
                ))
            }
            Ok(md) if file && !md.is_file() => {
                return err(format!("data root {} is not a file", p.display()))
            }
            Ok(md) if !file && !md.is_dir() => {
                return err(format!("data root {} is not a folder", p.display()))
            }
            Ok(_) => crate::util::canonical_plain(&p)
                .map_err(|e| Error(format!("cannot resolve {}: {e}", p.display())))?,
            Err(_) => p,
        };
        if !resolved.iter().any(|(q, _)| *q == p) {
            resolved.push((p, file));
        }
    }
    // A root inside another folder root is already covered by it.
    let all = resolved.clone();
    resolved.retain(|(p, _)| !all.iter().any(|(o, of)| !*of && o != p && p.starts_with(o)));
    resolved.sort();
    if resolved.is_empty() && cfg.android.is_none() {
        return err("[snapshot] resolves no data folder");
    }
    Ok(resolved)
}

// ------------------------------------------------------------------------------------ marker

fn write_marker(path: &Path, m: &Marker) -> Result<()> {
    fs_ctx(
        rightkit_fs::private::write_private_file(path, &serde_json::to_vec_pretty(m)?),
        "cannot write restore marker",
        path,
    )
}

fn claim_marker(path: &Path, m: &Marker) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(m)?;
    let mut f = fs_ctx(
        rightkit_fs::private::create_private_new(path),
        "cannot claim restore marker (an existing marker must be recovered first)",
        path,
    )?;
    ctx(f.write_all(&bytes), "cannot write", path)?;
    ctx(f.sync_all(), "cannot sync", path)
}

fn read_marker(path: &Path) -> Result<Marker> {
    let bytes = ctx(fs::read(path), "cannot read", path)?;
    let m: Marker = serde_json::from_slice(&bytes).map_err(|e| {
        Error(format!(
            "restore marker {} is unreadable: {e}",
            path.display()
        ))
    })?;
    if m.schema != SCHEMA {
        return err(format!(
            "restore marker {} has schema {}, this rightkit-qa reads {SCHEMA}",
            path.display(),
            m.schema
        ));
    }
    Ok(m)
}

/// The managed root of a marker path `<managed>/snapshots/<app>/restore-marker.json`.
fn managed_of(marker_path: &Path) -> Result<PathBuf> {
    marker_path
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            Error(format!(
                "{} is not under a managed root",
                marker_path.display()
            ))
        })
}

fn discard(marker_path: &Path, snap_dir: &Path) {
    if snap_dir.exists() {
        let _ = remove_tree(snap_dir);
    }
    let _ = fs::remove_file(marker_path);
}

// ------------------------------------------------------------------------------------ arm

/// A snapshot taken and verified; the data it covers is restored by [`Armed::restore`] or, if that
/// never runs, by `Drop` or the next start.
#[derive(Debug)]
pub struct Armed {
    marker_path: PathBuf,
    marker: Marker,
    done: bool,
}

impl Armed {
    /// Exact restore marker held by this snapshot, for child harness handoff.
    pub fn marker_path(&self) -> &Path {
        &self.marker_path
    }

    pub fn snapshot_id(&self) -> &str {
        &self.marker.snapshot_id
    }

    /// Restore every data folder and credential item to the snapshot, verify against the
    /// manifest, then remove the marker and the snapshot. On an error the marker stays and the
    /// next `rightkit-qa` start tries again.
    pub fn restore(mut self) -> Result<RestoreReport> {
        self.done = true;
        unregister(&self.marker_path);
        restore_marker(&self.marker_path, &mut self.marker)
    }
}

fn unregister(marker_path: &Path) {
    armed_list().retain(|(p, _)| p != marker_path);
}

impl Drop for Armed {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        self.done = true;
        unregister(&self.marker_path);
        match restore_marker(&self.marker_path, &mut self.marker) {
            Ok(r) => eprintln!("[qa] {}", r.describe()),
            Err(e) => eprintln!(
                "[qa] SNAPSHOT RESTORE FAILED: {e}. The restore marker stays at {}; the next rightkit-qa start restores it.",
                self.marker_path.display()
            ),
        }
    }
}

/// Restore after `result` and combine the two: a run whose restore failed is never a pass, and a
/// failed run reports a failed restore as well. The marker stays after a failed restore.
pub fn finish<T>(armed: Armed, result: Result<T>) -> Result<T> {
    match (result, armed.restore()) {
        (Ok(v), Ok(done)) => {
            eprintln!("[qa] {}", done.describe());
            Ok(v)
        }
        (Ok(_), Err(r)) => err(format!(
            "the run finished but the snapshot restore FAILED: {r}. The restore marker stays; the next rightkit-qa start restores it. This run is not a pass."
        )),
        (Err(e), Ok(done)) => {
            eprintln!("[qa] {}", done.describe());
            Err(e)
        }
        (Err(e), Err(r)) => err(format!(
            "{e}; and the snapshot restore FAILED: {r}. The restore marker stays; the next rightkit-qa start restores it."
        )),
    }
}

/// Take the snapshot for `app` under `managed_root` (see the module docs). Recovers a leftover
/// marker first. Refuses while the app runs.
pub fn arm(app: &str, cfg: &SnapshotConfig, managed_root: &Path) -> Result<Armed> {
    arm_with_owner(app, cfg, managed_root, None)
}

/// Arm using the installed app's credential-only service when its OS store is owner-bound.
/// The selected executable is pinned by hash in the restore marker; no direct-store fallback.
pub fn arm_with_owner(
    app: &str,
    cfg: &SnapshotConfig,
    managed_root: &Path,
    credential_owner: Option<OwnerConfig>,
) -> Result<Armed> {
    cfg.validate()?;
    if credential_owner.is_some() && cfg.credentials.is_empty() {
        return err("a credential owner requires snapshot credential sets");
    }
    if !single_component(app) {
        return err("snapshot app must be one normal path component");
    }
    for note in recover_all(managed_root)? {
        eprintln!("[qa] {note}");
    }
    let running = if cfg.process_names.iter().all(|n| n.trim().is_empty()) {
        Vec::new()
    } else {
        running_processes(&cfg.process_names)?
    };
    if !running.is_empty() {
        return err(format!(
            "{app} is running ({}); quit it before a journey run. Nothing was changed.",
            running.join(", ")
        ));
    }
    let roots = resolve_roots(cfg, managed_root)?;
    let creds: Vec<CredRecord> = cfg
        .credentials
        .iter()
        .map(|c| CredRecord {
            service: c.service.clone(),
            windows_target: c.windows_target.clone(),
            windows_prefix: c.windows_prefix.clone(),
            accounts: c.accounts.clone(),
        })
        .collect();

    let app_dir = managed_root.join(SNAPSHOTS_DIR).join(app);
    fs_ctx(
        rightkit_fs::private::ensure_private_dir(&app_dir),
        "cannot create",
        &app_dir,
    )?;
    let id = new_id();
    let snap_dir = app_dir.join(&id);
    let marker_path = app_dir.join(MARKER_FILE);
    let mut records = Vec::new();
    for (index, (p, file)) in roots.iter().enumerate() {
        records.push(RootRecord {
            index,
            live_path: utf8(p)?.to_string(),
            existed: if *file { p.is_file() } else { p.is_dir() },
            file: *file,
        });
    }
    let credential_owner = credential_owner
        .map(|owner| owner.for_snapshot(&snap_dir, &cfg.process_names))
        .transpose()?;
    let mut marker = Marker {
        schema: SCHEMA,
        snapshot_id: id,
        app: app.to_string(),
        state: STATE_SNAPSHOTTING.into(),
        owner_pid: std::process::id(),
        owner_command: crate::process::fingerprint(std::process::id()),
        created_at: now_iso(),
        snapshot_dir: utf8(&snap_dir)?.to_string(),
        roots: records,
        credentials: creds,
        manifest_sha256: None,
        credentials_sha256: None,
        credential_owner,
        process_names: cfg.process_names.clone(),
        android: cfg.android.as_ref().map(AndroidRecord::from_config),
    };
    // The marker comes first: nothing is copied or read from the OS store before it exists.
    claim_marker(&marker_path, &marker)?;
    match build_snapshot(&snap_dir, &mut marker) {
        Ok((manifest_sha, cred_sha)) => {
            ensure_stopped(&marker)?;
            marker.manifest_sha256 = Some(manifest_sha);
            marker.credentials_sha256 = cred_sha;
            marker.state = STATE_ARMED.into();
            if let Err(e) = write_marker(&marker_path, &marker) {
                if marker.credential_owner.is_none() {
                    discard(&marker_path, &snap_dir);
                }
                return Err(e);
            }
            armed_list().push((marker_path.clone(), marker.snapshot_id.clone()));
            Ok(Armed {
                marker_path,
                marker,
                done: false,
            })
        }
        Err(e) => {
            // Live data was only read: removing the snapshot's own files is safe.
            if marker.credential_owner.is_none() {
                discard(&marker_path, &snap_dir);
            }
            Err(Error(format!("snapshot failed before launch: {e}")))
        }
    }
}

fn build_snapshot(snap_dir: &Path, marker: &mut Marker) -> Result<(String, Option<String>)> {
    fs_ctx(
        rightkit_fs::private::ensure_private_dir(snap_dir),
        "cannot create",
        snap_dir,
    )?;
    let mut manifest = Manifest {
        roots: BTreeMap::new(),
        android: None,
    };
    for r in &marker.roots {
        let live = Path::new(&r.live_path);
        let mut entries = Vec::new();
        if r.existed && r.file {
            let entry = file_entry(live)?;
            let dst = snap_dir.join(ROOTS_DIR).join(r.index.to_string());
            ctx(fs::create_dir_all(&dst), "cannot create", &dst)?;
            let to = dst.join(rel_path(&entry.rel));
            ctx(fs::copy(live, &to), "cannot copy", live)?;
            entries = vec![entry];
            let bad = diff(&entries, &scan(&dst)?, false);
            if !bad.is_empty() {
                return err(format!(
                    "the copy of {} does not match its source: {}",
                    live.display(),
                    bad.join("; ")
                ));
            }
            let moved = diff(&entries, &[file_entry(live)?], true);
            if !moved.is_empty() {
                return err(format!(
                    "{} changed while it was copied: {}",
                    live.display(),
                    moved.join("; ")
                ));
            }
        } else if r.existed {
            entries = scan(live)?;
            let dst = snap_dir.join(ROOTS_DIR).join(r.index.to_string());
            copy_tree(live, &dst, &entries)?;
            let copied = scan(&dst)?;
            let bad = diff(&entries, &copied, false);
            if !bad.is_empty() {
                return err(format!(
                    "the copy of {} does not match its source: {}",
                    live.display(),
                    bad.join("; ")
                ));
            }
            // The source must not have changed while it was copied.
            let again = scan(live)?;
            let moved = diff(&entries, &again, true);
            if !moved.is_empty() {
                return err(format!(
                    "{} changed while it was copied (is the app still writing?): {}",
                    live.display(),
                    moved.join("; ")
                ));
            }
        }
        manifest.roots.insert(r.index.to_string(), entries);
    }
    if let Some(rec) = marker.android.as_mut() {
        crate::snapshot_android::snapshot(rec, snap_dir)?;
        manifest.android = Some(AndroidDigest {
            package: rec.package.clone(),
            serial: rec.serial.clone(),
            tar_sha256: rec.tar_sha256.clone().unwrap_or_default(),
            listing_sha256: rec.listing_sha256.clone().unwrap_or_default(),
        });
    }
    let cred_sha = if marker.credentials.is_empty() {
        None
    } else if marker.credential_owner.is_some() {
        let metadata = owner::operate(marker, "snapshot")?;
        let (_, hash) = load_credentials(marker, snap_dir, None)?;
        if metadata.credentials_sha256 != hash {
            return err("credential owner returned a different file digest");
        }
        Some(hash)
    } else {
        let file = read_credentials(&marker.credentials)?;
        validate_credentials(&marker.credentials, &file)?;
        let bytes = serde_json::to_vec_pretty(&file)?;
        let path = snap_dir.join(CREDENTIALS_FILE);
        fs_ctx(
            rightkit_fs::private::write_private_file(&path, &bytes),
            "cannot write",
            &path,
        )?;
        Some(sha256_hex(&bytes))
    };
    let bytes = serde_json::to_vec_pretty(&manifest)?;
    let path = snap_dir.join(MANIFEST_FILE);
    fs_ctx(
        rightkit_fs::private::write_private_file(&path, &bytes),
        "cannot write",
        &path,
    )?;
    Ok((sha256_hex(&bytes), cred_sha))
}

// ------------------------------------------------------------------------------------ restore / recover

fn restore_marker(marker_path: &Path, marker: &mut Marker) -> Result<RestoreReport> {
    let managed = managed_of(marker_path)?;
    let snap_dir = PathBuf::from(&marker.snapshot_dir);
    if let Some(owner) = &marker.credential_owner {
        owner.verify()?;
        owner::verify_location(marker)?;
        if snap_dir
            != managed
                .join(SNAPSHOTS_DIR)
                .join(&marker.app)
                .join(&marker.snapshot_id)
            || marker_path != snap_dir.parent().unwrap().join(MARKER_FILE)
        {
            return err("credential owner snapshot directory differs from this restore marker");
        }
    }
    ensure_stopped_before_restore(marker)?;
    let mut report = RestoreReport {
        app: marker.app.clone(),
        snapshot_id: marker.snapshot_id.clone(),
        ..RestoreReport::default()
    };
    if marker.state == STATE_SNAPSHOTTING {
        // The journey never started; live data was only read.
        discard(marker_path, &snap_dir);
        report.discarded = true;
        return Ok(report);
    }
    report.manifest_sha256 = marker.manifest_sha256.clone().unwrap_or_default();
    report.credentials_sha256 = marker.credentials_sha256.clone();
    if marker.state != STATE_ARMED && marker.state != STATE_RESTORING {
        return err(format!(
            "restore marker has the unknown state {:?}",
            marker.state
        ));
    }
    // Before the first live write: the snapshot must be the one the marker describes.
    let manifest_path = snap_dir.join(MANIFEST_FILE);
    let manifest_bytes = ctx(fs::read(&manifest_path), "cannot read", &manifest_path)?;
    if Some(sha256_hex(&manifest_bytes)) != marker.manifest_sha256 {
        return err(
            "the snapshot manifest does not match the restore marker; live data was NOT touched",
        );
    }
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)?;
    if manifest.roots.len() != marker.roots.len() {
        return err("snapshot manifest root count differs from the marker");
    }
    let mut root_indices = BTreeSet::new();
    for root in &marker.roots {
        if !root_indices.insert(root.index) {
            return err("snapshot marker contains duplicate root indices");
        }
    }
    for entries in manifest.roots.values() {
        let mut paths = BTreeSet::new();
        for entry in entries {
            let path = Path::new(&entry.rel);
            if entry.rel.is_empty()
                || path.is_absolute()
                || path
                    .components()
                    .any(|part| !matches!(part, Component::Normal(_)))
                || !paths.insert(&entry.rel)
            {
                return err("snapshot manifest contains an unsafe or duplicate relative path");
            }
        }
    }
    let cred_file = if marker.credentials.is_empty() {
        None
    } else {
        let want = marker.credentials_sha256.as_deref().ok_or_else(|| {
            Error("credential snapshot has no recorded digest; live data was NOT touched".into())
        })?;
        Some(load_credentials(marker, &snap_dir, Some(want))?.0)
    };
    for r in &marker.roots {
        check_root_sane(Path::new(&r.live_path), &managed, r.file)?;
        let Some(entries) = manifest.roots.get(&r.index.to_string()) else {
            return err("the snapshot manifest lacks a data folder the marker names; live data was NOT touched");
        };
        if r.existed {
            let snap_root = snap_dir.join(ROOTS_DIR).join(r.index.to_string());
            let bad = diff(entries, &scan(&snap_root)?, false);
            if !bad.is_empty() {
                return err(format!(
                    "the snapshot copy of {} is damaged ({}); live data was NOT touched",
                    r.live_path,
                    bad.join("; ")
                ));
            }
        }
    }
    if let Some(rec) = &marker.android {
        let want = AndroidDigest {
            package: rec.package.clone(),
            serial: rec.serial.clone(),
            tar_sha256: rec.tar_sha256.clone().unwrap_or_default(),
            listing_sha256: rec.listing_sha256.clone().unwrap_or_default(),
        };
        if manifest.android.as_ref() != Some(&want) {
            return err("the Android snapshot in the manifest does not match the restore marker; live data was NOT touched");
        }
        crate::snapshot_android::verify_copy(rec, &snap_dir)?;
    }
    ensure_stopped_before_restore(marker)?;
    marker.state = STATE_RESTORING.into();
    write_marker(marker_path, marker)?;
    if marker.credential_owner.is_some() {
        let metadata = owner::operate(marker, "restore")?;
        if Some(metadata.credentials_sha256) != marker.credentials_sha256 {
            return err("credential owner restore digest does not match the marker");
        }
        ensure_stopped(marker)?;
        report.credentials_restored = cred_file.as_ref().map_or(0, |file| file.items.len());
    }
    for r in &marker.roots {
        let entries = &manifest.roots[&r.index.to_string()];
        let snap_root = snap_dir.join(ROOTS_DIR).join(r.index.to_string());
        if r.file {
            restore_file_root(Path::new(&r.live_path), &snap_root, r.existed, entries)?;
        } else {
            restore_root(Path::new(&r.live_path), &snap_root, r.existed, entries)?;
        }
        report.roots_restored += 1;
    }
    if marker.credential_owner.is_none() {
        if let Some(file) = &cred_file {
            report.credentials_restored = restore_credentials(&marker.credentials, file)?;
        }
    }
    if let Some(rec) = &marker.android {
        crate::snapshot_android::restore(rec, &snap_dir)?;
        report.android_restored = true;
    }
    // Everything is verified: the marker goes first, then the snapshot (and its credential file).
    ctx(fs::remove_file(marker_path), "cannot remove", marker_path)?;
    let _ = fs::remove_file(snap_dir.join(CREDENTIALS_FILE));
    let _ = remove_tree(&snap_dir);
    Ok(report)
}

fn owner_alive(m: &Marker) -> bool {
    if m.owner_pid == std::process::id() {
        return true;
    }
    if !crate::process::is_alive(m.owner_pid) {
        return false;
    }
    match &m.owner_command {
        Some(cmd) => crate::process::fingerprint(m.owner_pid).as_deref() == Some(cmd.as_str()),
        None => true,
    }
}

/// Restore (or discard) every leftover marker whose owner process is gone. Call it at the start
/// of every `rightkit-qa` process except a scenario worker. A marker whose owner is still alive
/// is left alone and reported. An error means a snapshot could not be restored: stop.
pub fn recover_all(managed_root: &Path) -> Result<Vec<String>> {
    let dir = managed_root.join(SNAPSHOTS_DIR);
    let mut notes = Vec::new();
    if !dir.is_dir() {
        return Ok(notes);
    }
    for item in ctx(fs::read_dir(&dir), "cannot read", &dir)? {
        let item = ctx(item, "cannot read", &dir)?;
        let marker_path = item.path().join(MARKER_FILE);
        if !marker_path.is_file() {
            continue;
        }
        let mut marker = read_marker(&marker_path)?;
        if owner_alive(&marker) {
            notes.push(format!(
                "snapshot {} of {} is held by the running process {}; left alone",
                marker.snapshot_id, marker.app, marker.owner_pid
            ));
            continue;
        }
        let report = restore_marker(&marker_path, &mut marker).map_err(|e| {
            Error(format!(
                "crash recovery of snapshot {} of {} FAILED: {e}. The marker {} stays; fix the cause and start again. Nothing else will run.",
                marker.snapshot_id,
                marker.app,
                marker_path.display()
            ))
        })?;
        notes.push(format!("crash recovery: {}", report.describe()));
    }
    Ok(notes)
}

#[cfg(test)]
mod credential_validation_tests {
    use super::*;

    fn sets() -> Vec<CredRecord> {
        vec![CredRecord {
            service: "validation.only".into(),
            windows_target: None,
            windows_prefix: None,
            accounts: vec!["first".into(), "later".into()],
        }]
    }

    fn valid_file() -> CredFile {
        CredFile {
            items: vec![
                CredItem {
                    set: 0,
                    account: "first".into(),
                    present: false,
                    value: None,
                    sha256: None,
                },
                CredItem {
                    set: 0,
                    account: "later".into(),
                    present: true,
                    value: Some(base64_encode(b"nonempty")),
                    sha256: Some(sha256_hex(b"nonempty")),
                },
            ],
        }
    }

    #[test]
    fn accepts_complete_present_and_absent_payload() {
        assert!(validate_credentials(&sets(), &valid_file()).is_ok());
    }

    #[test]
    fn rejects_later_corrupt_value_before_restore_can_begin() {
        let mut file = valid_file();
        file.items[1].sha256 = Some(sha256_hex(b"different"));
        assert!(validate_credentials(&sets(), &file).is_err());
        file.items[1].value = Some("%%%".into());
        assert!(validate_credentials(&sets(), &file).is_err());
        file.items[1].value = Some(String::new());
        file.items[1].sha256 = Some(sha256_hex(b""));
        assert!(validate_credentials(&sets(), &file).is_err());
    }

    #[test]
    fn rejects_omission_duplication_and_unknown_accounts() {
        let mut file = valid_file();
        file.items.pop();
        assert!(validate_credentials(&sets(), &file).is_err());
        let mut file = valid_file();
        file.items[1] = file.items[0].clone();
        assert!(validate_credentials(&sets(), &file).is_err());
        let mut file = valid_file();
        file.items[1].account = "undeclared".into();
        assert!(validate_credentials(&sets(), &file).is_err());
        file.items[1].set = 1;
        assert!(validate_credentials(&sets(), &file).is_err());
    }

    #[test]
    fn rejects_absent_values_and_duplicate_declarations() {
        let mut file = valid_file();
        file.items[0].value = Some(base64_encode(b"unexpected"));
        assert!(validate_credentials(&sets(), &file).is_err());
        let mut declared = sets();
        declared[0].accounts[1] = "first".into();
        assert!(validate_credentials(&declared, &valid_file()).is_err());
    }

    #[test]
    fn rejects_missing_unknown_and_duplicate_json_fields() {
        let valid = serde_json::to_vec(&valid_file()).unwrap();
        assert!(parse_credentials(&valid, &sets()).is_ok());
        let mut value = serde_json::to_value(valid_file()).unwrap();
        value["items"][0].as_object_mut().unwrap().remove("value");
        assert!(parse_credentials(&serde_json::to_vec(&value).unwrap(), &sets()).is_err());
        let mut value = serde_json::to_value(valid_file()).unwrap();
        value["items"][0]["extra"] = serde_json::Value::Null;
        assert!(parse_credentials(&serde_json::to_vec(&value).unwrap(), &sets()).is_err());
        let text =
            String::from_utf8(valid)
                .unwrap()
                .replacen("\"set\":0", "\"set\":0,\"set\":0", 1);
        assert!(parse_credentials(text.as_bytes(), &sets()).is_err());
    }
}

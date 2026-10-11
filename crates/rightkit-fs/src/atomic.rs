//! Atomic file replacement: temp file in the destination directory, `fsync`, rename over the
//! target, `fsync` the directory. Readers see the old content or the new content, never a mix,
//! and a crash at any point leaves the old file intact plus (at worst) an orphan temp file
//! that [`sweep_stale_temps`] reclaims.
//!
//! Consolidates CutRight `fsutil::atomic_write` (project and qa copies), CodeRight
//! `private_fs::atomic` and the settings/model-store writers.

use crate::error::{FsError, Result};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

/// Marker inside every temp name; sweeping only ever touches names containing it.
pub const TEMP_MARKER: &str = ".rktmp-";
static COUNTER: AtomicU64 = AtomicU64::new(0);
const TEMP_ATTEMPTS: u32 = 32;

#[derive(Clone, Copy, Debug)]
pub struct AtomicOptions {
    /// Temp inode is created owner-only (0600 / owner DACL) and the result stays owner-only.
    pub private: bool,
    /// Create missing parent directories first (owner-only when `private`).
    pub create_parents: bool,
    /// `fsync` the file and its directory. Disable only for rebuildable data.
    pub durable: bool,
}

impl Default for AtomicOptions {
    fn default() -> Self {
        Self {
            private: false,
            create_parents: false,
            durable: true,
        }
    }
}

impl AtomicOptions {
    pub fn private() -> Self {
        Self {
            private: true,
            ..Self::default()
        }
    }
}

/// How private a written file is, per platform. [`AtomicWriteOptions::private`] `true` is the same as
/// [`Privacy::OwnerOnly`] and wins over [`AtomicWriteOptions::privacy`].
///
/// | | Unix | Windows |
/// |---|---|---|
/// | [`Privacy::Inherit`] | mode from the existing file, else `0666 & ~umask` | the parent's inherited ACL |
/// | [`Privacy::OwnerOnly`] | `0600` from creation (parents `0700`) | one protected ACE for the current user (`D:PAI(A;;FA;;;<user>)`) set on the temp before the write, and re-applied to the destination around a `ReplaceFile` replace |
/// | [`Privacy::OwnerOnlyUnixInheritWindows`] | as `OwnerOnly` | as `Inherit`: no ACL step, so the file carries the parent's ACL |
///
/// Windows measurements (build 10.0.26300.9550, `tests/atomic_dacl_windows.rs`): a fresh file in a
/// temp directory inherits `D:AI(A;ID;FA;;;<user>)` and, after a rename over a private file,
/// keeps that inherited ACL; `OwnerOnly` ends `D:PAI(A;;FA;;;<user>)` in every replace mode and
/// destination state. The ACL step costs a native security call (with an `icacls` fallback) per
/// write; pick `OwnerOnlyUnixInheritWindows` where the parent directory's ACL is already the
/// intended boundary (a per-user data directory) and only the Unix mode bits need tightening.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Privacy {
    /// No privacy step on either platform. The default.
    #[default]
    Inherit,
    /// Owner-only on both platforms.
    OwnerOnly,
    /// Owner-only mode bits on Unix, the inherited ACL on Windows.
    OwnerOnlyUnixInheritWindows,
}

/// How a commit replaces an existing destination.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReplaceMode {
    /// `rename(2)` on Unix; `std::fs::rename` on Windows, which replaces with POSIX semantics, so a
    /// reader holding the old file open does not block the writer. The default mode.
    ///
    /// What a durable write guarantees when it returns `Ok`, per platform:
    ///
    /// * Unix: the temp file was `fsync`ed (`F_FULLFSYNC` first on Apple) before the `rename`, and the
    ///   directory was `fsync`ed after it, so the new name and content survive power loss.
    /// * Windows: the temp file was flushed (`FlushFileBuffers`) before the rename. The rename itself
    ///   is not issued write-through and there is no directory `fsync`, so after a power loss the
    ///   old content may reappear. Use [`ReplaceMode::MoveWriteThrough`] or
    ///   [`ReplaceMode::ReplaceFile`] where that matters.
    ///
    /// With `durable: false` no flush is done on any platform. The call and flag word each replace
    /// receives are observable under the `testing` feature (`atomic::testing`).
    #[default]
    Rename,
    /// Windows: `ReplaceFileW` when the destination exists (with `REPLACEFILE_WRITE_THROUGH` when
    /// `durable`), so the replacement is on stable storage when the call returns. A missing
    /// destination is moved into place with `MoveFileExW` (write-through when `durable`).
    /// Elsewhere this is the same as [`ReplaceMode::Rename`].
    ///
    /// DACL, verified on Windows build 10.0.26300.9550 by `tests/atomic_dacl_windows.rs`
    /// (reads the DACL back with `GetNamedSecurityInfoW`): `ReplaceFileW` keeps the DESTINATION's
    /// DACL, not the temp file's. A private write over a missing or already-private destination ends
    /// owner-only (`D:PAI(A;;FA;;;<user SID>)`), but over a broader destination
    /// (`D:AI(A;;FR;;;WD)(A;ID;FA;;;<user SID>)`) it stayed broad. A `private` write therefore
    /// narrows the destination to owner-only before the replace (best effort) and again after it
    /// (an error if that fails), so the result is owner-only in every case. A non-private write
    /// over a private destination keeps the destination's DACL. [`ReplaceMode::Rename`] needs no
    /// such step: the moved file carries the temp file's owner-only DACL.
    ReplaceFile,
    /// Windows: `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH`, so the
    /// move is on stable storage when the call returns, whether or not the destination exists
    /// (the mode HeardRight's telemetry outbox uses). This is a separate mode, not part of
    /// `durable`, because `MoveFileExW` replaces without POSIX semantics: a concurrent reader that
    /// holds the destination open without `FILE_SHARE_DELETE` makes the move fail with a sharing or
    /// access error, which [`AtomicWriteOptions::retry_sharing_violation`] can retry. Choose it for
    /// files nobody holds open across a write. The temp keeps its own DACL, so private writes stay
    /// owner-only. Elsewhere this is the same as [`ReplaceMode::Rename`].
    MoveWriteThrough,
    /// Publish only if the destination does not exist, atomically: the call fails with
    /// [`FsError::AlreadyExists`] when the destination exists (or appears first), and the temp file is
    /// removed. Never check-then-rename. Unix: `renameat2(RENAME_NOREPLACE)` on Linux,
    /// `renamex_np(RENAME_EXCL)` on Apple; where the primitive is missing (old kernel, other Unix, a
    /// filesystem without support) it falls back to `link(2)` then `unlink(2)`, which is still
    /// exclusive (`link` fails if the name exists) but leaves both names for an instant, so a crash
    /// between the two leaves a temp file for [`sweep_stale_temps`]. Windows: `MoveFileExW` without
    /// `MOVEFILE_REPLACE_EXISTING` (write-through when `durable`). See also [`rename_exclusive`].
    NoReplace,
}

/// Retry for a publish step that fails because another process holds the destination open.
///
/// Only Windows sharing and lock violations (`ERROR_SHARING_VIOLATION` 32 and
/// `ERROR_LOCK_VIOLATION` 33) are retried. Other platforms never report them, so the policy is
/// inert there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Retries after the first attempt. Zero means one attempt and no retry.
    pub attempts: u32,
    /// Pause before each retry.
    pub delay: Duration,
}

/// Full options for an atomic write. [`AtomicOptions`] is the three-field form and converts
/// with `From`; it stays unchanged so existing literals keep compiling.
#[derive(Clone, Copy, Debug)]
pub struct AtomicWriteOptions {
    /// Temp inode is created owner-only (0600 / owner DACL) and the result stays owner-only.
    pub private: bool,
    /// Create missing parent directories first (owner-only when `private`).
    pub create_parents: bool,
    /// `fsync` the file and its directory. Disable only for rebuildable data.
    pub durable: bool,
    /// How the temp file replaces the destination. Default [`ReplaceMode::Rename`].
    pub replace: ReplaceMode,
    /// Retry the publish step on Windows sharing violations. Default `None` (no retry).
    pub retry_sharing_violation: Option<RetryPolicy>,
    /// Platform-split privacy (see [`Privacy`]). Default [`Privacy::Inherit`]; `private: true`
    /// overrides it with [`Privacy::OwnerOnly`].
    pub privacy: Privacy,
}

impl AtomicWriteOptions {
    /// The privacy this write gets: `private: true` is [`Privacy::OwnerOnly`].
    pub fn effective_privacy(&self) -> Privacy {
        if self.private {
            Privacy::OwnerOnly
        } else {
            self.privacy
        }
    }

    /// Owner-only mode bits and `0700` parents are wanted (Unix).
    fn unix_owner_only(&self) -> bool {
        self.effective_privacy() != Privacy::Inherit
    }

    /// The Windows owner-only ACL step is wanted.
    fn windows_acl(&self) -> bool {
        self.effective_privacy() == Privacy::OwnerOnly
    }
}

impl Default for AtomicWriteOptions {
    fn default() -> Self {
        AtomicOptions::default().into()
    }
}

impl From<AtomicOptions> for AtomicWriteOptions {
    fn from(options: AtomicOptions) -> Self {
        Self {
            private: options.private,
            create_parents: options.create_parents,
            durable: options.durable,
            replace: ReplaceMode::Rename,
            retry_sharing_violation: None,
            privacy: Privacy::Inherit,
        }
    }
}

impl RetryPolicy {
    pub fn new(attempts: u32, delay: Duration) -> Self {
        Self { attempts, delay }
    }
}

/// Atomically replace `path` with `bytes`.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    atomic_write_with(path, AtomicOptions::default(), |f| f.write_all(bytes))
}

/// Atomically replace `path` with `bytes`, owner-only.
pub fn atomic_write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    atomic_write_with(path, AtomicOptions::private(), |f| f.write_all(bytes))
}

/// Atomically replace `path` with whatever `fill` writes. If `fill` fails, the destination is
/// untouched and the temp file is removed.
pub fn atomic_write_with(
    path: &Path,
    options: AtomicOptions,
    fill: impl FnOnce(&mut File) -> std::io::Result<()>,
) -> Result<()> {
    atomic_write_with_options(path, options.into(), fill)
}

/// [`atomic_write_with`] with the full [`AtomicWriteOptions`].
pub fn atomic_write_with_options(
    path: &Path,
    options: AtomicWriteOptions,
    fill: impl FnOnce(&mut File) -> std::io::Result<()>,
) -> Result<()> {
    let mut writer = AtomicWriter::create_with(path, options)?;
    fill(writer.file_mut()).map_err(|e| FsError::io(writer.temp_path(), e))?;
    writer.commit()
}

/// [`atomic_write_with_options`], also returning the lowercase hex SHA-256 of the published bytes
/// (see [`AtomicWriter::commit_with_digest`]).
#[cfg(feature = "digest")]
pub fn atomic_write_with_digest(
    path: &Path,
    options: AtomicWriteOptions,
    fill: impl FnOnce(&mut File) -> std::io::Result<()>,
) -> Result<String> {
    let mut writer = AtomicWriter::create_with(path, options)?;
    fill(writer.file_mut()).map_err(|e| FsError::io(writer.temp_path(), e))?;
    writer.commit_with_digest()
}

/// Rename `from` to `to` only if `to` does not exist, atomically (never check-then-rename). Fails
/// with [`FsError::AlreadyExists`] when `to` exists or appears first, leaving `from` in place.
/// Mechanisms per platform are on [`ReplaceMode::NoReplace`].
pub fn rename_exclusive(from: &Path, to: &Path) -> Result<()> {
    rename_exclusive_io(from, to).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            FsError::AlreadyExists(to.to_path_buf())
        } else {
            FsError::io(to, e)
        }
    })
}

#[cfg(windows)]
fn rename_exclusive_io(from: &Path, to: &Path) -> std::io::Result<()> {
    move_file_ex(from, to, 0)
}

#[cfg(unix)]
fn rename_exclusive_io(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let cfrom = CString::new(from.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let cto = CString::new(to.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    #[cfg(target_os = "linux")]
    {
        const RENAME_NOREPLACE: libc::c_uint = 1;
        // SAFETY: both paths are NUL-terminated C strings that outlive the call.
        let rc = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                libc::AT_FDCWD,
                cfrom.as_ptr(),
                libc::AT_FDCWD,
                cto.as_ptr(),
                RENAME_NOREPLACE,
            )
        };
        if rc == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::ENOSYS) | Some(libc::EINVAL) | Some(libc::ENOTSUP) => {}
            _ => return Err(error),
        }
    }
    #[cfg(target_vendor = "apple")]
    {
        const RENAME_EXCL: libc::c_uint = 0x0000_0004;
        // SAFETY: both paths are NUL-terminated C strings that outlive the call.
        let rc = unsafe { libc::renamex_np(cfrom.as_ptr(), cto.as_ptr(), RENAME_EXCL) };
        if rc == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::ENOTSUP) | Some(libc::EINVAL) | Some(libc::ENOSYS) => {}
            _ => return Err(error),
        }
    }
    // No exclusive rename here: `link` fails with EEXIST if `to` exists, then drop the old name.
    fs::hard_link(from, to)?;
    fs::remove_file(from)
}

#[cfg(not(any(unix, windows)))]
fn rename_exclusive_io(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::hard_link(from, to)?;
    fs::remove_file(from)
}

/// A streaming atomic write. Nothing is visible at the destination until [`commit`]; dropping
/// an uncommitted writer removes its temp file. A process killed mid-write leaves only the
/// temp file behind.
///
/// [`commit`]: AtomicWriter::commit
#[derive(Debug)]
pub struct AtomicWriter {
    file: Option<File>,
    temp: PathBuf,
    target: PathBuf,
    options: AtomicWriteOptions,
}

impl AtomicWriter {
    pub fn create(target: &Path, options: AtomicOptions) -> Result<Self> {
        Self::create_with(target, options.into())
    }

    /// Like [`AtomicWriter::create`], with the full [`AtomicWriteOptions`].
    pub fn create_with(target: &Path, options: AtomicWriteOptions) -> Result<Self> {
        let parent = parent_of(target);
        if options.create_parents {
            let private_dirs = if cfg!(windows) {
                options.windows_acl()
            } else {
                options.unix_owner_only()
            };
            if private_dirs {
                crate::private::ensure_private_dir(&parent)?;
            } else {
                fs::create_dir_all(&parent).map_err(|e| FsError::io(&parent, e))?;
            }
        }
        // Keep the mode of an existing regular destination (never a symlink's target).
        #[cfg(unix)]
        let existing_mode = match fs::symlink_metadata(target) {
            Ok(m) if m.is_file() => {
                use std::os::unix::fs::PermissionsExt;
                Some(m.permissions().mode() & 0o7777)
            }
            _ => None,
        };
        let name = target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| FsError::BadName(target.display().to_string()))?;
        let mut last = None;
        for attempt in 0..TEMP_ATTEMPTS {
            let temp = parent.join(temp_name(&name, attempt));
            let mut open = OpenOptions::new();
            open.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                let mode = if options.unix_owner_only() {
                    0o600
                } else {
                    existing_mode.unwrap_or(0o666)
                };
                open.mode(mode)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
            }
            match open.open(&temp) {
                Ok(file) => {
                    #[cfg(not(unix))]
                    if options.windows_acl() {
                        if let Err(e) = crate::private::restrict_owner_only(&temp) {
                            let _ = fs::remove_file(&temp);
                            return Err(e);
                        }
                    }
                    #[cfg(unix)]
                    if let (false, Some(mode)) = (options.unix_owner_only(), existing_mode) {
                        // `mode()` above is masked by umask; restore the exact prior bits.
                        use std::os::unix::fs::PermissionsExt;
                        let _ = file.set_permissions(fs::Permissions::from_mode(mode));
                    }
                    return Ok(Self {
                        file: Some(file),
                        temp,
                        target: target.to_path_buf(),
                        options,
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = Some(e),
                Err(e) => return Err(FsError::io(&temp, e)),
            }
        }
        Err(FsError::io(
            target,
            last.unwrap_or_else(|| std::io::Error::other("temp name collisions")),
        ))
    }

    pub fn temp_path(&self) -> &Path {
        &self.temp
    }

    pub fn file_mut(&mut self) -> &mut File {
        self.file.as_mut().expect("writer is open until commit")
    }

    pub fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        let temp = self.temp.clone();
        self.file_mut()
            .write_all(bytes)
            .map_err(|e| FsError::io(temp, e))
    }

    /// Publish: flush, `fsync`, replace the target, `fsync` the directory.
    ///
    /// If the replace succeeds but the directory `fsync` fails, the destination already holds the
    /// new content and this returns [`FsError::ReplacedButDirSyncFailed`]. Check
    /// [`FsError::target_replaced`] before deciding to write again. Any other error means the
    /// destination is unchanged.
    pub fn commit(self) -> Result<()> {
        self.commit_inner(false).map(|_| ())
    }

    /// [`AtomicWriter::commit`], also returning the lowercase hex SHA-256 of the published bytes.
    ///
    /// The digest is taken from the temp file after the last write (and `fsync`), immediately before
    /// the replace, so it is the digest of exactly what is published however the bytes were written
    /// (including through [`AtomicWriter::file_mut`]). That costs one sequential read of data the page
    /// cache still holds. For hashing while streaming, feed [`crate::digest::Sha256Hasher`] yourself.
    #[cfg(feature = "digest")]
    pub fn commit_with_digest(self) -> Result<String> {
        self.commit_inner(true)
            .map(|d| d.expect("a digest was requested"))
    }

    fn commit_inner(mut self, want_digest: bool) -> Result<Option<String>> {
        let file = self.file.take().expect("writer is open until commit");
        let result = (|| -> Result<Option<String>> {
            if self.options.durable {
                durable_sync(&file).map_err(|e| FsError::io(&self.temp, e))?;
            }
            drop(file);
            #[cfg(feature = "digest")]
            let digest = if want_digest {
                Some(
                    crate::digest::sha256_file(&self.temp)
                        .map_err(|e| FsError::io(&self.temp, e))?,
                )
            } else {
                None
            };
            #[cfg(not(feature = "digest"))]
            let digest: Option<String> = {
                let _ = want_digest;
                None
            };
            publish(&self.temp, &self.target, self.options).map_err(|e| {
                if self.options.replace == ReplaceMode::NoReplace
                    && e.kind() == std::io::ErrorKind::AlreadyExists
                {
                    FsError::AlreadyExists(self.target.clone())
                } else {
                    FsError::io(&self.target, e)
                }
            })?;
            if self.options.durable {
                try_sync_dir(&parent_of(&self.target)).map_err(|source| {
                    FsError::ReplacedButDirSyncFailed {
                        path: self.target.clone(),
                        source,
                    }
                })?;
            }
            Ok(digest)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&self.temp);
        }
        // Either published or cleaned up: nothing left for Drop.
        self.temp = PathBuf::new();
        result
    }
}

impl Drop for AtomicWriter {
    fn drop(&mut self) {
        if !self.temp.as_os_str().is_empty() {
            self.file.take();
            let _ = fs::remove_file(&self.temp);
        }
    }
}

fn parent_of(path: &Path) -> PathBuf {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

fn temp_name(name: &str, attempt: u32) -> String {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    // `<pid>` is parsed back by `sweep_stale_temps` to tell a crashed writer from a live one.
    format!(
        ".{name}{TEMP_MARKER}{}-{n}-{nanos:x}-{attempt}",
        std::process::id()
    )
}

/// Best-effort directory `fsync` so a rename itself survives power loss (no-op off unix).
/// Errors are discarded; use [`AtomicWriter::commit`] when the caller must know.
pub fn sync_dir(dir: &Path) {
    let _ = try_sync_dir(dir);
}

/// Directory `fsync`, reporting failure (no-op off unix, where a write-through replace is the
/// durability step).
fn try_sync_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let d = File::open(dir)?;
        durable_sync(&d)
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

/// Replaces `target` with `temp` under the requested mode, retrying sharing violations when the
/// options ask for it.
fn publish(temp: &Path, target: &Path, options: AtomicWriteOptions) -> std::io::Result<()> {
    // `ReplaceFileW` keeps the DESTINATION's DACL (measured, see `ReplaceMode::ReplaceFile`), so a
    // private write over a broader destination would otherwise stay broad. Narrow the destination
    // first, so the new content is never readable by more principals than the old content was
    // (best effort: if it fails, the check after the replace decides), and again after the replace.
    let harden =
        options.windows_acl() && options.replace == ReplaceMode::ReplaceFile && cfg!(windows);
    if harden {
        let _ = crate::private::restrict_owner_only(target);
    }
    retry_with(
        options.retry_sharing_violation,
        is_sharing_violation,
        || replace_step(temp, target, options),
    )?;
    if harden {
        crate::private::restrict_owner_only(target).map_err(|e| {
            std::io::Error::other(format!(
                "replaced, but could not make the destination owner-only: {e}"
            ))
        })?;
    }
    Ok(())
}

/// Runs `op`, and while `classify` says its error is transient and retries remain, pauses and
/// runs it again.
fn retry_with<T>(
    policy: Option<RetryPolicy>,
    classify: fn(&std::io::Error) -> bool,
    mut op: impl FnMut() -> std::io::Result<T>,
) -> std::io::Result<T> {
    let mut retries_left = policy.map_or(0, |p| p.attempts);
    loop {
        match op() {
            Err(error) if retries_left > 0 && classify(&error) => {
                retries_left -= 1;
                if let Some(policy) = policy {
                    std::thread::sleep(policy.delay);
                }
            }
            other => return other,
        }
    }
}

/// `ERROR_SHARING_VIOLATION` or `ERROR_LOCK_VIOLATION` on Windows. Never true elsewhere.
#[cfg(windows)]
fn is_sharing_violation(error: &std::io::Error) -> bool {
    matches!(error.raw_os_error(), Some(32) | Some(33))
}

#[cfg(not(windows))]
fn is_sharing_violation(_error: &std::io::Error) -> bool {
    false
}

/// The platform call a replace uses. Reported to the `testing` hook.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplaceApi {
    /// `std::fs::rename` (Unix: `rename(2)`).
    Rename,
    /// Windows `MoveFileExW`; `flags` are its `dwFlags`.
    MoveFileEx,
    /// Windows `ReplaceFileW`; `flags` are its `dwReplaceFlags`.
    ReplaceFile,
    /// The exclusive rename of [`ReplaceMode::NoReplace`] on Unix (`flags` 0).
    RenameExclusive,
}

/// `MOVEFILE_REPLACE_EXISTING`.
pub const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
/// `MOVEFILE_WRITE_THROUGH`.
pub const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
/// `REPLACEFILE_WRITE_THROUGH`.
pub const REPLACEFILE_WRITE_THROUGH: u32 = 0x1;

/// What one replace call is about to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplaceCall {
    pub api: ReplaceApi,
    /// The API's flag word (always 0 for [`ReplaceApi::Rename`]).
    pub flags: u32,
    pub temp: PathBuf,
    pub target: PathBuf,
}

/// Chooses the call and flags for one replace. Write-through on `ReplaceFile` follows
/// `options.durable`, the Windows counterpart of the file and directory `fsync` a durable write does
/// on Unix. `Rename` never uses `MoveFileExW`: `std::fs::rename` replaces with POSIX semantics on
/// Windows, which a concurrent reader's open handle cannot block (measured: `MoveFileExW` made
/// writers fail with `ERROR_ACCESS_DENIED` against readers in `tests/e2e.rs`).
fn plan_replace(options: AtomicWriteOptions, target_exists: bool) -> (ReplaceApi, u32) {
    if options.replace == ReplaceMode::NoReplace {
        return if cfg!(windows) {
            (
                ReplaceApi::MoveFileEx,
                if options.durable {
                    MOVEFILE_WRITE_THROUGH
                } else {
                    0
                },
            )
        } else {
            (ReplaceApi::RenameExclusive, 0)
        };
    }
    if !cfg!(windows) {
        return (ReplaceApi::Rename, 0);
    }
    match (options.replace, target_exists) {
        (ReplaceMode::NoReplace, _) => unreachable!("handled above"),
        (ReplaceMode::Rename, _) => (ReplaceApi::Rename, 0),
        (ReplaceMode::MoveWriteThrough, _) => (
            ReplaceApi::MoveFileEx,
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        ),
        (ReplaceMode::ReplaceFile, true) => (
            ReplaceApi::ReplaceFile,
            if options.durable {
                REPLACEFILE_WRITE_THROUGH
            } else {
                0
            },
        ),
        // ReplaceFile mode, first creation: no reader can hold a missing file open.
        (ReplaceMode::ReplaceFile, false) => (
            ReplaceApi::MoveFileEx,
            MOVEFILE_REPLACE_EXISTING
                | if options.durable {
                    MOVEFILE_WRITE_THROUGH
                } else {
                    0
                },
        ),
    }
}

fn replace_step(temp: &Path, target: &Path, options: AtomicWriteOptions) -> std::io::Result<()> {
    let exists =
        options.replace == ReplaceMode::ReplaceFile && fs::symlink_metadata(target).is_ok();
    let (api, flags) = plan_replace(options, exists);
    #[cfg(feature = "testing")]
    if let Some(error) = testing::before_replace(&ReplaceCall {
        api,
        flags,
        temp: temp.to_path_buf(),
        target: target.to_path_buf(),
    }) {
        return Err(error);
    }
    match api {
        ReplaceApi::Rename => fs::rename(temp, target),
        ReplaceApi::RenameExclusive => rename_exclusive_io(temp, target),
        ReplaceApi::MoveFileEx => move_file_ex(temp, target, flags),
        ReplaceApi::ReplaceFile => replace_file_w(temp, target, flags),
    }
}

#[cfg(windows)]
fn move_file_ex(temp: &Path, target: &Path, flags: u32) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }
    let from: Vec<u16> = temp.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: both buffers are NUL-terminated and outlive the call.
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), flags) } == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn move_file_ex(temp: &Path, target: &Path, _flags: u32) -> std::io::Result<()> {
    fs::rename(temp, target)
}

#[cfg(windows)]
fn replace_file_w(temp: &Path, target: &Path, flags: u32) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn ReplaceFileW(
            replaced_file_name: *const u16,
            replacement_file_name: *const u16,
            backup_file_name: *const u16,
            replace_flags: u32,
            exclude: *mut std::ffi::c_void,
            reserved: *mut std::ffi::c_void,
        ) -> i32;
    }
    let replaced: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    let replacement: Vec<u16> = temp.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: both buffers are NUL-terminated and outlive the call. The backup, exclude and
    // reserved arguments are null, which the API documents as "not used".
    let ok = unsafe {
        ReplaceFileW(
            replaced.as_ptr(),
            replacement.as_ptr(),
            std::ptr::null(),
            flags,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file_w(temp: &Path, target: &Path, _flags: u32) -> std::io::Result<()> {
    fs::rename(temp, target)
}

/// Fault injection for consumer tests (feature `testing`, off by default). A hook runs on the
/// calling thread immediately before the replace call of every atomic write on that thread. It sees
/// the call that is about to happen ([`ReplaceCall`]: the API, its flag word, temp and target) and may
/// return an error, which then stands in for the OS call: the destination is untouched, and the
/// writer removes its temp file and returns the error, exactly as for a failed `MoveFileExW`,
/// `ReplaceFileW` or `rename`.
///
/// The feature cannot be used in a release build by accident: without `debug_assertions` the
/// crate fails to compile with it enabled.
#[cfg(feature = "testing")]
pub mod testing {
    use super::ReplaceCall;
    use std::cell::RefCell;

    type Hook = Box<dyn Fn(&ReplaceCall) -> Option<std::io::Error>>;

    thread_local! {
        static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }

    /// Removes the hook when dropped.
    pub struct HookGuard(());

    impl Drop for HookGuard {
        fn drop(&mut self) {
            HOOK.with(|h| *h.borrow_mut() = None);
        }
    }

    /// Install `hook` for this thread until the guard drops. Returning `Some(error)` fails the
    /// replace with that error; `None` lets the real call run.
    pub fn on_replace(
        hook: impl Fn(&ReplaceCall) -> Option<std::io::Error> + 'static,
    ) -> HookGuard {
        HOOK.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
        HookGuard(())
    }

    pub(super) fn before_replace(call: &ReplaceCall) -> Option<std::io::Error> {
        HOOK.with(|h| h.borrow().as_ref().and_then(|hook| hook(call)))
    }
}

/// How strong a flush [`durable_sync_with`] achieved, weakest first.
///
/// - [`Full`](Self::Full): the strongest flush the platform offers: `F_FULLFSYNC` on Apple
///   (asks the drive to flush its cache), `fsync(2)` on other Unix, `FlushFileBuffers` on Windows.
/// - [`Barrier`](Self::Barrier): Apple `F_BARRIERFSYNC`, which orders writes but does not force
///   them to the medium.
/// - [`Fsync`](Self::Fsync): Apple plain `fsync(2)`, which can leave data in the drive cache.
///
/// Only Apple targets ever report less than `Full`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SyncLevel {
    Fsync,
    Barrier,
    Full,
}

/// What [`durable_sync_with`] does when the strongest flush (`F_FULLFSYNC`) is unavailable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SyncPolicy {
    /// Fall back to `F_BARRIERFSYNC`, then `fsync`, as libuv does. The result says which level
    /// was reached. This is what [`durable_sync`] does. The default.
    #[default]
    Fallback,
    /// Never fall back: if the strongest flush fails, return its error.
    Strict,
}

/// The sync ladder, with the three Apple steps passed in so the policy can be tested anywhere.
#[cfg_attr(
    not(any(target_vendor = "apple", feature = "testing", test)),
    allow(dead_code)
)]
pub(crate) fn sync_ladder(
    policy: SyncPolicy,
    full: impl FnOnce() -> std::io::Result<()>,
    barrier: impl FnOnce() -> std::io::Result<()>,
    fsync: impl FnOnce() -> std::io::Result<()>,
) -> std::io::Result<SyncLevel> {
    let full_error = match full() {
        Ok(()) => return Ok(SyncLevel::Full),
        Err(error) => error,
    };
    if policy == SyncPolicy::Strict {
        return Err(full_error);
    }
    if barrier().is_ok() {
        return Ok(SyncLevel::Barrier);
    }
    fsync().map(|()| SyncLevel::Fsync)
}

/// Flush a file to stable storage. On Apple targets this matches libuv's `uv__fs_fsync`:
/// `F_FULLFSYNC`, then `F_BARRIERFSYNC`, then plain `fsync`, because `F_FULLFSYNC` fails with
/// `EIO`/`ENOTSUP` on some external and network volumes where the weaker syncs succeed.
/// Elsewhere it is `File::sync_all`. The fallback is silent here; use [`durable_sync_with`] to
/// learn the level reached or to forbid the fallback.
pub fn durable_sync(file: &File) -> std::io::Result<()> {
    durable_sync_with(file, SyncPolicy::Fallback).map(|_| ())
}

/// [`durable_sync`] that reports the [`SyncLevel`] reached and honours a [`SyncPolicy`].
/// With [`SyncPolicy::Strict`] an Apple volume that cannot `F_FULLFSYNC` is an error (the
/// `F_FULLFSYNC` error), not a weaker success. Other platforms report [`SyncLevel::Full`].
pub fn durable_sync_with(file: &File, policy: SyncPolicy) -> std::io::Result<SyncLevel> {
    #[cfg(target_vendor = "apple")]
    {
        use std::os::fd::AsRawFd;
        const F_BARRIERFSYNC: libc::c_int = 85;
        let fd = file.as_raw_fd();
        let check = |rc: libc::c_int| {
            if rc == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        };
        // SAFETY: `fd` is a valid open descriptor borrowed from `file` for these calls.
        sync_ladder(
            policy,
            || check(unsafe { libc::fcntl(fd, libc::F_FULLFSYNC) }),
            || check(unsafe { libc::fcntl(fd, F_BARRIERFSYNC) }),
            || check(unsafe { libc::fsync(fd) }),
        )
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        let _ = policy;
        file.sync_all().map(|()| SyncLevel::Full)
    }
}

#[cfg(test)]
mod sync_ladder_tests {
    use super::{sync_ladder, SyncLevel, SyncPolicy};
    use std::cell::Cell;
    use std::io::{Error, ErrorKind, Result};

    fn ok() -> Result<()> {
        Ok(())
    }
    fn no(kind: ErrorKind) -> impl FnOnce() -> Result<()> {
        move || Err(Error::from(kind))
    }

    #[test]
    fn full_success_stops_at_full() {
        let barrier_ran = Cell::new(false);
        let level = sync_ladder(
            SyncPolicy::Fallback,
            ok,
            || {
                barrier_ran.set(true);
                Ok(())
            },
            ok,
        );
        assert_eq!(level.unwrap(), SyncLevel::Full);
        assert!(!barrier_ran.get());
    }

    #[test]
    fn fallback_reports_each_weaker_level() {
        let level = |barrier: Result<()>, fsync: Result<()>| {
            sync_ladder(
                SyncPolicy::Fallback,
                no(ErrorKind::Unsupported),
                || barrier,
                || fsync,
            )
        };
        assert_eq!(level(Ok(()), Ok(())).unwrap(), SyncLevel::Barrier);
        assert_eq!(
            level(Err(Error::from(ErrorKind::Other)), Ok(())).unwrap(),
            SyncLevel::Fsync
        );
        let all_failed = level(
            Err(Error::from(ErrorKind::Other)),
            Err(Error::from(ErrorKind::TimedOut)),
        );
        assert_eq!(all_failed.unwrap_err().kind(), ErrorKind::TimedOut);
    }

    #[test]
    fn strict_returns_the_full_error_and_never_tries_weaker_steps() {
        let weaker_ran = Cell::new(0);
        let result = sync_ladder(
            SyncPolicy::Strict,
            no(ErrorKind::Unsupported),
            || {
                weaker_ran.set(weaker_ran.get() + 1);
                Ok(())
            },
            || {
                weaker_ran.set(weaker_ran.get() + 1);
                Ok(())
            },
        );
        assert_eq!(result.unwrap_err().kind(), ErrorKind::Unsupported);
        assert_eq!(weaker_ran.get(), 0);
    }

    #[test]
    fn levels_order_weakest_first() {
        assert!(SyncLevel::Fsync < SyncLevel::Barrier && SyncLevel::Barrier < SyncLevel::Full);
    }
}

/// Remove orphaned temp files in `dir` left by crashed writers. A temp file is reclaimed when
/// the process that created it is gone, or when it is older than `max_age` (pid reuse guard).
/// Only names containing [`TEMP_MARKER`] are ever considered. Returns the number removed.
pub fn sweep_stale_temps(dir: &Path, max_age: Duration) -> Result<usize> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(FsError::io(dir, e)),
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(pid) = temp_owner_pid(&name) else {
            continue;
        };
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let old = meta
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|age| age >= max_age);
        if ((pid != std::process::id() && !crate::process_alive(pid)) || old)
            && fs::remove_file(entry.path()).is_ok()
        {
            removed += 1;
        }
    }
    Ok(removed)
}

fn temp_owner_pid(name: &str) -> Option<u32> {
    let rest = &name[name.find(TEMP_MARKER)? + TEMP_MARKER.len()..];
    rest.split('-').next()?.parse().ok()
}

#[cfg(test)]
mod retry_tests {
    use super::{retry_with, RetryPolicy};
    use std::cell::Cell;
    use std::io;
    use std::time::Duration;

    fn sharing(_: &io::Error) -> bool {
        true
    }

    #[test]
    fn retries_until_success_within_the_budget() {
        let calls = Cell::new(0);
        let policy = Some(RetryPolicy::new(3, Duration::ZERO));
        let result = retry_with(policy, sharing, || {
            calls.set(calls.get() + 1);
            if calls.get() < 3 {
                Err(io::Error::from_raw_os_error(32))
            } else {
                Ok(())
            }
        });
        assert!(result.is_ok());
        assert_eq!(calls.get(), 3, "one attempt plus two retries");
    }

    #[test]
    fn gives_up_after_the_retry_budget() {
        let calls = Cell::new(0);
        let policy = Some(RetryPolicy::new(2, Duration::ZERO));
        let result: io::Result<()> = retry_with(policy, sharing, || {
            calls.set(calls.get() + 1);
            Err(io::Error::from_raw_os_error(32))
        });
        assert!(result.is_err());
        assert_eq!(
            calls.get(),
            3,
            "one attempt plus two retries, then the error"
        );
    }

    #[test]
    fn no_policy_means_one_attempt() {
        let calls = Cell::new(0);
        let result: io::Result<()> = retry_with(None, sharing, || {
            calls.set(calls.get() + 1);
            Err(io::Error::from_raw_os_error(32))
        });
        assert!(result.is_err());
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn unclassified_errors_are_not_retried() {
        let calls = Cell::new(0);
        let policy = Some(RetryPolicy::new(5, Duration::ZERO));
        let never = |_: &io::Error| false;
        let result: io::Result<()> = retry_with(policy, never, || {
            calls.set(calls.get() + 1);
            Err(io::Error::from_raw_os_error(32))
        });
        assert!(result.is_err());
        assert_eq!(calls.get(), 1);
    }
}

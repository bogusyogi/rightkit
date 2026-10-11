//! Atomic file replacement: temp file in the destination directory, `fsync`, rename over the
//! target, `fsync` the directory. Readers see the old content or the new content, never a mix,
//! and a crash at any point leaves the old file intact plus (at worst) an orphan temp file
//! that [`sweep_stale_temps`] reclaims.
//!
//! A write has two phases. Until the replace call succeeds nothing is published: any failure
//! leaves the destination as it was and removes the temp file. After it succeeds the new content
//! is in place, and every later failure (the directory sync, removing a leftover temp name or
//! backup file) is a typed post-publication error whose
//! [`FsError::target_replaced`] is true, reported only after the required directory sync ran.
//!
//! Consolidates CutRight `fsutil::atomic_write` (project and qa copies), CodeRight
//! `private_fs::atomic` and the settings/model-store writers.

use crate::error::{FsError, Result};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

/// Marker inside every temp name this crate generates. Sweeping only touches complete generated
/// names (`.<name>.rktmp-<pid>-<n>-<attempt>` from `StateDir`, `.<name>.rktmp-<pid>-<n>-<nanos in
/// hex>-<attempt>` from atomic writes), and public writes refuse a target with such a name, so
/// the marker alone never marks a committed record for deletion.
pub const TEMP_MARKER: &str = ".rktmp-";
static COUNTER: AtomicU64 = AtomicU64::new(0);
pub(crate) const TEMP_ATTEMPTS: u32 = 32;

#[derive(Clone, Copy, Debug)]
pub struct AtomicOptions {
    /// Temp inode is created owner-only (0600 / owner DACL) and the result stays owner-only.
    pub private: bool,
    /// Create missing parent directories first (owner-only when `private`).
    pub create_parents: bool,
    /// `fsync` the file and its directory (Unix); on Windows the file is flushed and the strongest
    /// supported publication is used, which is weaker for some modes. See
    /// [`publication_durability`] and [`ReplaceMode`]. Disable only for rebuildable data.
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
/// | [`Privacy::OwnerOnly`] | `0600` from creation (parents `0700`) | one protected ACE for the current user (`D:P(A;;FA;;;<user>)`) passed to `CreateFileW` when the temp file is created, so it never exists with a broader ACL; a `ReplaceFile`-mode write over an existing destination is published by `MoveFileExW` (the temp's DACL travels with it) instead of `ReplaceFileW`, which would keep the destination's |
/// | [`Privacy::OwnerOnlyUnixInheritWindows`] | as `OwnerOnly` | as `Inherit`: no ACL step, so the file carries the parent's ACL |
///
/// Windows measurements (build 10.0.26300.9550, `tests/atomic_dacl_windows.rs`): a fresh file in a
/// temp directory inherits `D:AI(A;ID;FA;;;<user>)` and, after a rename over a private file,
/// keeps that inherited ACL; `OwnerOnly` ends with the one protected ACE for the user (the
/// `AI` bookkeeping flag may or may not show) in every replace mode and destination state. The
/// ACL costs a native security descriptor per write; pick `OwnerOnlyUnixInheritWindows` where
/// the parent directory's ACL is already the intended boundary (a per-user data directory) and
/// only the Unix mode bits need tightening.
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
    ///   old content may reappear. Use [`ReplaceMode::MoveWriteThrough`] where that matters.
    ///
    /// With `durable: false` no flush is done on any platform. [`publication_durability`] reports
    /// which of these a given option set gets. The call and flag word each replace receives are
    /// observable under the `testing` feature (`atomic::testing`).
    #[default]
    Rename,
    /// Windows: `ReplaceFileW` when the destination exists, so the replacement keeps the
    /// destination's attributes and ACL. A missing destination is moved into place with
    /// `MoveFileExW` (write-through when `durable`). Elsewhere this is the same as
    /// [`ReplaceMode::Rename`].
    ///
    /// **Durability.** `ReplaceFileW` has no supported write-through: Microsoft documents its
    /// `REPLACEFILE_WRITE_THROUGH` flag as "not supported", and this crate never passes it. Over an
    /// existing destination a durable write flushes the temp file before the call and nothing
    /// after it, so the swap itself can be lost in a power failure (the file is never torn: the
    /// old or the new content survives). Only the first creation is write-through. Use
    /// [`ReplaceMode::MoveWriteThrough`] when the replacement must be on stable storage;
    /// [`publication_durability`] states what each mode gives.
    ///
    /// **Backup and partial failure.** `ReplaceFileW` is given a unique backup file
    /// (`.<name>.rktbak-<pid>-<n>` beside the destination, never swept) because without one a
    /// failure after the original was removed (`ERROR_UNABLE_TO_MOVE_REPLACEMENT`, 1176) leaves
    /// neither version at the destination. The backup is deleted after a successful replace
    /// (a failure to delete it is [`FsError::ReplacedButPostStepFailed`] with
    /// [`crate::PostPublishStep::TempCleanup`]). When the call fails with 1176 or 1177 and the
    /// destination is not intact, the result is [`FsError::ReplaceInterrupted`] and the temp and
    /// backup files are left in place for recovery.
    ///
    /// DACL, verified on Windows build 10.0.26300.9550 by `tests/atomic_dacl_windows.rs`
    /// (reads the DACL back with `GetNamedSecurityInfoW`): `ReplaceFileW` keeps the DESTINATION's
    /// DACL, not the temp file's. A **private** write therefore does not use it: over an existing
    /// destination it is published by `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING` (plus
    /// `MOVEFILE_WRITE_THROUGH` when `durable`), which carries the temp file's owner-only DACL
    /// with it. There is no step that narrows the destination, so no moment at which a broader
    /// file at the destination name lends its ACL to the new secret. A reader holding the old file
    /// open without share-delete can make that move fail like [`ReplaceMode::MoveWriteThrough`].
    /// A non-private write over a private destination keeps the destination's DACL.
    /// [`ReplaceMode::Rename`] likewise moves the temp file's owner-only DACL.
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
    /// between the two leaves a temp file for [`sweep_stale_temps`]. If the `unlink` fails after the
    /// `link` the content is published and complete: the write still syncs the directory, then
    /// returns [`FsError::ReplacedButPostStepFailed`] ([`crate::PostPublishStep::TempCleanup`]).
    /// Windows: `MoveFileExW` without `MOVEFILE_REPLACE_EXISTING` (write-through when `durable`).
    /// See also [`rename_exclusive`].
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
    /// `fsync` the file and its directory (Unix); on Windows the file is flushed and the strongest
    /// supported publication is used, which is weaker for some modes. See
    /// [`publication_durability`] and [`ReplaceMode`]. Disable only for rebuildable data.
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
///
/// `to` must not be shaped like a temp name this crate generates ([`FsError::BadName`]). Where the
/// kernel has no exclusive rename the fallback is `link` then `unlink`; if the `unlink` fails,
/// `to` exists with the content and this returns [`FsError::ReplacedButPostStepFailed`]
/// ([`crate::PostPublishStep::TempCleanup`]), with `from` still present as a second name.
pub fn rename_exclusive(from: &Path, to: &Path) -> Result<()> {
    if let Some(name) = to.file_name() {
        reject_reserved_name(&name.to_string_lossy())?;
    }
    match rename_exclusive_io(from, to) {
        Ok(None) => Ok(()),
        Ok(Some(source)) => Err(FsError::ReplacedButPostStepFailed {
            path: to.to_path_buf(),
            step: crate::PostPublishStep::TempCleanup,
            source,
        }),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(FsError::AlreadyExists(to.to_path_buf()))
        }
        Err(e) => Err(FsError::io(to, e)),
    }
}

/// Renames `from` to `to` only if `to` does not exist. `Ok(None)`: published, nothing left over.
/// `Ok(Some(error))`: published (`to` exists with the content) but removing the `from` name failed
/// (the `link` fallback). `Err`: nothing was published.
#[cfg(windows)]
fn rename_exclusive_io(from: &Path, to: &Path) -> std::io::Result<Option<std::io::Error>> {
    move_file_ex(from, to, 0).map(|()| None)
}

#[cfg(unix)]
fn rename_exclusive_io(from: &Path, to: &Path) -> std::io::Result<Option<std::io::Error>> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    #[cfg(feature = "testing")]
    if let Some(unlink_fails) = testing::link_fallback() {
        return link_then_unlink(from, to, unlink_fails);
    }
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
            return Ok(None);
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
            return Ok(None);
        }
        let error = std::io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::ENOTSUP) | Some(libc::EINVAL) | Some(libc::ENOSYS) => {}
            _ => return Err(error),
        }
    }
    link_then_unlink(from, to, false)
}

#[cfg(not(any(unix, windows)))]
fn rename_exclusive_io(from: &Path, to: &Path) -> std::io::Result<Option<std::io::Error>> {
    link_then_unlink(from, to, false)
}

/// No exclusive rename here: `link` fails with `EEXIST` if `to` exists, then drop the old name.
/// Once the `link` succeeds `to` is published, so a failed `unlink` is returned as `Ok(Some(..))`
/// (published, with a leftover second name for the same inode), never as an error. With
/// `inject_unlink_failure` (the `testing` seam) the unlink is not attempted and an error stands in
/// for its failure.
#[cfg_attr(not(feature = "testing"), allow(unused_variables))]
#[cfg_attr(windows, allow(dead_code))]
fn link_then_unlink(
    from: &Path,
    to: &Path,
    inject_unlink_failure: bool,
) -> std::io::Result<Option<std::io::Error>> {
    fs::hard_link(from, to)?;
    #[cfg(feature = "testing")]
    if inject_unlink_failure {
        return Ok(Some(std::io::Error::other("injected unlink failure")));
    }
    Ok(fs::remove_file(from).err())
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
    ///
    /// A target whose file name is shaped like a temp name this crate generates
    /// (`.<name>.rktmp-<pid>-<n>-<attempt>`) is refused with [`FsError::BadName`]: a sweep would
    /// delete it as an orphan.
    pub fn create_with(target: &Path, options: AtomicWriteOptions) -> Result<Self> {
        let name = target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| FsError::BadName(target.display().to_string()))?;
        reject_reserved_name(&name)?;
        let parent = parent_of(target);
        if options.create_parents {
            let private_dirs = if cfg!(windows) {
                options.windows_acl()
            } else {
                options.unix_owner_only()
            };
            if options.durable {
                // Publish every directory this creates (and any left unpublished by an earlier
                // or concurrent attempt) before the file is written below it.
                create_dirs_durable(&parent, private_dirs)?;
            }
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
        #[cfg(not(unix))]
        let existing_mode: Option<u32> = None;
        let mut last = None;
        for attempt in 0..TEMP_ATTEMPTS {
            let temp = parent.join(temp_name(&name, attempt));
            match open_temp(&temp, &options, existing_mode) {
                Ok(file) => {
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
    /// Until the replace succeeds nothing is published: any error is an ordinary one, the
    /// destination is unchanged, and the temp file is removed. Once it has succeeded the
    /// destination holds the new content, the directory sync (durable writes) still runs, and a
    /// failure of any later step is reported as [`FsError::ReplacedButDirSyncFailed`] or
    /// [`FsError::ReplacedButPostStepFailed`]. Check [`FsError::target_replaced`] before deciding
    /// to write again. What a durable write guarantees per platform is on [`ReplaceMode`].
    pub fn commit(self) -> Result<()> {
        self.commit_inner(false, |_| Ok(())).map(|_| ())
    }

    /// [`AtomicWriter::commit`] with a check that runs after the temp file's content is written
    /// (flushed and synced first when `durable`) and before anything is published.
    ///
    /// `check` receives the temp file's path. Its `Err` aborts the write: the destination is left
    /// as it was (no replace is attempted), the temp file is removed, and the error is returned as
    /// [`FsError::Io`] carrying the temp path. `Ok` publishes exactly the content that was checked.
    /// The check must not write to the temp file; it is a read-only gate.
    pub fn commit_checked(self, check: impl FnOnce(&Path) -> std::io::Result<()>) -> Result<()> {
        self.commit_inner(false, check).map(|_| ())
    }

    /// [`AtomicWriter::commit`], also returning the lowercase hex SHA-256 of the published bytes.
    ///
    /// The digest is taken from the temp file after the last write (and `fsync`), immediately before
    /// the replace, so it is the digest of exactly what is published however the bytes were written
    /// (including through [`AtomicWriter::file_mut`]). That costs one sequential read of data the page
    /// cache still holds. For hashing while streaming, feed [`crate::digest::Sha256Hasher`] yourself.
    #[cfg(feature = "digest")]
    pub fn commit_with_digest(self) -> Result<String> {
        self.commit_inner(true, |_| Ok(()))
            .map(|d| d.expect("a digest was requested"))
    }

    fn commit_inner(
        mut self,
        want_digest: bool,
        check: impl FnOnce(&Path) -> std::io::Result<()>,
    ) -> Result<Option<String>> {
        let file = self.file.take().expect("writer is open until commit");
        let result = self.finish(file, want_digest, check);
        // Unpublished: the temp is all there is to clean. Published with a later error: the
        // temp name is already gone (rename), or it is a leftover second name for the
        // published inode (link fallback), and removing it again is correct and harmless.
        // An interrupted replace is the exception: the temp may hold the only copy of the new
        // content, so it stays for the caller (the error names it).
        if matches!(&result, Err(e) if !matches!(e, FsError::ReplaceInterrupted { .. })) {
            let _ = fs::remove_file(&self.temp);
        }
        // Either published or cleaned up: nothing left for Drop.
        self.temp = PathBuf::new();
        result
    }

    /// Flush, check, publish, then complete the post-publication steps in a fixed order.
    fn finish(
        &self,
        file: File,
        want_digest: bool,
        check: impl FnOnce(&Path) -> std::io::Result<()>,
    ) -> Result<Option<String>> {
        if self.options.durable {
            durable_sync(&file).map_err(|e| FsError::io(&self.temp, e))?;
            #[cfg(feature = "testing")]
            testing::record(testing::Step::TempFlushed);
        }
        drop(file);
        #[cfg(feature = "digest")]
        let digest = if want_digest {
            Some(crate::digest::sha256_file(&self.temp).map_err(|e| FsError::io(&self.temp, e))?)
        } else {
            None
        };
        #[cfg(not(feature = "digest"))]
        let digest: Option<String> = {
            let _ = want_digest;
            None
        };

        // Caller's gate on the flushed temp file. A failure here is before publication: the
        // destination is untouched and the caller's `commit` path removes the temp file.
        check(&self.temp).map_err(|e| FsError::io(&self.temp, e))?;

        // The one step that publishes. An error here means nothing was published.
        let published = publish(&self.temp, &self.target, self.options)?;

        // Published from here on. The required directory sync runs whatever else failed, so a
        // later error is never reported before the durability work is done.
        let dir_sync = if self.options.durable {
            try_sync_dir(&parent_of(&self.target)).err()
        } else {
            None
        };
        // The most serious failure is the one reported: directory sync, then cleanup.
        if let Some(source) = dir_sync {
            return Err(FsError::ReplacedButDirSyncFailed {
                path: self.target.clone(),
                source,
            });
        }
        if let Some(source) = published.cleanup {
            return Err(FsError::ReplacedButPostStepFailed {
                path: self.target.clone(),
                step: crate::PostPublishStep::TempCleanup,
                source,
            });
        }
        Ok(digest)
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

/// Creates the temp file `temp`, exclusively and without following a link at its name. With the
/// Windows ACL step wanted the file is created with its owner-only security descriptor already
/// in place, so no principal the parent's inherited ACL would admit can open it before it is
/// restricted; creation fails (nothing exists) if the descriptor cannot be built. Unix creates it
/// with its final mode bits.
fn open_temp(
    temp: &Path,
    options: &AtomicWriteOptions,
    existing_mode: Option<u32>,
) -> std::io::Result<File> {
    #[cfg(windows)]
    if options.windows_acl() {
        return create_owner_only_temp(temp);
    }
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
    let file = open.open(temp)?;
    #[cfg(unix)]
    if let (false, Some(mode)) = (options.unix_owner_only(), existing_mode) {
        // `mode()` above is masked by umask; restore the exact prior bits.
        use std::os::unix::fs::PermissionsExt;
        let _ = file.set_permissions(fs::Permissions::from_mode(mode));
    }
    #[cfg(not(unix))]
    let _ = (options, existing_mode);
    Ok(file)
}

#[cfg(windows)]
fn create_owner_only_temp(temp: &Path) -> std::io::Result<File> {
    #[cfg(feature = "testing")]
    if let Some(error) = testing::before_harden(HardenStage::TempCreate, temp) {
        return Err(error);
    }
    crate::private::create_new_owner_only(temp)
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

/// The pid inside `name` when `name` is a *complete* temp name this crate generates, else `None`.
///
/// Two forms exist: `.<prefix>.rktmp-<pid>-<n>-<attempt>` (`StateDir`) and
/// `.<name>.rktmp-<pid>-<n>-<nanos in lowercase hex>-<attempt>` (atomic writes), with decimal
/// `pid`, `n` and `attempt` (`attempt < `[`TEMP_ATTEMPTS`]). The marker is searched from the
/// right, so a real name that itself contains `.rktmp-` still parses by its generated tail. A
/// name that merely contains the marker (`invoice.rktmp-1-final`) is not a temp name.
pub(crate) fn generated_temp_pid(name: &str) -> Option<u32> {
    let body = name.strip_prefix('.')?;
    let at = body.rfind(TEMP_MARKER)?;
    if at == 0 {
        return None; // no name between the leading dot and the marker
    }
    let tail = &body[at + TEMP_MARKER.len()..];
    let mut parts = tail.split('-');
    let (pid, n) = (parts.next()?, parts.next()?);
    let (nanos, attempt) = match (parts.next(), parts.next(), parts.next()) {
        (Some(attempt), None, None) => (None, attempt),
        (Some(nanos), Some(attempt), None) => (Some(nanos), attempt),
        _ => return None,
    };
    let decimal = |s: &str| !s.is_empty() && s.len() <= 20 && s.bytes().all(|b| b.is_ascii_digit());
    let hex = |s: &str| {
        !s.is_empty() && s.len() <= 8 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    };
    if !(decimal(pid) && decimal(n) && decimal(attempt)) || !nanos.is_none_or(hex) {
        return None;
    }
    n.parse::<u64>().ok()?;
    if attempt.parse::<u32>().ok()? >= TEMP_ATTEMPTS {
        return None;
    }
    pid.parse().ok()
}

/// Refuses a public write target whose name is a complete generated temp name: such a file would
/// be indistinguishable from an orphan and a sweep would delete it.
pub(crate) fn reject_reserved_name(name: &str) -> Result<()> {
    if generated_temp_pid(name).is_some() {
        Err(FsError::BadName(name.to_string()))
    } else {
        Ok(())
    }
}

/// Best-effort directory `fsync` so a rename itself survives power loss (no-op off unix).
/// Errors are discarded; use [`AtomicWriter::commit`] when the caller must know.
pub fn sync_dir(dir: &Path) {
    let _ = try_sync_dir(dir);
}

/// Directory `fsync`, reporting failure. A no-op off unix: Windows has no directory flush (a
/// directory handle opened for listing cannot be flushed), so the strongest publication there is
/// a write-through move; see [`publication_durability`].
fn try_sync_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        #[cfg(feature = "testing")]
        if let Some(error) = testing::before_dir_sync(dir) {
            return Err(error);
        }
        let d = File::open(dir)?;
        durable_sync(&d)
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

/// Syncs the directory that holds `path`, reporting failure (Unix; a no-op elsewhere). For callers
/// that create a directory entry some other way (a clone or a hard link) and owe it the same
/// durability as an atomic write.
pub(crate) fn sync_parent_dir(path: &Path) -> std::io::Result<()> {
    try_sync_dir(&parent_of(path))
}

/// Directories whose creation was made but whose publishing directory sync has not (yet)
/// succeeded, by `(dev, ino)`. A creator records a directory here when it makes it and clears it
/// after the sync of its parent succeeds, so a retry after a failed sync, or a concurrent
/// creator that merely finds the directory, still owes (and runs) that sync instead of
/// building a durable write on an unflushed link. In-process only.
#[cfg(unix)]
pub(crate) mod unpublished {
    use std::collections::HashSet;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    fn set() -> MutexGuard<'static, HashSet<(u64, u64)>> {
        static SET: OnceLock<Mutex<HashSet<(u64, u64)>>> = OnceLock::new();
        SET.get_or_init(|| Mutex::new(HashSet::new()))
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    pub(crate) fn insert(key: (u64, u64)) {
        set().insert(key);
    }

    pub(crate) fn contains(key: (u64, u64)) -> bool {
        let set = set();
        !set.is_empty() && set.contains(&key)
    }

    pub(crate) fn remove(key: (u64, u64)) {
        set().remove(&key);
    }
}

/// Creates `dir` and its missing ancestors one component at a time, top down, publishing each
/// new directory by syncing its parent before going on (Unix). An ancestor that already exists
/// but is recorded as unpublished (see [`unpublished`]) gets its parent synced now. A failed
/// sync is [`FsError::DirSyncFailed`]; the directories already made stay, and a retry completes
/// the sync. New directories are `0700` when `owner_only`. Elsewhere there is no directory flush,
/// so nothing is done here and the caller's ordinary creation runs.
#[cfg(unix)]
fn create_dirs_durable(dir: &Path, owner_only: bool) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    if dir.as_os_str().is_empty() {
        return Ok(());
    }
    let mut chain: Vec<&Path> = dir
        .ancestors()
        .filter(|p| !p.as_os_str().is_empty())
        .collect();
    chain.reverse();
    for path in chain {
        let mut created = false;
        match fs::symlink_metadata(path) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let mut builder = fs::DirBuilder::new();
                if owner_only {
                    builder.mode(0o700);
                }
                match builder.create(path) {
                    Ok(()) => created = true,
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(e) => return Err(FsError::io(path, e)),
                }
            }
            Err(e) => return Err(FsError::io(path, e)),
        }
        // Only a real directory (not a link to one) can be an entry this call or an earlier one
        // created, so only that is keyed.
        let key = fs::symlink_metadata(path)
            .ok()
            .filter(|m| m.is_dir())
            .map(|m| (m.dev(), m.ino()));
        let mut needed = created;
        if let Some(key) = key {
            if created {
                unpublished::insert(key);
            } else if unpublished::contains(key) {
                needed = true;
            }
        }
        if !needed || path.parent().is_none() {
            continue;
        }
        sync_parent_dir(path).map_err(|source| FsError::DirSyncFailed {
            path: path.to_path_buf(),
            source,
        })?;
        if let Some(key) = key {
            unpublished::remove(key);
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn create_dirs_durable(_dir: &Path, _owner_only: bool) -> Result<()> {
    Ok(())
}

/// What happened after a replace call succeeded: the failure, if any, of removing a leftover
/// second name for the published file (the `link` fallback, or the `ReplaceFileW` backup). The
/// content is in place either way.
struct Published {
    cleanup: Option<std::io::Error>,
}

/// Replaces `target` with `temp` under the requested mode, retrying sharing violations when the
/// options ask for it. `Err` means nothing was published, except [`FsError::ReplaceInterrupted`],
/// which says exactly which of the old and new content survive where; `Ok` means the new content
/// is in place and carries the failure of any step after that.
///
/// A private write needs no step on the destination: Windows `ReplaceFile` mode plans a
/// `MoveFileExW` replace for it (see [`plan_replace_on`]), which keeps the temp file's owner-only
/// DACL, so the published file is owner-only from the instant it has its name.
fn publish(temp: &Path, target: &Path, options: AtomicWriteOptions) -> Result<Published> {
    let cleanup = retry_with(
        options.retry_sharing_violation,
        is_sharing_violation,
        || replace_step(temp, target, options),
    )
    .map_err(|e| {
        if options.replace == ReplaceMode::NoReplace
            && e.kind() == std::io::ErrorKind::AlreadyExists
        {
            return FsError::AlreadyExists(target.to_path_buf());
        }
        match PartialReplace::take(e) {
            Ok(partial) => FsError::ReplaceInterrupted {
                path: target.to_path_buf(),
                new_content: partial.new_content,
                previous_content: partial.previous_content,
                source: partial.source,
            },
            Err(e) => FsError::io(target, e),
        }
    })?;
    Ok(Published { cleanup })
}

/// `ReplaceFileW` failed after it had already changed names (`ERROR_UNABLE_TO_MOVE_REPLACEMENT`
/// 1176, `ERROR_UNABLE_TO_MOVE_REPLACEMENT_2` 1177): Microsoft documents that without a backup
/// file the replaced file is already gone, and with one it may have been renamed to the backup
/// name. Carried inside an `io::Error` from [`replace_step`] to [`publish`], which turns it into
/// [`FsError::ReplaceInterrupted`].
#[derive(Debug)]
struct PartialReplace {
    /// Where the new content still is (the temp file), if it exists.
    new_content: Option<PathBuf>,
    /// Where the previous content is: the backup file, else the destination, if it exists.
    previous_content: Option<PathBuf>,
    source: std::io::Error,
}

impl std::fmt::Display for PartialReplace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "replace interrupted midway: {}", self.source)
    }
}

impl std::error::Error for PartialReplace {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

impl PartialReplace {
    /// The partial state inside `error`, or `error` back when it is an ordinary failure.
    fn take(error: std::io::Error) -> std::result::Result<Self, std::io::Error> {
        if !error.get_ref().is_some_and(|inner| inner.is::<Self>()) {
            return Err(error);
        }
        match error.into_inner().map(|inner| inner.downcast::<Self>()) {
            Some(Ok(partial)) => Ok(*partial),
            Some(Err(inner)) => Err(std::io::Error::other(inner)),
            None => unreachable!("an inner error was present"),
        }
    }
}

/// `ERROR_UNABLE_TO_MOVE_REPLACEMENT`.
const ERROR_UNABLE_TO_MOVE_REPLACEMENT: i32 = 1176;
/// `ERROR_UNABLE_TO_MOVE_REPLACEMENT_2`.
const ERROR_UNABLE_TO_MOVE_REPLACEMENT_2: i32 = 1177;

/// Decides from the files that exist after a failed replace call whether it left the old and new
/// content as they were. Only the two `ReplaceFileW` codes that can leave a half-done swap are
/// looked at; every other error is returned unchanged, as is one that left the destination in
/// place with no backup beside it. Otherwise the error becomes a [`PartialReplace`] naming where
/// each version now is, and the caller keeps the temp file.
fn classify_partial(
    error: std::io::Error,
    temp: &Path,
    target: &Path,
    backup: Option<&Path>,
) -> std::io::Error {
    if !matches!(
        error.raw_os_error(),
        Some(ERROR_UNABLE_TO_MOVE_REPLACEMENT | ERROR_UNABLE_TO_MOVE_REPLACEMENT_2)
    ) {
        return error;
    }
    let present = |p: &Path| fs::symlink_metadata(p).is_ok();
    let backup_present = backup.filter(|b| present(b));
    if present(target) && backup_present.is_none() {
        return error;
    }
    let kind = error.kind();
    std::io::Error::new(
        kind,
        PartialReplace {
            new_content: present(temp).then(|| temp.to_path_buf()),
            previous_content: backup_present
                .map(Path::to_path_buf)
                .or_else(|| present(target).then(|| target.to_path_buf())),
            source: error,
        },
    )
}

/// A unique sibling name for the backup `ReplaceFileW` is given. It is not a `.rktmp-` name, so
/// [`sweep_stale_temps`] never reclaims it: should a replace be interrupted, the previous content
/// sits there until someone decides what to do with it.
fn backup_path(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    parent_of(target).join(format!(".{name}.rktbak-{}-{n}", std::process::id()))
}

/// Where in an atomic write an owner-only security step runs. Reported to the `testing` hook
/// ([`HardenCall`]). Only Windows takes these steps; Unix gets its mode bits at creation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HardenStage {
    /// The temp file is created with its owner-only security descriptor (no later restriction).
    TempCreate,
    /// No longer taken: a private write is published by a move that keeps the temp file's DACL,
    /// so the destination is not narrowed. Kept so existing matches keep compiling.
    DestinationBeforeReplace,
    /// No longer taken: nothing re-checks the destination after the replace. Kept so existing
    /// matches keep compiling.
    DestinationAfterReplace,
}

/// One owner-only step about to run, as the `testing` hook sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HardenCall {
    pub stage: HardenStage,
    pub path: PathBuf,
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
/// `REPLACEFILE_WRITE_THROUGH`. Windows documents this `ReplaceFileW` flag as not supported, so this
/// crate never passes it (no replace call it makes carries it); the constant stays so existing
/// imports keep compiling.
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

/// Chooses the call and flags for one replace. Write-through follows `options.durable` only where
/// Windows supports it: `MoveFileExW` with `MOVEFILE_WRITE_THROUGH`. `ReplaceFileW` has no
/// supported write-through (its `REPLACEFILE_WRITE_THROUGH` flag is documented as not supported),
/// so it always gets flag word 0. `Rename` never uses `MoveFileExW`: `std::fs::rename` replaces
/// with POSIX semantics on Windows, which a concurrent reader's open handle cannot block
/// (measured: `MoveFileExW` made writers fail with `ERROR_ACCESS_DENIED` against readers in
/// `tests/e2e.rs`).
fn plan_replace(options: AtomicWriteOptions, target_exists: bool) -> (ReplaceApi, u32) {
    plan_replace_on(cfg!(windows), options, target_exists)
}

/// [`plan_replace`] for an explicit platform, so the Windows plan is testable everywhere.
fn plan_replace_on(
    windows: bool,
    options: AtomicWriteOptions,
    target_exists: bool,
) -> (ReplaceApi, u32) {
    if options.replace == ReplaceMode::NoReplace {
        return if windows {
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
    if !windows {
        return (ReplaceApi::Rename, 0);
    }
    match (options.replace, target_exists) {
        (ReplaceMode::NoReplace, _) => unreachable!("handled above"),
        (ReplaceMode::Rename, _) => (ReplaceApi::Rename, 0),
        (ReplaceMode::MoveWriteThrough, _) => (
            ReplaceApi::MoveFileEx,
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        ),
        // A private write must not go through ReplaceFileW: it keeps the DESTINATION's DACL, so a
        // concurrent writer that put a broadly readable file there would hand its ACL to the
        // new secret. A move keeps the temp file's owner-only DACL. It is also write-through.
        (ReplaceMode::ReplaceFile, true) if options.windows_acl() => (
            ReplaceApi::MoveFileEx,
            MOVEFILE_REPLACE_EXISTING
                | if options.durable {
                    MOVEFILE_WRITE_THROUGH
                } else {
                    0
                },
        ),
        // No supported write-through exists for ReplaceFileW: flag word 0 whatever `durable` says.
        (ReplaceMode::ReplaceFile, true) => (ReplaceApi::ReplaceFile, 0),
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

/// What a durable write's publication guarantees, as [`publication_durability`] reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PublicationDurability {
    /// `durable: false`: nothing is flushed, on any platform.
    None,
    /// The temp file is flushed before the replace, and nothing is flushed after it. The new
    /// content is never torn, but after a power loss the previous version can reappear. This is
    /// the typed "durable publication is not available" answer: Windows `Rename`, and Windows
    /// `ReplaceFile` over an existing destination.
    FileFlushedOnly,
    /// The temp file is flushed, the replace runs, and the parent directory is flushed (Unix).
    DirectoryFlushed,
    /// The temp file is flushed and the replace is `MoveFileExW` with `MOVEFILE_WRITE_THROUGH`
    /// (Windows `MoveWriteThrough`, `NoReplace`, and `ReplaceFile`'s first creation). Microsoft
    /// documents the move as complete on disk when the call returns; this crate has not run a
    /// power-loss test of it.
    WriteThroughMove,
}

/// What publishing with `options` guarantees on this platform when the call returns `Ok`, given
/// whether the destination already exists (only `ReplaceMode::ReplaceFile` on Windows depends on
/// it). Callers that must not accept [`PublicationDurability::FileFlushedOnly`] can check this
/// before writing.
pub fn publication_durability(
    options: &AtomicWriteOptions,
    destination_exists: bool,
) -> PublicationDurability {
    publication_durability_on(cfg!(windows), options, destination_exists)
}

fn publication_durability_on(
    windows: bool,
    options: &AtomicWriteOptions,
    destination_exists: bool,
) -> PublicationDurability {
    if !options.durable {
        return PublicationDurability::None;
    }
    if !windows {
        return PublicationDurability::DirectoryFlushed;
    }
    match plan_replace_on(true, *options, destination_exists) {
        (ReplaceApi::MoveFileEx, flags) if flags & MOVEFILE_WRITE_THROUGH != 0 => {
            PublicationDurability::WriteThroughMove
        }
        _ => PublicationDurability::FileFlushedOnly,
    }
}

/// One replace call. `Ok(None)`: published cleanly. `Ok(Some(error))`: published, but removing a
/// leftover second name failed (the `link` fallback, or the `ReplaceFileW` backup). `Err`: nothing
/// was published, unless the error carries a [`PartialReplace`].
fn replace_step(
    temp: &Path,
    target: &Path,
    options: AtomicWriteOptions,
) -> std::io::Result<Option<std::io::Error>> {
    let exists =
        options.replace == ReplaceMode::ReplaceFile && fs::symlink_metadata(target).is_ok();
    let (api, flags) = plan_replace(options, exists);
    // `ReplaceFileW` without a backup deletes the destination before it moves the replacement
    // in, and some failures happen in between. A unique backup keeps the old content reachable.
    let backup = (api == ReplaceApi::ReplaceFile).then(|| backup_path(target));
    let outcome = (|| -> std::io::Result<Option<std::io::Error>> {
        #[cfg(feature = "testing")]
        {
            testing::record(testing::Step::Replace);
            if let Some(error) = testing::before_replace(&ReplaceCall {
                api,
                flags,
                temp: temp.to_path_buf(),
                target: target.to_path_buf(),
            }) {
                return Err(error);
            }
        }
        match api {
            ReplaceApi::Rename => fs::rename(temp, target).map(|()| None),
            ReplaceApi::RenameExclusive => rename_exclusive_io(temp, target),
            ReplaceApi::MoveFileEx => move_file_ex(temp, target, flags).map(|()| None),
            ReplaceApi::ReplaceFile => {
                let backup = backup.as_deref().expect("planned with a backup");
                replace_file_w(temp, target, backup, flags)?;
                // Published. The old content sat at the backup name; drop it.
                Ok(match fs::remove_file(backup) {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => Some(e),
                    _ => None,
                })
            }
        }
    })();
    outcome.map_err(|error| classify_partial(error, temp, target, backup.as_deref()))
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
fn replace_file_w(temp: &Path, target: &Path, backup: &Path, flags: u32) -> std::io::Result<()> {
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
    let backup: Vec<u16> = backup.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: all three name buffers are NUL-terminated and outlive the call. The exclude and
    // reserved arguments are null, which the API documents as "not used".
    let ok = unsafe {
        ReplaceFileW(
            replaced.as_ptr(),
            replacement.as_ptr(),
            backup.as_ptr(),
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
fn replace_file_w(temp: &Path, target: &Path, _backup: &Path, _flags: u32) -> std::io::Result<()> {
    fs::rename(temp, target)
}

/// Fault injection for consumer tests (feature `testing`, off by default). Every hook is per
/// thread and removed when its guard drops.
///
/// - [`on_replace`]: runs on the calling thread immediately before the replace call of every
///   atomic write. It sees the call that is about to happen ([`ReplaceCall`]: the API, its flag
///   word, temp and target) and may return an error, which then stands in for the OS call: the
///   destination is untouched, and the writer removes its temp file and returns the error,
///   exactly as for a failed `MoveFileExW`, `ReplaceFileW` or `rename`.
/// - [`on_harden`]: runs before each Windows owner-only security step ([`HardenCall`]) and may
///   return an error in place of it, to prove a write fails closed. Unix takes no such steps.
/// - [`take_steps`]: the flush, replace and directory-sync calls of this thread's writes, in
///   order ([`Step`]), so tests assert the order a power-loss argument depends on.
/// - [`fail_dir_sync`] and [`force_link_fallback`]: make the post-publication steps fail.
///
/// The feature cannot be used in a release build by accident: without `debug_assertions` the
/// crate fails to compile with it enabled.
#[cfg(feature = "testing")]
pub mod testing {
    use super::{HardenCall, HardenStage, ReplaceCall};
    use std::cell::{Cell, RefCell};
    use std::path::{Path, PathBuf};

    type Hook = Box<dyn Fn(&ReplaceCall) -> Option<std::io::Error>>;
    type HardenHook = Box<dyn Fn(&HardenCall) -> Option<std::io::Error>>;

    /// One durability-relevant call of an atomic write, in the order it happened.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Step {
        /// The temp file was flushed before publication (durable writes only).
        TempFlushed,
        /// The replace call ran (an injected failure stands in for it).
        Replace,
        /// The directory was synced after publication (attempted; [`fail_dir_sync`] can make it
        /// fail). Unix only.
        DirSynced(PathBuf),
    }

    thread_local! {
        static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
        static HARDEN_HOOK: RefCell<Option<HardenHook>> = const { RefCell::new(None) };
        static STEPS: RefCell<Vec<Step>> = const { RefCell::new(Vec::new()) };
        static DIR_SYNC_FAILURE: Cell<Option<std::io::ErrorKind>> = const { Cell::new(None) };
        static LINK_FALLBACK: Cell<Option<bool>> = const { Cell::new(None) };
    }

    /// Removes the hook when dropped.
    pub struct HookGuard(fn());

    impl Drop for HookGuard {
        fn drop(&mut self) {
            (self.0)();
        }
    }

    /// Install `hook` for this thread until the guard drops. Returning `Some(error)` fails the
    /// replace with that error; `None` lets the real call run.
    pub fn on_replace(
        hook: impl Fn(&ReplaceCall) -> Option<std::io::Error> + 'static,
    ) -> HookGuard {
        HOOK.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
        HookGuard(|| HOOK.with(|h| *h.borrow_mut() = None))
    }

    /// Install `hook` for this thread's Windows owner-only steps until the guard drops. Returning
    /// `Some(error)` fails that step with the error; `None` lets it run. Never called on Unix.
    pub fn on_harden(hook: impl Fn(&HardenCall) -> Option<std::io::Error> + 'static) -> HookGuard {
        HARDEN_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
        HookGuard(|| HARDEN_HOOK.with(|h| *h.borrow_mut() = None))
    }

    /// Make every directory sync this thread's atomic writes perform fail with `kind`, in place of
    /// the OS call, until the guard drops. Unix only.
    pub fn fail_dir_sync(kind: std::io::ErrorKind) -> HookGuard {
        DIR_SYNC_FAILURE.with(|f| f.set(Some(kind)));
        HookGuard(|| DIR_SYNC_FAILURE.with(|f| f.set(None)))
    }

    /// Make `rename_exclusive` and `ReplaceMode::NoReplace` on this thread use the `link` then
    /// `unlink` fallback even where the kernel has an exclusive rename. With `unlink_fails` the
    /// unlink is not attempted and an injected error stands in for its failure, leaving the old
    /// name next to the published one. Unix only.
    pub fn force_link_fallback(unlink_fails: bool) -> HookGuard {
        LINK_FALLBACK.with(|f| f.set(Some(unlink_fails)));
        HookGuard(|| LINK_FALLBACK.with(|f| f.set(None)))
    }

    /// The flushes, replaces and directory syncs this thread's atomic writes made since the last
    /// call, in order. Empties the log.
    pub fn take_steps() -> Vec<Step> {
        STEPS.with(|steps| std::mem::take(&mut *steps.borrow_mut()))
    }

    pub(super) fn record(step: Step) {
        STEPS.with(|steps| steps.borrow_mut().push(step));
    }

    pub(super) fn before_replace(call: &ReplaceCall) -> Option<std::io::Error> {
        HOOK.with(|h| h.borrow().as_ref().and_then(|hook| hook(call)))
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    pub(super) fn before_harden(stage: HardenStage, path: &Path) -> Option<std::io::Error> {
        let call = HardenCall {
            stage,
            path: path.to_path_buf(),
        };
        HARDEN_HOOK.with(|h| h.borrow().as_ref().and_then(|hook| hook(&call)))
    }

    #[cfg(unix)]
    pub(super) fn before_dir_sync(dir: &Path) -> Option<std::io::Error> {
        record(Step::DirSynced(dir.to_path_buf()));
        DIR_SYNC_FAILURE.with(|f| {
            f.get()
                .map(|kind| std::io::Error::new(kind, "injected directory sync failure"))
        })
    }

    #[cfg(unix)]
    pub(super) fn link_fallback() -> Option<bool> {
        LINK_FALLBACK.with(Cell::get)
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
/// Only complete generated temp names are ever considered (`.<name>.rktmp-<pid>-<n>-<nanos in
/// hex>-<attempt>`, or the `StateDir` form `.<name>.rktmp-<pid>-<n>-<attempt>`): a committed file
/// that merely contains [`TEMP_MARKER`] (`invoice.rktmp-1-final`) is left alone, and public
/// writes refuse targets with a generated temp name. Returns the number removed.
pub fn sweep_stale_temps(dir: &Path, max_age: Duration) -> Result<usize> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(FsError::io(dir, e)),
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(pid) = generated_temp_pid(&name) else {
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

#[cfg(test)]
mod plan_tests {
    use super::*;

    fn options(replace: ReplaceMode, durable: bool) -> AtomicWriteOptions {
        AtomicWriteOptions {
            replace,
            durable,
            ..AtomicWriteOptions::default()
        }
    }

    const MODES: [ReplaceMode; 4] = [
        ReplaceMode::Rename,
        ReplaceMode::ReplaceFile,
        ReplaceMode::MoveWriteThrough,
        ReplaceMode::NoReplace,
    ];

    /// `ReplaceFileW` documents `REPLACEFILE_WRITE_THROUGH` as unsupported: whatever the mode,
    /// durability and destination, the Windows plan hands it flag word 0.
    #[test]
    fn windows_replace_file_never_gets_the_unsupported_write_through_flag() {
        for replace in MODES {
            for durable in [true, false] {
                for exists in [true, false] {
                    let (api, flags) = plan_replace_on(true, options(replace, durable), exists);
                    if api == ReplaceApi::ReplaceFile {
                        assert_eq!(flags, 0, "{replace:?} durable={durable} exists={exists}");
                    }
                }
            }
        }
        assert_eq!(
            plan_replace_on(true, options(ReplaceMode::ReplaceFile, true), true),
            (ReplaceApi::ReplaceFile, 0)
        );
    }

    /// `ReplaceFileW` keeps the destination's DACL, so a private write over an existing
    /// destination is planned as a move, which carries the temp file's owner-only DACL.
    #[test]
    fn a_private_windows_replace_file_over_an_existing_destination_is_a_move() {
        let private = |privacy, durable| AtomicWriteOptions {
            privacy,
            durable,
            replace: ReplaceMode::ReplaceFile,
            ..AtomicWriteOptions::default()
        };
        assert_eq!(
            plan_replace_on(true, private(Privacy::OwnerOnly, true), true),
            (
                ReplaceApi::MoveFileEx,
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH
            )
        );
        assert_eq!(
            plan_replace_on(true, private(Privacy::OwnerOnly, false), true),
            (ReplaceApi::MoveFileEx, MOVEFILE_REPLACE_EXISTING)
        );
        let by_flag = AtomicWriteOptions {
            private: true,
            replace: ReplaceMode::ReplaceFile,
            ..AtomicWriteOptions::default()
        };
        assert_eq!(
            plan_replace_on(true, by_flag, true).0,
            ReplaceApi::MoveFileEx
        );
        // No ACL step is wanted for this privacy on Windows, so the destination's ACL is kept.
        assert_eq!(
            plan_replace_on(
                true,
                private(Privacy::OwnerOnlyUnixInheritWindows, true),
                true
            ),
            (ReplaceApi::ReplaceFile, 0)
        );
        assert_eq!(
            publication_durability_on(true, &by_flag, true),
            PublicationDurability::WriteThroughMove
        );
    }

    #[test]
    fn windows_write_through_is_only_ever_movefileex() {
        let both = MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH;
        assert_eq!(
            plan_replace_on(true, options(ReplaceMode::MoveWriteThrough, true), true),
            (ReplaceApi::MoveFileEx, both)
        );
        assert_eq!(
            plan_replace_on(true, options(ReplaceMode::ReplaceFile, true), false),
            (ReplaceApi::MoveFileEx, both),
            "first creation under ReplaceFile is a write-through move"
        );
        assert_eq!(
            plan_replace_on(true, options(ReplaceMode::NoReplace, true), true),
            (ReplaceApi::MoveFileEx, MOVEFILE_WRITE_THROUGH)
        );
        assert_eq!(
            plan_replace_on(true, options(ReplaceMode::Rename, true), true),
            (ReplaceApi::Rename, 0),
            "the default mode stays std::fs::rename"
        );
    }

    #[test]
    fn publication_durability_states_what_each_windows_mode_gives() {
        use PublicationDurability::*;
        let on_windows = |replace, durable, exists| {
            publication_durability_on(true, &options(replace, durable), exists)
        };
        assert_eq!(on_windows(ReplaceMode::Rename, true, true), FileFlushedOnly);
        assert_eq!(
            on_windows(ReplaceMode::Rename, true, false),
            FileFlushedOnly
        );
        assert_eq!(
            on_windows(ReplaceMode::ReplaceFile, true, true),
            FileFlushedOnly,
            "ReplaceFileW has no supported write-through"
        );
        assert_eq!(
            on_windows(ReplaceMode::ReplaceFile, true, false),
            WriteThroughMove
        );
        assert_eq!(
            on_windows(ReplaceMode::MoveWriteThrough, true, true),
            WriteThroughMove
        );
        assert_eq!(
            on_windows(ReplaceMode::NoReplace, true, false),
            WriteThroughMove
        );
        for replace in MODES {
            assert_eq!(on_windows(replace, false, true), None, "{replace:?}");
        }
    }

    #[test]
    fn publication_durability_off_windows_is_a_directory_flush() {
        for replace in MODES {
            let durable = options(replace, true);
            assert_eq!(
                publication_durability_on(false, &durable, true),
                PublicationDurability::DirectoryFlushed
            );
            assert_eq!(
                publication_durability_on(false, &options(replace, false), true),
                PublicationDurability::None
            );
        }
    }
}

#[cfg(test)]
mod partial_replace_tests {
    use super::*;
    use std::io::Error;

    fn code(code: i32) -> Error {
        Error::from_raw_os_error(code)
    }

    fn setup(
        target: bool,
        temp: bool,
        backup: bool,
    ) -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let (t, tmp, b) = (
            dir.path().join("t"),
            dir.path().join("tmp"),
            dir.path().join("bak"),
        );
        for (present, path) in [(target, &t), (temp, &tmp), (backup, &b)] {
            if present {
                fs::write(path, b"x").unwrap();
            }
        }
        (dir, t, tmp, b)
    }

    #[test]
    fn other_errors_and_an_intact_destination_are_ordinary() {
        let (_d, target, temp, backup) = setup(true, true, false);
        let kept = classify_partial(code(1176), &temp, &target, Some(&backup));
        assert!(
            PartialReplace::take(kept).is_err(),
            "destination intact, no backup"
        );
        let (_d, target, temp, backup) = setup(false, true, true);
        let other = classify_partial(code(5), &temp, &target, Some(&backup));
        assert!(
            PartialReplace::take(other).is_err(),
            "only 1176 and 1177 qualify"
        );
    }

    #[test]
    fn a_missing_destination_or_a_backup_beside_it_is_a_partial_state() {
        // Destination gone, old content in the backup, new content still in the temp.
        let (_d, target, temp, backup) = setup(false, true, true);
        let partial =
            PartialReplace::take(classify_partial(code(1177), &temp, &target, Some(&backup)))
                .expect("partial");
        assert_eq!(partial.new_content, Some(temp.clone()));
        assert_eq!(partial.previous_content, Some(backup.clone()));
        // Destination gone and no backup (1176 without one): the old content is lost, the new
        // content is the temp.
        let (_d, target, temp, _backup) = setup(false, true, false);
        let partial = PartialReplace::take(classify_partial(code(1176), &temp, &target, None))
            .expect("partial");
        assert_eq!(partial.new_content, Some(temp));
        assert_eq!(partial.previous_content, None);
    }

    #[test]
    fn backup_names_are_unique_and_never_swept() {
        let target = Path::new("/d/state.json");
        let (a, b) = (backup_path(target), backup_path(target));
        assert_ne!(a, b);
        let name = a.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with(".state.json.rktbak-"), "{name}");
        assert_eq!(generated_temp_pid(&name), None);
    }
}

#[cfg(test)]
mod temp_name_tests {
    use super::*;

    #[test]
    fn every_name_this_crate_generates_parses_back_to_its_pid() {
        for attempt in 0..TEMP_ATTEMPTS {
            let atomic = temp_name("state.json", attempt);
            assert_eq!(
                generated_temp_pid(&atomic),
                Some(std::process::id()),
                "{atomic}"
            );
        }
        // The `StateDir` form has no nanosecond field.
        assert_eq!(generated_temp_pid(".a.rktmp-123-45-0"), Some(123));
        assert_eq!(generated_temp_pid(".a.rktmp-123-45-ff-31"), Some(123));
        // A real name that itself contains the marker still parses by its generated tail.
        assert_eq!(generated_temp_pid(".a.rktmp-9-9-9.rktmp-5-6-7-0"), Some(5));
        assert_eq!(
            generated_temp_pid("..rktmp-5-6-7"),
            None,
            "no name before the marker"
        );
    }

    #[test]
    fn names_that_merely_contain_the_marker_are_not_temp_names() {
        for name in [
            "invoice.rktmp-1-final",
            ".invoice.rktmp-1-final",
            "invoice.rktmp-1-2-3",
            "a.rktmp-1",
            ".a.rktmp-1",
            ".a.rktmp-1-2",
            ".a.rktmp-1-2-3-4-5",
            ".a.rktmp--2-0",
            ".a.rktmp-1-2-32",
            ".a.rktmp-1-2-xyz-0",
            ".a.rktmp-1-2-123456789-0",
            ".a.rktmp-99999999999-2-0",
            ".a.rktmp-1-+2-0",
            ".rktmp-1-2-0",
            ".a.rktmp-",
            "a",
        ] {
            assert_eq!(generated_temp_pid(name), None, "{name}");
            assert!(reject_reserved_name(name).is_ok(), "{name}");
        }
        assert!(matches!(
            reject_reserved_name(".a.rktmp-1-2-0"),
            Err(FsError::BadName(_))
        ));
    }
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

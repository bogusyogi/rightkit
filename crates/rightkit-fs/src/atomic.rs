//! Atomic file replacement: temp file in the destination directory, `fsync`, rename over the
//! target, `fsync` the directory. Readers see the old content or the new content, never a mix,
//! and a crash at any point leaves the old file intact plus (at worst) an orphan temp file
//! that generic sweeps retain because its name proves no ownership.
//!
//! A write has two phases. Until the replace call succeeds nothing is published: any failure
//! leaves the destination as it was and removes the temp file. After it succeeds the new content
//! is in place, and every later failure (the directory sync, removing a leftover temp name) is a
//! typed post-publication error whose [`FsError::target_replaced`] is true, reported only after
//! the required directory sync ran.
//!
//! # Every publish path
//!
//! Notation. *flush* is `durable_sync`: `F_FULLFSYNC` with the Apple ladder, `fsync(2)` elsewhere on
//! Unix, `FlushFileBuffers` on Windows. *dir sync* is `fsync` of the destination's directory (Unix
//! only; Windows has no directory flush, see [`publication_durability`]). Every write passes the
//! same phases in this order: (1) create the temp, (2) write, (3) flush (`durable`; or, for
//! [`AtomicWriter::commit_checked`] in any mode, `File::sync_all`), (4) the caller's check, (5)
//! publish with one OS call, (6) dir sync (`durable`, Unix), (7) report. Before step 5 nothing is
//! published: a failure leaves the destination as it was and removes the temp. A crash before step
//! 5 leaves the destination as it was plus an orphan `.<name>.rktmp-<pid>-...` that
//! generic sweeps retain. "Test" names a test in `tests/` (or a unit test in this file);
//! "seam" means the row cannot be produced without fault injection; no build produces it.
//!
//! ## 1. Creating the temp (step 1), per platform x privacy x destination
//!
//! | Platform | Privacy | Destination | Temp is created with | Notes | Test |
//! |---|---|---|---|---|---|
//! | Unix | `Inherit` | absent, or not a regular file (a symlink is never followed) | `open(O_CREAT\|O_EXCL\|O_NOFOLLOW\|O_CLOEXEC, 0666 & ~umask)` | | `atomic_privacy.rs::inherit_is_unchanged_and_private_true_wins_over_privacy` |
//! | Unix | `Inherit` | regular file | the same `open`, mode = the file's mode & ~umask, then `fchmod` to its exact bits | never wider than the file it replaces | `e2e.rs::private_writes_are_owner_only_and_plain_writes_keep_the_existing_mode` |
//! | Unix | `Inherit` | symlink | as for absent: the link's target lends no mode | the rename replaces the link itself | `atomic_privacy.rs::a_symlink_at_the_destination_is_replaced_not_followed_and_lends_no_mode` |
//! | Unix | `OwnerOnly`, `OwnerOnlyUnixInheritWindows`, `private: true` | any | `open(... , 0600)` (final bits at creation, no later `chmod`); parents `0700` | macOS: the parent's ACL is read first, and an inheritable `allow` entry refuses the write ([`FsError::UnsafeAcl`]) before anything exists; a temp that carries an `allow` entry anyway is removed while still empty and the write is refused with the same error; a deny-only ACL passes. No ACL is ever removed from an inode, because that cannot revoke a descriptor already open (see `acl`) | `atomic_publish.rs::the_temp_is_owner_only_before_the_replace_publishes_it`; macOS ACLs: `acl_macos.rs::an_inheriting_parent_refuses_a_private_atomic_write_and_creates_nothing` |
//! | Windows | `Inherit`, `OwnerOnlyUnixInheritWindows` | any | `OpenOptions::create_new`: the parent's inherited ACL | | `atomic_publish.rs::windows::inherit_privacy_takes_no_owner_only_step`, `atomic_dacl_windows.rs::privacy_variants_on_windows` |
//! | Windows | `OwnerOnly`, `private: true` | any | `CreateFileW(CREATE_NEW)` with the descriptor `D:P(A;;FA;;;<user>)` in `SECURITY_ATTRIBUTES` (never created then restricted); fails closed, nothing created, if the descriptor cannot be built | | `atomic_publish.rs::windows::the_temp_carries_its_owner_only_descriptor_from_creation_and_is_never_restricted_later`, `a_failed_owner_only_creation_publishes_nothing_and_leaves_no_temp` |
//! | all | any | a target named like a generated temp name, or, on Windows, one that Windows would store as such a name: it drops leading spaces, trailing spaces and periods, and a `:stream` suffix | refused before anything is created ([`FsError::BadName`]) | | `atomic_namespace.rs::a_public_write_cannot_target_a_generated_temp_name`, `windows_trailing_space_and_period_names_are_refused`; unit `temp_name_tests::windows_names_that_normalize_to_a_temp_name_are_reserved` |
//!
//! ## 2. Publishing (step 5) and what a crash leaves, per platform x mode x durable x destination
//!
//! Privacy does not change the publish call: a private temp already carries its owner-only mode or
//! descriptor, and every publish below moves that temp, with its ACL, into place. On Unix the two
//! replacing modes are the same `rename(2)`.
//!
//! | # | Platform | Mode | durable | Destination | Flushed, and when | OS call that publishes | A crash after each step leaves | Error | Test |
//! |---|---|---|---|---|---|---|---|---|---|
//! | U1 | Unix | `Rename`, `MoveWriteThrough` | yes | exists or not | temp (step 3) before the call; dir (step 6) after | `rename(2)` | before the call: destination as it was, orphan temp. After the call, before the dir sync: old or new content after reboot, never a mix (the temp's data was flushed first). After the dir sync: new | replace failed: `Io`, temp removed, destination untouched. Dir sync failed: `ReplacedButDirSyncFailed` (`target_replaced()`) | `atomic_publish.rs::a_durable_write_flushes_the_temp_then_replaces_then_syncs_the_directory`, `a_failed_replace_syncs_no_directory_and_publishes_nothing`; `atomic_options.rs::directory_sync_failure_after_replace_is_typed_and_keeps_new_content` |
//! | U2 | Unix | same | no | exists or not | nothing (with `commit_checked`: the temp only, never the dir) | `rename(2)` | old or new name, but the new file's *data* may be lost or short after power loss: that is what `durable: false` means | as U1, no dir sync | `atomic_publish.rs::a_non_durable_write_flushes_and_syncs_nothing`, `atomic_options.rs::directory_sync_is_skipped_when_not_durable`, `atomic_checked_sync.rs::a_non_durable_checked_write_syncs_the_file_before_the_check_and_never_the_directory` |
//! | U3 | Unix | `NoReplace` | yes | absent | temp before; dir after | `renameat2(RENAME_NOREPLACE)` (Linux), `renamex_np(RENAME_EXCL)` (Apple) | as U1 | as U1 | `atomic_publish.rs::no_replace_flushes_publishes_exclusively_then_syncs_the_directory`, `atomic_exclusive.rs::publishes_when_absent_and_refuses_when_present_without_touching_the_old_file` |
//! | U4 | Unix | `NoReplace` | any | exists, or appears first | temp (durable) | the same call, which fails `EEXIST` | destination untouched, temp removed, no dir sync | `AlreadyExists` | `atomic_publish.rs::a_no_replace_loser_syncs_no_directory_and_leaves_the_winner`, `atomic_exclusive.rs::racing_publishers_have_exactly_one_winner_and_the_losers_leave_no_temp` |
//! | U5 | Unix | `NoReplace`, kernel/filesystem without the exclusive rename | any | absent | as U3 | `link(2)` then `unlink(2)` of the temp name | between the two calls both names are one inode with the full content (published); a crash leaves an ambiguous temp name retained by generic sweeps | `link` fails `EEXIST`: `AlreadyExists`. `unlink` fails: dir sync still runs, then `ReplacedButPostStepFailed(TempCleanup)` | `atomic_publish.rs::post_publication::the_link_fallback_publishes_cleanly_when_the_unlink_works`, `a_failed_unlink_after_the_link_is_a_typed_error_and_the_directory_is_still_synced`; the real ENOSYS/EINVAL fallback needs an old kernel: seam `force_link_fallback` |
//! | U5b | Unix | `NoReplace`, no exclusive rename and `link(2)` fails (`EPERM`, `ENOTSUP`, `EMLINK`, `EINVAL`, `ENOSYS`: FAT, exFAT, some shares) | any | absent | as U3 | `open(O_CREAT\|O_EXCL)` of the destination name (an empty placeholder), then `rename(2)` of the temp over it | between the two calls the destination name holds an empty file; a crash there leaves that empty file at the destination | placeholder exists (or appears first): `AlreadyExists`, temp removed. The rename fails: the placeholder is removed, nothing published | `atomic_exclusive_nolink.rs::*`; seam `force_no_hard_links`; no FAT volume has been used |
//! | U6 | Android | all | any | any | as U1 to U5 | `rename(2)` and `fsync(2)` as on Linux (`sync_all`, reported `SyncLevel::Full`; there is no `F_FULLFSYNC` and none is tried); `NoReplace` always takes the U5 `link` then `unlink` path (no raw `renameat2`) | as the row it follows | as the row it follows | host: `app_dirs` injected-root tests; device behaviour is unverified (needs an emulator run) |
//! | W1 | Windows | `Rename` | yes | exists or not | temp (`FlushFileBuffers`) before the call; no dir sync exists | `std::fs::rename` (POSIX-semantics rename; not write-through) | before: destination as it was, orphan temp. After: old or new, never a mix, but the *swap itself* may be lost in a power failure (`FileFlushedOnly`) | `Io`, temp removed | `atomic_publish.rs::a_durable_write_on_windows_flushes_the_temp_and_claims_no_directory_sync`; plan: unit `plan_tests::publication_durability_states_what_each_windows_mode_gives` |
//! | W2 | Windows | `Rename` | no | exists or not | nothing | `std::fs::rename` | as W1, without the temp flush | `Io` | `atomic_publish.rs::a_non_durable_write_flushes_and_syncs_nothing` |
//! | W3 | Windows | `MoveWriteThrough` | yes / no | exists or not | temp flushed (durable) | `MoveFileExW(MOVEFILE_REPLACE_EXISTING \| MOVEFILE_WRITE_THROUGH)` (also when not `durable`) | destination old or new; committed when the call returns | `Io`; sharing/lock violations retried per [`RetryPolicy`] | `atomic_testing.rs::the_flags_each_mode_hands_the_os`, unit `retry_tests::*`; no power-loss test has been run (the guarantee is Microsoft's) |
//! | W4 | Windows | `NoReplace` | yes / no | absent | temp flushed (durable) | `MoveFileExW(0 [\| MOVEFILE_WRITE_THROUGH when durable])`: fails if the name exists | as W3 | exists or appears first: `AlreadyExists`, temp removed | `atomic_exclusive.rs::publishes_when_absent_and_refuses_when_present_without_touching_the_old_file` |
//!
//! There is no `ReplaceFileW` path: it can fail after it has already removed the destination
//! (error 1176), leaving neither version under the destination name, and this crate has no
//! recovery protocol for that state. A Windows write over an existing file is a rename (W1, W2)
//! or a move (W3).
//!
//! ## 3. Directories made for `create_parents`
//!
//! The same table, for directories, is in the `state` module docs (`StateDir::create`, `child`).
//! For [`create_dir_all_durable`], which a durable `create_parents` write calls. The rule is the
//! same everywhere: the link of a directory is synced (its parent `fsync`ed, before the next
//! component) **only when this call created it**, meaning the create returned `Ok` and not
//! `AlreadyExists`. There is no record of earlier syncs and no walk over components that already
//! exist, so a write whose directories all exist makes no ancestor sync; the destination
//! directory's own sync after the published file is the write's (table 2), not this one's.
//!
//! **Residual, stated here once.** A directory created by an earlier call that died before its
//! parent sync, or whose parent sync failed (`DirSyncFailed`: the directory stays), is not synced
//! by later calls: they find it and leave it, however it got there. That is still strictly more
//! than 0.3.3, which synced no ancestor at all, but it is not a guarantee that every directory on
//! the path has a durable link. A symlinked component is likewise left alone, and the ancestors of
//! its target are not walked.
//!
//! | Platform | Component | Action | A crash leaves | Error | Test |
//! |---|---|---|---|---|---|
//! | Unix | missing, and this call's create returns `Ok` | `mkdir` (`0700` when private), then `fsync` of its parent before the next component | before that sync: possibly no directory after reboot; nothing was written below it yet | `DirSyncFailed` (the directory stays; see the residual) | `atomic_publish.rs::parents::every_created_ancestor_is_published_before_the_file_is_written`, `a_failed_ancestor_sync_stops_the_write_and_the_retry_syncs_only_what_it_creates` |
//! | Unix | exists (made by an earlier call, another process, a crashed run, or moved into place) | none: not looked at again, so a second identical call syncs no ancestor | n/a (see the residual) | none | `atomic_publish.rs::parents::a_second_identical_write_syncs_no_ancestor`, `a_pre_existing_directory_is_not_synced_only_what_the_call_creates_is`, `a_directory_moved_into_place_is_a_pre_existing_directory` |
//! | Unix | missing, but a racing creator wins (`AlreadyExists`) | none: theirs to publish | n/a | none | none: needs a racing creator |
//! | Unix | a symlink | none | n/a | none | none: an open gap, stated in the README |
//! | Unix, macOS | missing under a parent with an inheritable `allow` ACL entry (private only) | refused before `mkdir`; a directory that got an `allow` entry anyway is removed (empty) and refused; deny-only ACLs pass | nothing created | [`FsError::UnsafeAcl`] | `acl_macos.rs::an_inheriting_parent_refuses_private_parents` |
//! | Unix | `durable: false` | `create_dir_all` (or `ensure_private_dir`), no sync | the directories may be lost with the file, as the mode says | none | `atomic_publish.rs::parents::a_non_durable_write_makes_parents_without_syncing` |
//! | Windows | missing, and this call's create returns `Ok` | `create_dir` (or `ensure_private_dir`), then `FlushFileBuffers` on a `GENERIC_WRITE` + `FILE_FLAG_BACKUP_SEMANTICS` handle of the new directory and then of its parent, before the next component. A parent this call did not create that refuses that handle with `ERROR_ACCESS_DENIED` is skipped | before the flushes: possibly no directory after power loss, with nothing written below it yet | `DirSyncFailed` (the directory stays; see the residual) | `atomic_publish.rs::windows::created_parents_are_flushed_top_down_before_the_file_is_written`, `a_failed_parent_flush_is_a_dir_sync_failed_and_the_directory_stays`. The calls are observed through the `testing` seam; that the flush reaches the medium cannot be tested without power-loss injection |
//! | Windows | exists, a racing creator wins, or a symlink | none: not looked at again, so a second identical call flushes none | n/a (see the residual) | none | `atomic_publish.rs::windows::a_pre_existing_directory_is_not_flushed_only_what_the_call_creates_is` |
//! | Windows | `durable: false` | `create_dir_all` (or `ensure_private_dir`), no flush | the directories may be lost with the file, as the mode says | none | none |
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
/// | [`Privacy::OwnerOnly`] | `0600` from creation (parents `0700`) | one protected ACE for the current user (`D:P(A;;FA;;;<user>)`) passed to `CreateFileW` when the temp file is created, so it never exists with a broader ACL; every publish moves the temp file, so its DACL is the published file's |
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
    /// reader holding the old file open with `FILE_SHARE_DELETE` (what Rust's `File::open` asks
    /// for) does not block the writer. A reader that opened it WITHOUT delete sharing does: the
    /// write fails with `ERROR_ACCESS_DENIED` (5), the old file intact and the temp file removed
    /// (observed by a consumer's test on Windows 11, not by a test in this crate). That is not a
    /// sharing violation, so `retry_sharing_violation` does not retry it; a caller that wants a
    /// bounded retry of that case opts in explicitly with [`AccessDeniedRetry`]. The default mode.
    ///
    /// What a durable write guarantees when it returns `Ok`, per platform:
    ///
    /// * Unix: the temp file was `fsync`ed (`F_FULLFSYNC` first on Apple) before the `rename`, and the
    ///   directory was `fsync`ed after it, so the new name and content survive power loss.
    /// * Windows: the temp file was flushed (`FlushFileBuffers`) before the rename. The rename itself
    ///   is not issued write-through and there is no directory `fsync`, so after a power loss the
    ///   old content may reappear. Use [`ReplaceMode::MoveWriteThrough`] where that matters.
    ///
    /// With `durable: false` no flush is done on any platform, except that [`AtomicWriter::commit_checked`] still file-syncs the temp file before its check. [`publication_durability`] reports
    /// which of these a given option set gets.
    #[default]
    Rename,
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
    /// between the two leaves a temp file retained by [`sweep_stale_temps`]. If the `unlink` fails after the
    /// `link` the content is published and complete: the write still syncs the directory, then
    /// returns [`FsError::ReplacedButPostStepFailed`] ([`crate::PostPublishStep::TempCleanup`]).
    /// Windows: `MoveFileExW` without `MOVEFILE_REPLACE_EXISTING` (write-through when `durable`).
    /// See also [`rename_exclusive`].
    ///
    /// **Volumes without hard links** (FAT, exFAT, some network shares), where `link(2)` fails
    /// (`EPERM`, `ENOTSUP`, `EMLINK`, `EINVAL` or `ENOSYS`): a third step on Unix, after the exclusive
    /// rename and the `link` have both been unavailable. The destination name is claimed with an
    /// `O_EXCL` placeholder (`create_new`), then the finished temp file is renamed over it
    /// ([`ExclusiveMethod::PlaceholderThenRename`]). Per platform, what is guaranteed:
    ///
    /// * Linux, Apple (exclusive rename works): atomic; the name is never visible empty.
    /// * Unix without the exclusive rename, with hard links: exclusive; both names exist for an
    ///   instant (`link` then `unlink`).
    /// * Unix without either (FAT-class): exclusive against every other exclusive publisher
    ///   (`O_EXCL` decides the winner; the loser gets [`FsError::AlreadyExists`] and its temp is
    ///   removed). Not atomic for readers: the name holds an empty file between the claim and the
    ///   rename, and a crash there leaves that empty file at the destination (the caller's next
    ///   exclusive publish to it is refused). A writer that replaces without exclusivity
    ///   (`ReplaceMode::Rename`) can overwrite the placeholder. If the rename fails, the placeholder
    ///   is removed and nothing is published.
    /// * Windows: `MoveFileExW` without replace, atomic where the volume supports it; no hard links
    ///   are needed, so there is no fallback (not exercised on FAT or a network share by this crate).
    NoReplace,
}

/// Retry for a publish step that fails because another process holds the destination open.
///
/// Only Windows sharing and lock violations (`ERROR_SHARING_VIOLATION` 32 and
/// `ERROR_LOCK_VIOLATION` 33) are retried. Other platforms never report them, so the policy is
/// inert there.
///
/// `ERROR_ACCESS_DENIED` (5) is never retried through this policy, because it is also what a real
/// permission failure returns. A caller that knows a reader may hold the destination without
/// delete sharing can opt in separately with [`AccessDeniedRetry`], on the entry points listed
/// there; the default stays 32/33 only.
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

/// An explicit, bounded opt-in to retry Windows `ERROR_ACCESS_DENIED` (5) in the publish step, in
/// addition to the sharing (32) and lock (33) violations.
///
/// The case it exists for: a reader holds the destination open without `FILE_SHARE_DELETE`, so the
/// replace fails with error 5 although nothing about the caller's rights is wrong
/// ([`ReplaceMode::Rename`]). Error 5 is also what a real ACL denial returns, so it is never
/// retried unless the caller passes this value to one of the entry points that honour it:
///
/// * [`atomic_write_with_access_denied_retry`]
/// * [`publish_file_with_access_denied_retry`]
/// * [`AtomicWriter::create_with_access_denied_retry`] (and so every commit variant of that writer)
///
/// Every other entry point (`atomic_write_with_options`, `publish_file_with`,
/// `AtomicWriter::create_with`, `publish_file_first_free`, the verified-copy, clone, lease, state
/// and `publish_dir` paths) keeps the 32/33-only default and ignores this type.
///
/// Semantics, all fixed:
///
/// * Only the publish (replace) call is retried; nothing is re-written and no other step runs
///   again.
/// * The bound is the wrapped [`RetryPolicy`]: `attempts` retries after the first attempt, `delay`
///   before each. It **replaces** `AtomicWriteOptions::retry_sharing_violation` for that write, so
///   32, 33 and 5 all share one budget.
/// * After the bound the original final error is returned unchanged (`FsError::Io` carrying the
///   OS error 5), so a true ACL denial still fails, only later by `attempts * delay`.
/// * No access check is weakened: no ACL or attribute is modified and the replace is issued with
///   the same API, flags and open mode on every try.
/// * Windows only. Elsewhere error 5 is not a Windows code, so the opt-in is inert and the write
///   behaves as `retry_sharing_violation` would.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccessDeniedRetry {
    policy: RetryPolicy,
}

impl AccessDeniedRetry {
    /// Retry error 5 (and 32/33) up to `policy.attempts` times, pausing `policy.delay` before each.
    pub fn new(policy: RetryPolicy) -> Self {
        Self { policy }
    }

    /// The bound this opt-in applies.
    pub fn policy(&self) -> RetryPolicy {
        self.policy
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

/// [`atomic_write_with_options`] with an explicit [`AccessDeniedRetry`] opt-in: the publish step
/// also retries Windows `ERROR_ACCESS_DENIED` (5), bounded by `retry`, which replaces
/// `options.retry_sharing_violation` for this write. The original error is returned after the
/// bound. See [`AccessDeniedRetry`] for the exact semantics.
pub fn atomic_write_with_access_denied_retry(
    path: &Path,
    options: AtomicWriteOptions,
    retry: AccessDeniedRetry,
    fill: impl FnOnce(&mut File) -> std::io::Result<()>,
) -> Result<()> {
    let mut writer = AtomicWriter::create_with_access_denied_retry(path, options, retry)?;
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
/// Mechanisms and guarantees per platform are on [`ReplaceMode::NoReplace`]; [`rename_exclusive_reporting`]
/// says which one ran.
///
/// `to` must not be shaped like a temp name this crate generates ([`FsError::BadName`]). Where the
/// kernel has no exclusive rename the fallback is `link` then `unlink`, and where the volume has no
/// hard links either (FAT, exFAT, some shares) an `O_EXCL` placeholder at `to` that `from` is then
/// renamed over (see the guarantee there: not atomic for readers); if the `unlink` fails,
/// `to` exists with the content and this returns [`FsError::ReplacedButPostStepFailed`]
/// ([`crate::PostPublishStep::TempCleanup`]), with `from` still present as a second name.
pub fn rename_exclusive(from: &Path, to: &Path) -> Result<()> {
    rename_exclusive_reporting(from, to).map(|_| ())
}

/// [`rename_exclusive`], returning the [`ExclusiveMethod`] that published the file, so a caller
/// (or a test) can see that a volume without hard links took the placeholder path.
pub fn rename_exclusive_reporting(from: &Path, to: &Path) -> Result<ExclusiveMethod> {
    if let Some(name) = to.file_name() {
        reject_reserved_name(&name.to_string_lossy())?;
    }
    match rename_exclusive_io(from, to) {
        Ok(done) => match done.cleanup {
            None => Ok(done.method),
            Some(source) => Err(FsError::ReplacedButPostStepFailed {
                path: to.to_path_buf(),
                step: crate::PostPublishStep::TempCleanup,
                source,
            }),
        },
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(FsError::AlreadyExists(to.to_path_buf()))
        }
        Err(e) => Err(FsError::io(to, e)),
    }
}

/// Durably publish an already-written file at `to`, replacing `to` if it exists.
///
/// For files another process produced (an encoder's output, a download): `from` is synced to
/// stable storage, renamed over `to`, and the directory is synced. `from` must be a regular file
/// on the same filesystem as `to` and must be closed by its writer. Same as
/// [`publish_file_with`] with [`ReplaceMode::MoveWriteThrough`], `durable: true`.
///
/// Per platform: Unix flushes `from` (`F_FULLFSYNC` first on Apple), `rename(2)`s, then `fsync`s
/// the directory (both directories when they differ). Windows flushes `from`
/// (`FlushFileBuffers`) and calls `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING |
/// MOVEFILE_WRITE_THROUGH`; there is no directory flush there.
///
/// Failure before the replace leaves `to` untouched and `from` in place (it is the caller's
/// file; this never removes it). A failure after the replace is reported as
/// [`FsError::ReplacedButDirSyncFailed`], with `to` holding the new content.
pub fn publish_file(from: &Path, to: &Path) -> Result<()> {
    publish_file_with(
        from,
        to,
        AtomicWriteOptions {
            replace: ReplaceMode::MoveWriteThrough,
            ..AtomicWriteOptions::default()
        },
    )
}

/// [`publish_file`] with explicit [`AtomicWriteOptions`].
///
/// Honoured: `durable` (sync `from` first and the directory after; `false` does neither),
/// `replace` (use [`ReplaceMode::NoReplace`] to refuse an existing `to` with
/// [`FsError::AlreadyExists`]), `retry_sharing_violation` and `create_parents`. Ignored:
/// `private` and `privacy`, because the file already exists with the permissions its writer gave
/// it; apply them before publishing if needed. `to` must not be shaped like a temp name this
/// crate generates ([`FsError::BadName`]).
pub fn publish_file_with(from: &Path, to: &Path, options: AtomicWriteOptions) -> Result<()> {
    publish_file_impl(from, to, options, None, true)
}

/// [`publish_file_with`] with an explicit [`AccessDeniedRetry`] opt-in: the replace step also
/// retries Windows `ERROR_ACCESS_DENIED` (5), bounded by `retry`, which replaces
/// `options.retry_sharing_violation` for this call. The original error is returned after the
/// bound, with `from` untouched. See [`AccessDeniedRetry`] for the exact semantics.
pub fn publish_file_with_access_denied_retry(
    from: &Path,
    to: &Path,
    options: AtomicWriteOptions,
    retry: AccessDeniedRetry,
) -> Result<()> {
    publish_file_impl(from, to, options, Some(retry), true)
}

/// Publish the finished file `from` at `to`, replacing an existing file, with the write-through
/// move and the bounded Windows retry: a sharing violation or access-denied refusal (another
/// process holding the old file open) is retried, five attempts in total, 100 ms apart. Inert
/// elsewhere. The one copy of the helper that `rightkit-library` and `rightkit-media-fetch`
/// each defined.
pub fn publish_replacing(from: &Path, to: &Path) -> Result<()> {
    publish_file_with_access_denied_retry(
        from,
        to,
        AtomicWriteOptions {
            replace: ReplaceMode::MoveWriteThrough,
            ..AtomicWriteOptions::default()
        },
        AccessDeniedRetry::new(RetryPolicy::new(4, Duration::from_millis(100))),
    )
}

/// Publish the finished file `from` at the first of `candidates` that is free, without rewriting
/// it. For a caller that wants "`name.dng`, else `name-1.dng`, else ..." next to its inputs.
///
/// Each candidate is tried in order with an exclusive publish (as [`ReplaceMode::NoReplace`], so
/// never check-then-rename): one that already exists, or that another process takes first, is
/// skipped. The source is synced once (`durable`), not once per candidate. Returns the path that
/// was published. Every other error ends the search at once and is returned as
/// [`publish_file_with`] would return it, so a failure after the publish
/// ([`FsError::ReplacedButDirSyncFailed`], [`FsError::ReplacedButPostStepFailed`]) names the
/// winning candidate in its path and has [`FsError::target_replaced`] true. When every candidate
/// exists the result is [`FsError::AlreadyExists`] for the last one; an empty list is an
/// `InvalidInput` error. The mechanisms, including the one for filesystems without hard links, are
/// those of [`rename_exclusive`]. `options.replace` is ignored (it is always `NoReplace`); the
/// other options are honoured as in [`publish_file_with`], `create_parents` per candidate.
pub fn publish_file_first_free(
    from: &Path,
    candidates: &[PathBuf],
    options: AtomicWriteOptions,
) -> Result<PathBuf> {
    let options = AtomicWriteOptions {
        replace: ReplaceMode::NoReplace,
        ..options
    };
    let Some(last) = candidates.last() else {
        return Err(FsError::io(
            from,
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "publish_file_first_free needs at least one candidate",
            ),
        ));
    };
    let mut sync_source = true;
    for candidate in candidates {
        match publish_file_impl(from, candidate, options, None, sync_source) {
            Ok(()) => return Ok(candidate.clone()),
            Err(FsError::AlreadyExists(_)) => {
                // The first attempt synced the source before it reached the replace.
                sync_source = false;
            }
            Err(e) => return Err(e),
        }
    }
    Err(FsError::AlreadyExists(last.clone()))
}

fn publish_file_impl(
    from: &Path,
    to: &Path,
    options: AtomicWriteOptions,
    access_denied: Option<AccessDeniedRetry>,
    sync_source: bool,
) -> Result<()> {
    if let Some(name) = to.file_name() {
        reject_reserved_name(&name.to_string_lossy())?;
    }
    let meta = fs::symlink_metadata(from).map_err(|e| FsError::io(from, e))?;
    if !meta.is_file() {
        return Err(FsError::io(
            from,
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "publish_file source is not a regular file",
            ),
        ));
    }
    if options.create_parents {
        create_dir_all_durable(&parent_of(to), false)?;
    }
    if options.durable && sync_source {
        // Windows needs write access for FlushFileBuffers; Unix fsync works on a read-only fd.
        let mut open = OpenOptions::new();
        open.read(true);
        #[cfg(windows)]
        open.write(true);
        let file = open.open(from).map_err(|e| FsError::io(from, e))?;
        durable_sync_with(&file, SyncPolicy::Fallback).map_err(|e| FsError::io(from, e))?;
    }
    let published = publish(from, to, options, access_denied)?;
    let dir_sync = if options.durable {
        let (to_dir, from_dir) = (parent_of(to), parent_of(from));
        let mut result = try_sync_dir(&to_dir);
        if from_dir != to_dir {
            let other = try_sync_dir(&from_dir);
            result = result.and(other);
        }
        result.err()
    } else {
        None
    };
    if let Some(source) = dir_sync {
        return Err(FsError::ReplacedButDirSyncFailed {
            path: to.to_path_buf(),
            source,
        });
    }
    if let Some(source) = published.cleanup {
        return Err(FsError::ReplacedButPostStepFailed {
            path: to.to_path_buf(),
            step: crate::PostPublishStep::TempCleanup,
            source,
        });
    }
    Ok(())
}

/// How an exclusive publish ([`rename_exclusive`], [`ReplaceMode::NoReplace`]) claimed the name.
/// See [`ReplaceMode::NoReplace`] for what each one guarantees per platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExclusiveMethod {
    /// One atomic create-new rename: `renameat2(RENAME_NOREPLACE)` (Linux), `renamex_np(RENAME_EXCL)`
    /// (Apple). The destination appears whole, or not at all, and the name is never visible empty.
    ExclusiveRename,
    /// `MoveFileExW` without `MOVEFILE_REPLACE_EXISTING` (Windows): fails if the name exists.
    MoveFile,
    /// `link(2)` then `unlink(2)` of the source name: exclusive, with both names for an instant.
    LinkThenUnlink,
    /// No exclusive rename and no hard links on this volume (FAT, exFAT, some network shares): the
    /// destination name was claimed with an `O_EXCL` (`create_new`) placeholder, then the finished
    /// file was renamed over the placeholder. The name is exclusive, but for the instant between
    /// the two steps it holds an empty file, and a crash there leaves that empty file.
    PlaceholderThenRename,
}

/// What an exclusive rename did: how it claimed the name, and the failure, if any, of removing a
/// leftover second name for the published file (the `link` fallback). The content is in place
/// either way.
struct Exclusive {
    method: ExclusiveMethod,
    cleanup: Option<std::io::Error>,
}

impl Exclusive {
    fn clean(method: ExclusiveMethod) -> Self {
        Self {
            method,
            cleanup: None,
        }
    }
}

/// Renames `from` to `to` only if `to` does not exist. `Ok`: published; `cleanup` carries the
/// failure of removing the `from` name when the `link` fallback was used. `Err`: nothing was
/// published.
#[cfg(windows)]
fn rename_exclusive_io(from: &Path, to: &Path) -> std::io::Result<Exclusive> {
    move_file_ex(from, to, 0).map(|()| Exclusive::clean(ExclusiveMethod::MoveFile))
}

#[cfg(unix)]
fn rename_exclusive_io(from: &Path, to: &Path) -> std::io::Result<Exclusive> {
    rename_exclusive_io_captured(from, to, None)
}

#[cfg(unix)]
fn rename_exclusive_io_captured(
    from: &Path,
    to: &Path,
    captured: Option<(&File, bool)>,
) -> std::io::Result<Exclusive> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let cfrom = CString::new(from.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let cto = CString::new(to.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    // Android is `target_os = "android"`, not `"linux"`, so it skips the raw `renameat2` on purpose and
    // takes the `link` then `unlink` path below (row U5): an app process runs under a seccomp
    // allow-list on some releases, where a syscall outside the list kills the process (SIGSYS)
    // instead of failing with ENOSYS, and the `ENOSYS` probe here cannot catch that. Everything
    // else in the Unix rows (`fsync`, `rename(2)`, directory `fsync`) is plain Linux behaviour.
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
            return Ok(Exclusive::clean(ExclusiveMethod::ExclusiveRename));
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
            return Ok(Exclusive::clean(ExclusiveMethod::ExclusiveRename));
        }
        let error = std::io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::ENOTSUP) | Some(libc::EINVAL) | Some(libc::ENOSYS) => {}
            _ => return Err(error),
        }
    }
    let _ = (&cfrom, &cto);
    link_then_unlink_captured(from, to, false, captured)
}

#[cfg(not(any(unix, windows)))]
fn rename_exclusive_io(from: &Path, to: &Path) -> std::io::Result<Exclusive> {
    link_then_unlink(from, to, false)
}

/// Whether a failed `link(2)` means "this volume cannot make hard links" (as opposed to the name
/// existing, the source missing, or any other real failure). `EPERM` is what Linux returns on FAT
/// and exFAT, `ENOTSUP`/`EOPNOTSUPP` what macOS and some network shares return, `EMLINK` a volume
/// with a link-count limit, `EINVAL` and `ENOSYS` a filesystem or kernel without the operation.
#[cfg_attr(windows, allow(dead_code))]
fn hard_links_unsupported(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    if let Some(code) = error.raw_os_error() {
        return matches!(
            code,
            libc::EPERM | libc::ENOTSUP | libc::ENOSYS | libc::EINVAL | libc::EMLINK
        );
    }
    error.kind() == std::io::ErrorKind::Unsupported
}

/// No exclusive rename here: `link` fails with `EEXIST` if `to` exists, then drop the old name.
/// Once the `link` succeeds `to` is published, so a failed `unlink` is returned in `cleanup`
/// (published, with a leftover second name for the same inode), never as an error. A volume that
/// cannot make hard links at all falls through to [`placeholder_then_rename`]. With
/// `inject_unlink_failure` the unlink is not attempted and an error stands in for its failure.
#[cfg(not(any(unix, windows)))]
fn link_then_unlink(
    from: &Path,
    to: &Path,
    inject_unlink_failure: bool,
) -> std::io::Result<Exclusive> {
    link_then_unlink_captured(from, to, inject_unlink_failure, None)
}

#[allow(unused_variables)]
#[cfg_attr(windows, allow(dead_code))]
fn link_then_unlink_captured(
    from: &Path,
    to: &Path,
    inject_unlink_failure: bool,
    captured: Option<(&File, bool)>,
) -> std::io::Result<Exclusive> {
    let linked = fs::hard_link(from, to);
    match linked {
        Ok(()) => {}
        Err(e) if hard_links_unsupported(&e) && !captured.is_some_and(|(_, strict)| strict) => {
            return placeholder_then_rename(from, to);
        }
        Err(e) => return Err(e),
    }
    Ok(Exclusive {
        method: ExclusiveMethod::LinkThenUnlink,
        cleanup: match captured {
            Some((file, _)) => remove_captured_temp(from, file).err(),
            None => fs::remove_file(from).err(),
        },
    })
}

/// The third fallback, for a volume with neither an exclusive rename nor hard links (FAT, exFAT,
/// some network shares): claim `to` with an `O_EXCL` placeholder (`create_new`, which fails with
/// `AlreadyExists` if the name exists, atomically), then rename the finished `from` over the
/// placeholder. Exclusive against every other exclusive publisher; but the name is visible as an
/// empty file until the rename, a crash between the two leaves that empty file, and a writer that
/// replaces without exclusivity can still overwrite the placeholder. If the rename fails the
/// placeholder this call made is removed and the error returned, so nothing is published.
#[cfg_attr(windows, allow(dead_code))]
fn placeholder_then_rename(from: &Path, to: &Path) -> std::io::Result<Exclusive> {
    let claim = OpenOptions::new().write(true).create_new(true).open(to)?;
    match fs::rename(from, to) {
        Ok(()) => Ok(Exclusive::clean(ExclusiveMethod::PlaceholderThenRename)),
        Err(e) => {
            let _ = remove_captured_temp(to, &claim);
            Err(e)
        }
    }
}

/// The temp file of an [`AtomicWriter`] as an [`std::io::Write`]; see [`AtomicWriter::writer`].
#[derive(Debug)]
pub struct TempWriter<'a> {
    file: &'a mut File,
}

impl Write for TempWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.file.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

/// A streaming atomic write. Nothing is visible at the destination until [`commit`]; dropping
/// an uncommitted writer removes its temp file only while the name matches its captured live
/// identity. A foreign replacement is retained. A process killed mid-write leaves only the
/// temp file behind.
///
/// [`commit`]: AtomicWriter::commit
#[derive(Debug)]
pub struct AtomicWriter {
    file: Option<File>,
    temp: PathBuf,
    target: PathBuf,
    options: AtomicWriteOptions,
    /// The opt-in to retry error 5 in the publish step; `None` is the 32/33-only default.
    access_denied: Option<AccessDeniedRetry>,
    // Retain a live handle so an unlinked inode cannot be recycled before cleanup.
    temp_identity: File,
    strict_new: bool,
}

impl AtomicWriter {
    pub fn create(target: &Path, options: AtomicOptions) -> Result<Self> {
        Self::create_with(target, options.into())
    }

    /// Like [`AtomicWriter::create`], with the full [`AtomicWriteOptions`].
    ///
    /// A target whose file name is shaped like a temp name this crate generates
    /// (`.<name>.rktmp-<pid>-<n>-<attempt>`) is refused with [`FsError::BadName`] by the legacy
    /// reserved-name convention. Use [`AtomicWriter::create_new_private`] for native-name imports.
    pub fn create_with(target: &Path, options: AtomicWriteOptions) -> Result<Self> {
        Self::create_inner(target, options, None, false)
    }

    /// [`AtomicWriter::create_with`] with an explicit [`AccessDeniedRetry`] opt-in: every commit
    /// of this writer retries Windows `ERROR_ACCESS_DENIED` (5) in the publish step, bounded by
    /// `retry`, which replaces `options.retry_sharing_violation` for this writer. The original
    /// error is returned after the bound. See [`AccessDeniedRetry`] for the exact semantics.
    pub fn create_with_access_denied_retry(
        target: &Path,
        options: AtomicWriteOptions,
        retry: AccessDeniedRetry,
    ) -> Result<Self> {
        Self::create_inner(target, options, Some(retry), false)
    }

    /// Stage an owner-only file for durable, exclusive publication at any native destination
    /// leaf, including a name that resembles a generated temp. Missing parents are refused.
    /// Commit never replaces an existing destination or exposes an empty placeholder: a volume
    /// without exclusive rename or hard links fails before publication. File flush failures
    /// also fail before publication (including unavailable Apple `F_FULLFSYNC`);
    /// post-publication errors retain `target_replaced()` semantics.
    /// `commit_as` may change the leaf in the same directory but must use `NoReplace`.
    pub fn create_new_private(target: &Path) -> Result<Self> {
        Self::create_inner(
            target,
            AtomicWriteOptions {
                private: true,
                create_parents: false,
                durable: true,
                replace: ReplaceMode::NoReplace,
                ..AtomicWriteOptions::default()
            },
            None,
            true,
        )
    }

    fn create_inner(
        target: &Path,
        options: AtomicWriteOptions,
        access_denied: Option<AccessDeniedRetry>,
        strict_new: bool,
    ) -> Result<Self> {
        let name = target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| FsError::BadName(target.display().to_string()))?;
        if name.contains('\0') {
            return Err(FsError::BadName(target.display().to_string()));
        }
        if !strict_new {
            reject_reserved_name(&name)?;
        }
        let parent = parent_of(target);
        if options.create_parents {
            let private_dirs = if cfg!(windows) {
                options.windows_acl()
            } else {
                options.unix_owner_only()
            };
            if options.durable {
                // Publish the link of every directory this call creates on the path before the
                // file is written below them. Directories that already exist are not touched.
                create_dir_all_durable(&parent, private_dirs)?;
            } else if private_dirs {
                crate::private::ensure_private_dir(&parent)?;
            } else {
                fs::create_dir_all(&parent).map_err(|e| FsError::io(&parent, e))?;
            }
        }
        // macOS: a parent whose ACL would hand an inheritable `allow` entry to the new temp file makes the
        // owner-only guarantee impossible; refuse before anything is created.
        #[cfg(unix)]
        if options.unix_owner_only() {
            crate::acl::guard_parent_path(&parent, target)?;
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
                    let temp_identity = file.try_clone().map_err(|e| FsError::io(&temp, e))?;
                    // A temp that carries an `allow` entry anyway is still empty (nothing is written
                    // before this check): remove it and refuse. No ACL is ever removed from an
                    // inode that may already be open elsewhere.
                    #[cfg(unix)]
                    if options.unix_owner_only() {
                        if let Err(error) = crate::acl::refuse_acl(&file, &temp) {
                            drop(file);
                            let _ = remove_captured_temp(&temp, &temp_identity);
                            return Err(error);
                        }
                    }
                    return Ok(Self {
                        file: Some(file),
                        temp,
                        target: target.to_path_buf(),
                        options,
                        access_denied,
                        temp_identity,
                        strict_new,
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
        self.writer()
            .write_all(bytes)
            .map_err(|e| FsError::io(temp, e))
    }

    /// The temp file as an [`std::io::Write`], for code that streams into a writer (an encoder, a
    /// `BufWriter`). Unlike [`AtomicWriter::file_mut`] it is a plain pass-through
    /// to the file.
    pub fn writer(&mut self) -> TempWriter<'_> {
        TempWriter {
            file: self.file_mut(),
        }
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
        self.commit_inner(false, false, |_| Ok(())).map(|_| ())
    }

    /// [`AtomicWriter::commit`] under a different final name in the same directory, for a file whose
    /// name is known only once its bytes are written (a content hash computed at the end).
    ///
    /// `new_target` must be in the writer's directory ([`FsError::BadName`] otherwise) and must not
    /// be shaped like a temp name this crate generates, except for a writer created by
    /// [`AtomicWriter::create_new_private`], which accepts native leaves but requires `NoReplace`.
    /// `replace` is used for this publish in place
    /// of the writer's [`AtomicWriteOptions::replace`]; pass [`ReplaceMode::NoReplace`] for an
    /// exclusive publish that refuses an existing name. Everything else is as `commit`: the temp
    /// file is flushed, published to `new_target`, and on a failure before the replace the temp
    /// file is removed and nothing is published.
    pub fn commit_as(mut self, new_target: &Path, replace: ReplaceMode) -> Result<()> {
        let name = new_target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| FsError::BadName(new_target.display().to_string()))?;
        if name.contains('\0') {
            return Err(FsError::BadName(new_target.display().to_string()));
        }
        if self.strict_new {
            if replace != ReplaceMode::NoReplace {
                return Err(FsError::BadName(new_target.display().to_string()));
            }
        } else {
            reject_reserved_name(&name)?;
        }
        if parent_of(new_target) != parent_of(&self.target) {
            return Err(FsError::BadName(new_target.display().to_string()));
        }
        self.target = new_target.to_path_buf();
        self.options.replace = replace;
        self.commit()
    }

    /// [`AtomicWriter::commit`], returning what was actually achieved (M8-O4): the flush level the
    /// temp file reached ([`WriteReport::sync_level`], so an Apple volume that could not
    /// `F_FULLFSYNC` is visible instead of silently weaker) and what publishing with these options
    /// guarantees on this platform ([`WriteReport::publication`]). Errors are those of `commit`.
    pub fn commit_reporting(self) -> Result<WriteReport> {
        let publication = publication_durability(&self.options, true);
        let (_, sync_level) = self.commit_inner_level(false, false, |_| Ok(()))?;
        Ok(WriteReport {
            sync_level,
            publication,
        })
    }

    /// [`AtomicWriter::commit`] with a check that runs on the synced temp file before anything is
    /// published.
    ///
    /// Order, in every mode (`durable` or not): (1) the temp file's content is written and
    /// flushed, (2) the temp file is synced (`fsync`, or `F_FULLFSYNC` with the Apple ladder when
    /// `durable`; `FlushFileBuffers` on Windows) -- the file only, never the directory, (3)
    /// `check` runs, (4) the replace publishes, (5) the directory is synced if and only if
    /// `durable`. So what `check` reads is what is on stable storage and what `Ok` publishes;
    /// a non-durable write with a check pays one file sync that a plain [`AtomicWriter::commit`]
    /// does not, and still never syncs the directory.
    ///
    /// `check` receives the temp file's path. Its `Err` aborts the write: the destination is left
    /// as it was (no replace is attempted), the temp file is removed, and the error is returned as
    /// [`FsError::Io`] carrying the temp path. `Ok` publishes exactly the content that was checked.
    /// The check must not write to the temp file; it is a read-only gate.
    pub fn commit_checked(self, check: impl FnOnce(&Path) -> std::io::Result<()>) -> Result<()> {
        self.commit_inner(false, true, check).map(|_| ())
    }

    /// [`AtomicWriter::commit`], also returning the lowercase hex SHA-256 of the published bytes.
    ///
    /// The digest is taken from the temp file after the last write (and `fsync`), immediately before
    /// the replace, so it is the digest of exactly what is published however the bytes were written
    /// (including through [`AtomicWriter::file_mut`]). That costs one sequential read of data the page
    /// cache still holds. For hashing while streaming, feed [`crate::digest::Sha256Hasher`] yourself.
    #[cfg(feature = "digest")]
    pub fn commit_with_digest(self) -> Result<String> {
        self.commit_inner(true, false, |_| Ok(()))
            .map(|d| d.expect("a digest was requested"))
    }

    fn commit_inner(
        self,
        want_digest: bool,
        checked: bool,
        check: impl FnOnce(&Path) -> std::io::Result<()>,
    ) -> Result<Option<String>> {
        self.commit_inner_level(want_digest, checked, check)
            .map(|(digest, _)| digest)
    }

    fn commit_inner_level(
        mut self,
        want_digest: bool,
        checked: bool,
        check: impl FnOnce(&Path) -> std::io::Result<()>,
    ) -> Result<(Option<String>, Option<SyncLevel>)> {
        let file = self.file.take().expect("writer is open until commit");
        let result = self.finish(file, want_digest, checked, check);
        // Unpublished: the temp is all there is to clean. Published with a later error: the
        // temp name is already gone (rename), or it is a leftover second name for the
        // published inode (link fallback). Cleanup is allowed only for the captured live identity.
        if result.is_err() {
            let _ = remove_captured_temp(&self.temp, &self.temp_identity);
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
        checked: bool,
        check: impl FnOnce(&Path) -> std::io::Result<()>,
    ) -> Result<(Option<String>, Option<SyncLevel>)> {
        let mut sync_level = None;
        if self.options.durable {
            sync_level = Some(
                durable_sync_with(
                    &file,
                    if self.strict_new {
                        SyncPolicy::Strict
                    } else {
                        SyncPolicy::Fallback
                    },
                )
                .map_err(|e| FsError::io(&self.temp, e))?,
            );
        } else if checked {
            // A caller's check is about to vouch for these bytes: they must be on stable
            // storage (the file only, never the directory) before it reads them, in every mode.
            file.sync_all().map_err(|e| FsError::io(&self.temp, e))?;
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
        let published = publish_captured(
            &self.temp,
            &self.target,
            self.options,
            self.access_denied,
            Some((&self.temp_identity, self.strict_new)),
        )?;

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
        Ok((digest, sync_level))
    }
}

/// What a write achieved, as [`AtomicWriter::commit_reporting`] and [`atomic_write_reporting`]
/// return it (M8-O4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct WriteReport {
    /// The flush level the temp file reached before publication; `None` when the write was not
    /// `durable` (nothing was flushed). Only Apple targets ever report less than
    /// [`SyncLevel::Full`].
    pub sync_level: Option<SyncLevel>,
    /// What publishing guarantees on this platform for these options
    /// ([`publication_durability`]).
    pub publication: PublicationDurability,
}

impl WriteReport {
    /// True when the write was durable and every step reached the strongest level this
    /// platform offers: the flush was [`SyncLevel::Full`] and publication is not
    /// [`PublicationDurability::FileFlushedOnly`] or [`PublicationDurability::None`].
    pub fn is_fully_durable(&self) -> bool {
        self.sync_level == Some(SyncLevel::Full)
            && matches!(
                self.publication,
                PublicationDurability::DirectoryFlushed | PublicationDurability::WriteThroughMove
            )
    }
}

/// [`atomic_write_with_options`] that returns the [`WriteReport`] of what the write achieved.
pub fn atomic_write_reporting(
    path: &Path,
    bytes: &[u8],
    options: AtomicWriteOptions,
) -> Result<WriteReport> {
    let mut writer = AtomicWriter::create_with(path, options)?;
    writer.write_all(bytes)?;
    writer.commit_reporting()
}

fn require_captured_temp(path: &Path, file: &File) -> std::io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(std::io::Error::other(
            "staging name no longer identifies the owned file",
        ));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return Err(std::io::Error::other("staging name became a reparse point"));
        }
    }
    let expected = crate::FileKey::of_file(file, path).map_err(std::io::Error::other)?;
    let observed = crate::FileKey::of_path(path).map_err(std::io::Error::other)?;
    if expected != observed {
        return Err(std::io::Error::other("staging file identity changed"));
    }
    Ok(())
}

fn remove_captured_temp(path: &Path, file: &File) -> std::io::Result<()> {
    require_captured_temp(path, file)?;
    fs::remove_file(path)
}

impl Drop for AtomicWriter {
    fn drop(&mut self) {
        if !self.temp.as_os_str().is_empty() {
            self.file.take();
            let _ = remove_captured_temp(&self.temp, &self.temp_identity);
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
    crate::private::create_new_owner_only(temp)
}

fn parent_of(path: &Path) -> PathBuf {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// Most bytes of the destination name a temp name carries. The carried part is diagnostic only
/// (uniqueness and sweep recognition live in the suffix), so it is bounded rather than letting a
/// destination near the filesystem's component limit push the temp name past it (`ENAMETOOLONG`
/// for a write the destination itself allows). The bound is an internal constant, not a claim
/// about any filesystem's limit (D14: limits are filesystem-dependent).
const TEMP_PREFIX_MAX_BYTES: usize = 64;

/// `name` bounded to [`TEMP_PREFIX_MAX_BYTES`], cut only on a UTF-8 char boundary and never
/// empty (a generated name needs at least one char before the marker to parse back).
fn temp_prefix(name: &str) -> &str {
    if name.len() <= TEMP_PREFIX_MAX_BYTES {
        return name;
    }
    let mut end = TEMP_PREFIX_MAX_BYTES;
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    if end == 0 {
        // The first char alone exceeds the bound (impossible for valid UTF-8 chars, which are
        // at most 4 bytes): keep that one char.
        let first = name.chars().next().map_or(0, char::len_utf8);
        return &name[..first];
    }
    &name[..end]
}

fn temp_name(name: &str, attempt: u32) -> String {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    // `<pid>` is parsed back by `sweep_stale_temps` to tell a crashed writer from a live one.
    format!(
        ".{}{TEMP_MARKER}{}-{n}-{nanos:x}-{attempt}",
        temp_prefix(name),
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
/// be indistinguishable from an orphan. Generic sweeps retain both; legacy writers reserve it.
///
/// On Windows the check also runs on the name the filesystem will store: Win32 drops leading
/// spaces, trailing spaces and trailing periods from the last path component (and `name:stream`
/// addresses a stream of `name`), so `".record.rktmp-123-1-0 "`, `" .record.rktmp-123-1-0"` and
/// `".record.rktmp-123-1-0:s"` are all created as the reserved `".record.rktmp-123-1-0"`. Every
/// name check in the crate funnels through here, so no creation API sees only the raw text. A
/// name whose stored form is *not* reserved is accepted as it is (this is not a refusal of every
/// name Windows would normalise).
pub(crate) fn reject_reserved_name(name: &str) -> Result<()> {
    reject_reserved_name_on(cfg!(windows), name)
}

/// [`reject_reserved_name`] for an explicit platform, so the Windows rule is testable everywhere.
fn reject_reserved_name_on(windows: bool, name: &str) -> Result<()> {
    let reserved = generated_temp_pid(name).is_some()
        || (windows && generated_temp_pid(&windows_stored_name(name)).is_some());
    if reserved {
        Err(FsError::BadName(name.to_string()))
    } else {
        Ok(())
    }
}

/// The name Windows stores for a last path component written as `name`: the part before any
/// `:stream` suffix, without leading ASCII spaces and without trailing spaces and periods.
///
/// Leading-space removal is what the fifth Astra read reports from Microsoft's file-name
/// whitespace documentation; it has not been observed on a Windows host from this tree. Treating a
/// name as normalised when Windows would in fact keep it only refuses a name that was safe, which
/// is the harmless direction.
fn windows_stored_name(name: &str) -> String {
    let base = name.split(':').next().unwrap_or(name);
    base.trim_start_matches(' ')
        .trim_end_matches([' ', '.'])
        .to_string()
}

/// Best-effort directory `fsync` so a rename itself survives power loss (no-op off unix).
/// Errors are discarded; use [`AtomicWriter::commit`] when the caller must know.
pub fn sync_dir(dir: &Path) {
    let _ = try_sync_dir(dir);
}

/// Directory `fsync`, reporting failure. A no-op off unix: Windows has no directory flush (a
/// directory handle opened for listing cannot be flushed), so the strongest publication there is
/// a write-through move; see [`publication_durability`]. The fallible form of [`sync_dir`]: a
/// caller that owes a directory entry durability checks the result instead of discarding it.
pub fn try_sync_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        // `O_DIRECTORY`: a path that is a regular file is refused without being opened, so a
        // wrong argument cannot release a record lock this process holds on that file.
        let d = {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
                .open(dir)?
        };
        // Not `durable_sync`: that would also count as a file sync in `seams`.
        durable_sync_with_impl(&d, SyncPolicy::Fallback).map(|_| ())
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

/// Creates `dir` and its missing ancestors one component at a time, top down, and makes the link
/// of every component **this call created** durable by syncing its parent before going on (Unix).
/// A component counts as created only when the create returned `Ok`; one that already existed, or
/// that a racing creator made first (`AlreadyExists`), is neither synced nor looked at again, and
/// neither is a component that is a symlink. There is no record of earlier syncs and no walk over
/// pre-existing components, so when every directory already exists this makes no sync at all (the
/// destination directory's own sync after a published file is the write's, not this function's).
///
/// A directory created by an earlier call whose parent sync did not happen is not synced by
/// later calls; the residual is spelled out in the module docs, section 3.
///
/// New directories are `0700` when `owner_only`, and on macOS are refused
/// ([`FsError::UnsafeAcl`]) under a parent whose ACL has an inheritable `allow` entry. On Windows
/// each directory this call creates, and its parent, is flushed (`FlushFileBuffers` on a
/// `GENERIC_WRITE` handle) before the next component. Elsewhere there is no directory flush, so
/// only the ordinary creation runs.
///
/// Unlike [`AtomicWriter::create_with`] this makes no file; it is the directory half of a durable
/// `create_parents` write, for callers (a lock file, a settings file) that create a file some
/// other way below a directory this has made durable.
pub fn create_dir_all_durable(dir: &Path, owner_only: bool) -> Result<()> {
    create_dirs_durable(dir, owner_only)?;
    if owner_only {
        crate::private::ensure_private_dir(dir)
    } else {
        fs::create_dir_all(dir).map_err(|e| FsError::io(dir, e))
    }
}

#[cfg(unix)]
fn create_dirs_durable(dir: &Path, owner_only: bool) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    if dir.as_os_str().is_empty() {
        return Ok(());
    }
    let mut chain: Vec<&Path> = dir
        .ancestors()
        .filter(|p| !p.as_os_str().is_empty())
        .collect();
    chain.reverse();
    for path in chain {
        match fs::symlink_metadata(path) {
            // Already there (a directory, or a link): not this call's to publish.
            Ok(_) => continue,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(FsError::io(path, e)),
        }
        if owner_only {
            crate::acl::guard_parent_path(&parent_of(path), path)?;
        }
        let mut builder = fs::DirBuilder::new();
        if owner_only {
            builder.mode(0o700);
        }
        match builder.create(path) {
            Ok(()) => {}
            // A racing creator made it first: it is theirs to publish.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(FsError::io(path, e)),
        }
        if owner_only {
            // A directory that carries an allow ACL anyway is empty: remove it and refuse. It is
            // never repaired.
            if let Err(error) = crate::acl::refuse_acl_path(path) {
                let _ = fs::remove_dir(path);
                return Err(error);
            }
        }
        // This call created `path`: make its link in the parent durable before the next
        // component, and so before anything is written below it.
        sync_parent_dir(path).map_err(|source| FsError::DirSyncFailed {
            path: path.to_path_buf(),
            source,
        })?;
    }
    Ok(())
}

#[cfg(windows)]
fn create_dirs_durable(dir: &Path, owner_only: bool) -> Result<()> {
    if dir.as_os_str().is_empty() {
        return Ok(());
    }
    let mut chain: Vec<&Path> = dir
        .ancestors()
        .filter(|p| !p.as_os_str().is_empty())
        .collect();
    chain.reverse();
    // Whether the previous component was created by this call, so its parent was flushed already.
    let mut parent_created = false;
    for path in chain {
        match fs::symlink_metadata(path) {
            // Already there (a directory, or a link): not this call's to flush.
            Ok(_) => {
                parent_created = false;
                continue;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(FsError::io(path, e)),
        }
        if owner_only {
            // Every ancestor exists by now, so this creates exactly `path`, with its owner-only
            // descriptor from creation.
            crate::private::ensure_private_dir(path)?;
        } else {
            match fs::create_dir(path) {
                Ok(()) => {}
                // A racing creator made it first: it is theirs to flush.
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    parent_created = false;
                    continue;
                }
                Err(e) => return Err(FsError::io(path, e)),
            }
        }
        // This call created `path`: flush the directory itself, then its parent (which holds the
        // link), before the next component and so before anything is written below it.
        let failed = |source| FsError::DirSyncFailed {
            path: path.to_path_buf(),
            source,
        };
        flush_dir_windows(path).map_err(failed)?;
        match flush_dir_windows(&parent_of(path)) {
            Ok(()) => {}
            // A parent this call did not create may deny `GENERIC_WRITE` to a caller that may only
            // add children to it (a profile root, a drive root); the directory flush is then
            // unavailable there, and the refusal is not an error of this write.
            Err(e) if !parent_created && e.raw_os_error() == Some(5) => {}
            Err(e) => return Err(failed(e)),
        }
        parent_created = true;
    }
    Ok(())
}

/// `FlushFileBuffers` on a write handle of the directory `dir`. A handle opened for listing cannot
/// be flushed (`ERROR_ACCESS_DENIED`), so this opens `GENERIC_WRITE` with
/// `FILE_FLAG_BACKUP_SEMANTICS`, which is what opens a directory at all, and shares read, write
/// and delete so it neither blocks nor is blocked by another holder.
#[cfg(windows)]
pub(crate) fn flush_dir_windows(dir: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateFileW(
            name: *const u16,
            access: u32,
            share: u32,
            security: *const std::ffi::c_void,
            disposition: u32,
            flags: u32,
            template: *mut std::ffi::c_void,
        ) -> *mut std::ffi::c_void;
        fn FlushFileBuffers(file: *mut std::ffi::c_void) -> i32;
    }
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const FILE_SHARE_READ_WRITE_DELETE: u32 = 0x7;
    const OPEN_EXISTING: u32 = 3;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(Some(0)).collect();
    if wide[..wide.len() - 1].contains(&0) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path contains a NUL",
        ));
    }
    // SAFETY: `wide` is NUL-terminated and outlives the call; no security attribute is passed.
    let raw = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_WRITE,
            FILE_SHARE_READ_WRITE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if raw as isize == -1 || raw.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: CreateFileW returned a fresh owned handle; `File` closes it exactly once.
    let handle = unsafe { File::from_raw_handle(raw) };
    // SAFETY: the handle is live for the call.
    if unsafe { FlushFileBuffers(handle.as_raw_handle()) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn create_dirs_durable(_dir: &Path, _owner_only: bool) -> Result<()> {
    Ok(())
}

/// What happened after a replace call succeeded: the failure, if any, of removing a leftover
/// second name for the published file (the `link` fallback). The content is in place either way.
struct Published {
    cleanup: Option<std::io::Error>,
}

/// Replaces `target` with `temp` under the requested mode, retrying sharing violations when the
/// options ask for it. `Err` means nothing was published; `Ok` means the new content is in place
/// and carries the failure of any step after that.
///
/// A private write needs no step on the destination: every publish moves the temp file, so its
/// owner-only mode or DACL is the published file's from the instant it has its name.
fn publish(
    temp: &Path,
    target: &Path,
    options: AtomicWriteOptions,
    access_denied: Option<AccessDeniedRetry>,
) -> Result<Published> {
    publish_captured(temp, target, options, access_denied, None)
}

fn publish_captured(
    temp: &Path,
    target: &Path,
    options: AtomicWriteOptions,
    access_denied: Option<AccessDeniedRetry>,
    captured: Option<(&File, bool)>,
) -> Result<Published> {
    // Opted in: 5, 32 and 33 share the one bound the caller gave. Default: 32/33 only.
    let (policy, classify): (Option<RetryPolicy>, fn(&std::io::Error) -> bool) = match access_denied
    {
        Some(retry) => (Some(retry.policy()), is_sharing_or_access_denied),
        None => (options.retry_sharing_violation, is_sharing_violation),
    };
    let cleanup = retry_with(policy, classify, || match captured {
        Some(_) => replace_step_captured(temp, target, options, captured),
        None => replace_step(temp, target, options),
    })
    .map_err(|e| {
        if options.replace == ReplaceMode::NoReplace
            && e.kind() == std::io::ErrorKind::AlreadyExists
        {
            return FsError::AlreadyExists(target.to_path_buf());
        }
        FsError::io(target, e)
    })?;
    Ok(Published { cleanup })
}

/// Where in an atomic write an owner-only security step runs. Only Windows takes these steps; Unix
/// gets its mode bits at creation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HardenStage {
    /// The temp file is created with its owner-only security descriptor (no later restriction).
    TempCreate,
}

/// One owner-only step about to run.
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

/// `ERROR_SHARING_VIOLATION` (32) or `ERROR_LOCK_VIOLATION` (33) on Windows. Never true
/// elsewhere. `ERROR_ACCESS_DENIED` (5) is deliberately not counted here: it is also what a real
/// permission failure returns, so this default classifier never retries it, including when its
/// cause is a reader holding the destination without delete sharing. A caller that wants that case
/// retried opts in with `AccessDeniedRetry`, which switches the publish step to
/// `is_sharing_or_access_denied`.
fn is_sharing_violation(error: &std::io::Error) -> bool {
    transient_publish_error(cfg!(windows), error.raw_os_error(), false)
}

/// `is_sharing_violation` plus `ERROR_ACCESS_DENIED` (5) on Windows. Used only when the caller
/// opted in with `AccessDeniedRetry`; the retry bound is the caller's, and the final error is
/// returned unchanged, so a true ACL denial still fails.
fn is_sharing_or_access_denied(error: &std::io::Error) -> bool {
    transient_publish_error(cfg!(windows), error.raw_os_error(), true)
}

/// The classifier behind both, with the platform explicit so it is testable everywhere: on
/// Windows 32 and 33 are transient, and 5 only when `include_access_denied`; on any other
/// platform nothing is (the codes are Windows codes).
fn transient_publish_error(windows: bool, raw: Option<i32>, include_access_denied: bool) -> bool {
    windows
        && match raw {
            Some(32) | Some(33) => true,
            Some(5) => include_access_denied,
            _ => false,
        }
}

/// The platform call a replace uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplaceApi {
    /// `std::fs::rename` (Unix: `rename(2)`).
    Rename,
    /// Windows `MoveFileExW`; `flags` are its `dwFlags`.
    MoveFileEx,
    /// The exclusive rename of [`ReplaceMode::NoReplace`] on Unix (`flags` 0).
    RenameExclusive,
}

/// `MOVEFILE_REPLACE_EXISTING`.
pub const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
/// `MOVEFILE_WRITE_THROUGH`.
pub const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

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
/// Windows supports it for the mode: `MoveFileExW` with `MOVEFILE_WRITE_THROUGH`. `Rename` never
/// uses `MoveFileExW`: `std::fs::rename` replaces with POSIX semantics on Windows, which a
/// concurrent reader's handle opened with delete sharing cannot block (measured: `MoveFileExW`
/// made writers fail with `ERROR_ACCESS_DENIED` against such readers in `tests/e2e.rs`). A
/// reader that denies delete sharing blocks either call with error 5; see [`ReplaceMode::Rename`].
fn plan_replace(options: AtomicWriteOptions) -> (ReplaceApi, u32) {
    plan_replace_on(cfg!(windows), options)
}

/// [`plan_replace`] for an explicit platform, so the Windows plan is testable everywhere.
fn plan_replace_on(windows: bool, options: AtomicWriteOptions) -> (ReplaceApi, u32) {
    match (options.replace, windows) {
        (ReplaceMode::NoReplace, true) => (
            ReplaceApi::MoveFileEx,
            if options.durable {
                MOVEFILE_WRITE_THROUGH
            } else {
                0
            },
        ),
        (ReplaceMode::NoReplace, false) => (ReplaceApi::RenameExclusive, 0),
        (ReplaceMode::MoveWriteThrough, true) => (
            ReplaceApi::MoveFileEx,
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        ),
        (ReplaceMode::Rename | ReplaceMode::MoveWriteThrough, _) => (ReplaceApi::Rename, 0),
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
    /// the typed "durable publication is not available" answer: Windows `Rename`.
    FileFlushedOnly,
    /// The temp file is flushed, the replace runs, and the parent directory is flushed (Unix).
    DirectoryFlushed,
    /// The temp file is flushed and the replace is `MoveFileExW` with `MOVEFILE_WRITE_THROUGH`
    /// (Windows `MoveWriteThrough` and `NoReplace`). Microsoft documents the move as complete on
    /// disk when the call returns; this crate has not run a power-loss test of it.
    WriteThroughMove,
}

/// What publishing with `options` guarantees on this platform when the call returns `Ok`.
/// `destination_exists` is accepted for compatibility with callers written when a mode existed
/// whose answer depended on it; no mode does now, and it is ignored. Callers that must not accept
/// [`PublicationDurability::FileFlushedOnly`] can check this before writing.
pub fn publication_durability(
    options: &AtomicWriteOptions,
    destination_exists: bool,
) -> PublicationDurability {
    let _ = destination_exists;
    publication_durability_on(cfg!(windows), options)
}

fn publication_durability_on(windows: bool, options: &AtomicWriteOptions) -> PublicationDurability {
    if !options.durable {
        return PublicationDurability::None;
    }
    if !windows {
        return PublicationDurability::DirectoryFlushed;
    }
    match plan_replace_on(true, *options) {
        (ReplaceApi::MoveFileEx, flags) if flags & MOVEFILE_WRITE_THROUGH != 0 => {
            PublicationDurability::WriteThroughMove
        }
        _ => PublicationDurability::FileFlushedOnly,
    }
}

/// One replace call. `Ok(None)`: published cleanly. `Ok(Some(error))`: published, but removing a
/// leftover second name failed (the `link` fallback). `Err`: nothing was published.
fn replace_step(
    temp: &Path,
    target: &Path,
    options: AtomicWriteOptions,
) -> std::io::Result<Option<std::io::Error>> {
    replace_step_captured(temp, target, options, None)
}

fn replace_step_captured(
    temp: &Path,
    target: &Path,
    options: AtomicWriteOptions,
    captured: Option<(&File, bool)>,
) -> std::io::Result<Option<std::io::Error>> {
    let (api, flags) = plan_replace(options);
    if let Some((file, _)) = captured {
        require_captured_temp(temp, file)?;
    }
    match api {
        ReplaceApi::Rename => fs::rename(temp, target).map(|()| None),
        ReplaceApi::RenameExclusive => {
            #[cfg(unix)]
            let done = rename_exclusive_io_captured(temp, target, captured);
            #[cfg(not(unix))]
            let done = rename_exclusive_io(temp, target);
            done.map(|done| done.cleanup)
        }
        ReplaceApi::MoveFileEx => move_file_ex(temp, target, flags).map(|()| None),
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
#[cfg_attr(not(target_vendor = "apple"), allow(dead_code))]
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
    durable_sync_with_impl(file, policy)
}

/// [`durable_sync_with`] without the file-sync count (directory syncs use it, and count themselves
/// as directory syncs).
fn durable_sync_with_impl(file: &File, policy: SyncPolicy) -> std::io::Result<SyncLevel> {
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

/// Inspect temporary-looking names without removing them. Neither generated-name syntax nor a
/// dead PID proves that an entry belongs to this crate: it may be an imported committed record.
/// All ambiguous entries are retained. `max_age` is accepted for compatibility and ignored.
/// Returns zero; use [`sweep_stale_temps_report`] to observe retained candidates.
pub fn sweep_stale_temps(dir: &Path, max_age: Duration) -> Result<usize> {
    let _ = max_age;
    sweep_stale_temps_report(dir).map(|report| report.removed)
}

/// What [`sweep_stale_temps_report`] found (M8-O7b): the removal count of [`sweep_stale_temps`] plus
/// why each generated temp that was kept survived, so a caller can tell a writer confirmed dead
/// from one whose liveness could not be established.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct SweepReport {
    /// Files removed. Currently zero: generic discovery proves no ownership.
    pub removed: usize,
    /// Kept because the writer is this process or confirmed alive.
    pub retained_live: usize,
    /// Kept because the writer's liveness could not be established (query failed, or the
    /// platform has no probe). Never treated as dead.
    pub retained_unknown: usize,
    /// Kept although the PID is dead: name syntax and death prove no ownership.
    pub retained_other: usize,
    /// Whether retained counts were measured. True on Unix and Windows, including empty reports.
    pub retained_counted: bool,
}

/// [`sweep_stale_temps`] with retained counts classified by PID liveness. Every matching name
/// is retained, including regular files whose PID is confirmed dead.
pub fn sweep_stale_temps_report(dir: &Path) -> Result<SweepReport> {
    #[cfg(windows)]
    {
        crate::windows_temp_sweep::sweep_path_report(dir)
    }
    #[cfg(not(windows))]
    {
        sweep_stale_temps_path(dir)
    }
}

#[cfg(not(windows))]
fn sweep_stale_temps_path(dir: &Path) -> Result<SweepReport> {
    let mut report = SweepReport {
        retained_counted: true,
        ..SweepReport::default()
    };
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(report),
        Err(e) => return Err(FsError::io(dir, e)),
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(pid) = generated_temp_pid(&name) else {
            continue;
        };
        if pid == std::process::id() {
            report.retained_live += 1;
            continue;
        }
        match crate::process_liveness(pid) {
            crate::ProcessLiveness::Alive => {
                report.retained_live += 1;
                continue;
            }
            crate::ProcessLiveness::Unknown => {
                report.retained_unknown += 1;
                continue;
            }
            crate::ProcessLiveness::Dead => {}
        }
        // Syntax and PID death cannot distinguish a crash orphan from an imported record.
        report.retained_other += 1;
    }
    Ok(report)
}

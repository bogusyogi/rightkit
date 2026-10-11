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
//! [`sweep_stale_temps`] reclaims. "Test" names a test in `tests/` (or a unit test in this file);
//! "seam" means the row cannot be produced without fault injection and is covered by the named
//! `atomic::testing` hook, which stands in for the OS call or state.
//!
//! ## 1. Creating the temp (step 1), per platform x privacy x destination
//!
//! | Platform | Privacy | Destination | Temp is created with | Notes | Test |
//! |---|---|---|---|---|---|
//! | Unix | `Inherit` | absent, or not a regular file (a symlink is never followed) | `open(O_CREAT\|O_EXCL\|O_NOFOLLOW\|O_CLOEXEC, 0666 & ~umask)` | | `atomic_privacy.rs::inherit_is_unchanged_and_private_true_wins_over_privacy` |
//! | Unix | `Inherit` | regular file | the same `open`, mode = the file's mode & ~umask, then `fchmod` to its exact bits | never wider than the file it replaces | `e2e.rs::private_writes_are_owner_only_and_plain_writes_keep_the_existing_mode` |
//! | Unix | `Inherit` | symlink | as for absent: the link's target lends no mode | the rename replaces the link itself | `atomic_privacy.rs::a_symlink_at_the_destination_is_replaced_not_followed_and_lends_no_mode` |
//! | Unix | `OwnerOnly`, `OwnerOnlyUnixInheritWindows`, `private: true` | any | `open(... , 0600)` (final bits at creation, no later `chmod`); parents `0700` | macOS: the parent's ACL is read first, and an inheritable `allow` entry refuses the write ([`FsError::UnsafeAcl`]) before anything exists; an ACL the temp got anyway is removed and read back (see `acl`) | `atomic_publish.rs::the_temp_is_owner_only_before_the_replace_publishes_it`; macOS ACLs: `acl_macos.rs::an_inheriting_parent_refuses_a_private_atomic_write_and_creates_nothing` |
//! | Windows | `Inherit`, `OwnerOnlyUnixInheritWindows` | any | `OpenOptions::create_new`: the parent's inherited ACL | | `atomic_publish.rs::windows::inherit_privacy_takes_no_owner_only_step`, `atomic_dacl_windows.rs::privacy_variants_on_windows` |
//! | Windows | `OwnerOnly`, `private: true` | any | `CreateFileW(CREATE_NEW)` with the descriptor `D:P(A;;FA;;;<user>)` in `SECURITY_ATTRIBUTES` (never created then restricted); fails closed, nothing created, if the descriptor cannot be built | | `atomic_publish.rs::windows::the_temp_carries_its_owner_only_descriptor_from_creation_and_is_never_restricted_later`, `a_failed_owner_only_creation_publishes_nothing_and_leaves_no_temp` |
//! | all | any | a target named like a generated temp name, including on Windows one that *becomes* such a name after Win32 drops trailing spaces and periods or a `:stream` suffix | refused before anything is created ([`FsError::BadName`]) | | `atomic_namespace.rs::a_public_write_cannot_target_a_generated_temp_name`, `windows_trailing_space_and_period_names_are_refused`; unit `temp_name_tests::windows_names_that_normalize_to_a_temp_name_are_reserved` |
//!
//! ## 2. Publishing (step 5) and what a crash leaves, per platform x mode x durable x destination
//!
//! Privacy does not change the publish call, except the one Windows row marked (P). On Unix the
//! three replacing modes are the same `rename(2)`.
//!
//! | # | Platform | Mode | durable | Destination | Flushed, and when | OS call that publishes | A crash after each step leaves | Error | Test |
//! |---|---|---|---|---|---|---|---|---|---|
//! | U1 | Unix | `Rename`, `ReplaceFile`, `MoveWriteThrough` | yes | exists or not | temp (step 3) before the call; dir (step 6) after | `rename(2)` | before the call: destination as it was, orphan temp. After the call, before the dir sync: old or new content after reboot, never a mix (the temp's data was flushed first). After the dir sync: new | replace failed: `Io`, temp removed, destination untouched. Dir sync failed: `ReplacedButDirSyncFailed` (`target_replaced()`) | `atomic_publish.rs::a_durable_write_flushes_the_temp_then_replaces_then_syncs_the_directory`, `a_failed_replace_syncs_no_directory_and_publishes_nothing`; `atomic_options.rs::directory_sync_failure_after_replace_is_typed_and_keeps_new_content` |
//! | U2 | Unix | same | no | exists or not | nothing (with `commit_checked`: the temp only, never the dir) | `rename(2)` | old or new name, but the new file's *data* may be lost or short after power loss: that is what `durable: false` means | as U1, no dir sync | `atomic_publish.rs::a_non_durable_write_flushes_and_syncs_nothing`, `atomic_options.rs::directory_sync_is_skipped_when_not_durable`, `atomic_checked_sync.rs::a_non_durable_checked_write_syncs_the_file_before_the_check_and_never_the_directory` |
//! | U3 | Unix | `NoReplace` | yes | absent | temp before; dir after | `renameat2(RENAME_NOREPLACE)` (Linux), `renamex_np(RENAME_EXCL)` (Apple) | as U1 | as U1 | `atomic_publish.rs::no_replace_flushes_publishes_exclusively_then_syncs_the_directory`, `atomic_exclusive.rs::publishes_when_absent_and_refuses_when_present_without_touching_the_old_file` |
//! | U4 | Unix | `NoReplace` | any | exists, or appears first | temp (durable) | the same call, which fails `EEXIST` | destination untouched, temp removed, no dir sync | `AlreadyExists` | `atomic_publish.rs::a_no_replace_loser_syncs_no_directory_and_leaves_the_winner`, `atomic_exclusive.rs::racing_publishers_have_exactly_one_winner_and_the_losers_leave_no_temp` |
//! | U5 | Unix | `NoReplace`, kernel/filesystem without the exclusive rename | any | absent | as U3 | `link(2)` then `unlink(2)` of the temp name | between the two calls both names are one inode with the full content (published); a crash leaves the temp name for the sweep | `link` fails `EEXIST`: `AlreadyExists`. `unlink` fails: dir sync still runs, then `ReplacedButPostStepFailed(TempCleanup)` | `atomic_publish.rs::post_publication::the_link_fallback_publishes_cleanly_when_the_unlink_works`, `a_failed_unlink_after_the_link_is_a_typed_error_and_the_directory_is_still_synced`; the real ENOSYS/EINVAL fallback needs an old kernel: seam `force_link_fallback` |
//! | W1 | Windows | `Rename` | yes | exists or not | temp (`FlushFileBuffers`) before the call; no dir sync exists | `std::fs::rename` (POSIX-semantics rename; not write-through) | before: destination as it was, orphan temp. After: old or new, never a mix, but the *swap itself* may be lost in a power failure (`FileFlushedOnly`) | `Io`, temp removed | `atomic_publish.rs::a_durable_write_on_windows_flushes_the_temp_and_claims_no_directory_sync`; plan: unit `plan_tests::publication_durability_states_what_each_windows_mode_gives` |
//! | W2 | Windows | `Rename` | no | exists or not | nothing | `std::fs::rename` | as W1, without the temp flush | `Io` | `atomic_publish.rs::a_non_durable_write_flushes_and_syncs_nothing` |
//! | W3 | Windows | `ReplaceFile` | yes / no | absent | temp flushed (durable) | `MoveFileExW(MOVEFILE_REPLACE_EXISTING [\| MOVEFILE_WRITE_THROUGH when durable])` | destination absent, or complete new content (write-through: committed when the call returns) | `Io`, temp removed | `atomic_testing.rs::the_flags_each_mode_hands_the_os`; unit `plan_tests::windows_write_through_is_only_ever_movefileex` |
//! | W4 | Windows | `ReplaceFile`, no Windows ACL step wanted | yes / no | exists | temp flushed (durable); `ReplaceFileW` has no supported write-through, flag word 0 | `ReplaceFileW(.., backup = ".<name>.rktbak-<pid>-<n>")`; on success the backup is deleted | not atomic across a crash in the middle of the call (Microsoft documents only what the 1176 and 1177 failures leave): the destination is intact, or absent with the old content at the backup name and the new content still at the temp name, or new with the old at the backup. All three are recoverable from the names; the temp, backup and a recovery copy are never swept | success: `Ok`, a backup that cannot be deleted is `ReplacedButPostStepFailed(TempCleanup)`. 1176/1177 with the destination missing or a backup beside it: `ReplaceInterrupted` (below). Any other failure with the destination intact: `Io`, temp removed | `atomic.rs` unit `partial_replace_tests::*`; `atomic_publish.rs::a_replace_that_lost_the_destination_keeps_the_new_content_where_a_sweep_cannot_reach_it`, `error_1176_with_the_destination_intact_is_an_ordinary_failure_and_cleans_up`. A real crash inside `ReplaceFileW` cannot be tested without power-loss injection; the seam `on_replace` stands in for the OS state it leaves |
//! | W5 (P) | Windows | `ReplaceFile`, `OwnerOnly` / `private: true` | yes / no | exists | temp flushed (durable) | `MoveFileExW(MOVEFILE_REPLACE_EXISTING [\| MOVEFILE_WRITE_THROUGH])`, not `ReplaceFileW`, which would keep the destination's DACL | destination old or new (write-through when durable); no backup, no interrupted state | `Io`; a reader holding the old file without share-delete makes the move fail (retry: [`RetryPolicy`]) | unit `plan_tests::a_private_windows_replace_file_over_an_existing_destination_is_a_move`; `atomic_publish.rs::windows::a_private_replace_file_write_over_a_broad_destination_is_a_move_and_ends_owner_only` |
//! | W6 | Windows | `MoveWriteThrough` | yes / no | exists or not | temp flushed (durable) | `MoveFileExW(MOVEFILE_REPLACE_EXISTING \| MOVEFILE_WRITE_THROUGH)` (also when not `durable`) | destination old or new; committed when the call returns | `Io`; sharing/lock violations retried per [`RetryPolicy`] | `atomic_testing.rs::the_flags_each_mode_hands_the_os`, unit `retry_tests::*`; no power-loss test has been run (the guarantee is Microsoft's) |
//! | W7 | Windows | `NoReplace` | yes / no | absent | temp flushed (durable) | `MoveFileExW(0 [\| MOVEFILE_WRITE_THROUGH when durable])`: fails if the name exists | as W6 | exists or appears first: `AlreadyExists`, temp removed | `atomic_exclusive.rs::publishes_when_absent_and_refuses_when_present_without_touching_the_old_file` |
//!
//! ### Interrupted replace (W4) and its recovery files
//!
//! When `ReplaceFileW` fails with 1176 or 1177 and the destination is missing or a backup sits
//! beside it, the new content may exist only in the temp file. The temp is **moved at once to
//! `.<name>.rktnew-<pid>-<n>-<nanos in hex>`** (exclusive: it never replaces a file), a name that
//! no sweep matches, and [`FsError::ReplaceInterrupted`] names it as `new_content`; the old
//! content is `previous_content` (the `.rktbak-` backup, or the destination). Neither file is
//! ever swept, by [`sweep_stale_temps`] or `StateDir::sweep_temps`, whether the writer is alive,
//! dead or restarted, because protection is in the *file name* on disk, not in process memory
//! (test: `atomic_publish.rs::a_replace_that_lost_the_destination_keeps_the_new_content_where_a_sweep_cannot_reach_it`,
//! which sweeps with a zero age). Deciding which version wins and deleting the loser is the
//! caller's. If the move itself fails the temp stays under its sweepable name and the error's
//! message says so (seam `fail_recovery_move`; test `an_interrupted_replace_that_cannot_be_protected_says_so`).
//!
//! ## 3. Directories made for `create_parents`
//!
//! The same table, for directories, is in the `state` module docs (`StateDir::create`, `child`).
//! For [`create_dir_all_durable`], which a durable `create_parents` write calls:
//!
//! | Platform | Component | Action | A crash leaves | Error | Test |
//! |---|---|---|---|---|---|
//! | Unix | missing | `mkdir` (`0700` when private), then `fsync` of its parent before the next component | before that sync: possibly no directory after reboot; nothing was written below it yet | `DirSyncFailed`; the retry syncs again | `atomic_publish.rs::parents::every_created_ancestor_is_published_before_the_file_is_written`, `a_failed_ancestor_sync_stops_the_write_and_the_retry_completes_it` |
//! | Unix | exists, owned by the caller, not yet published by this process (made by another process, a crashed run or a racing thread) | `fsync` of its parent, once per process, then recorded | its link may have been lost before this sync; nothing was written below it yet | `DirSyncFailed` | `atomic_publish.rs::parents::a_directory_made_by_another_process_is_published_before_the_file_is_written` (a real `mkdir` child process) |
//! | Unix | exists, already published by this process | none | n/a | none | `atomic_publish.rs::parents::a_published_directory_is_not_synced_again` |
//! | Unix | exists, owned by someone else | none (not this user's link) | n/a | none | unit `published_tests::a_foreign_owned_directory_is_never_owed` |
//! | Unix, macOS | missing under a parent with an inheritable `allow` ACL entry (private only) | refused before `mkdir`; a directory that got an ACL anyway has it removed or is removed | nothing created | [`FsError::UnsafeAcl`] | `acl_macos.rs::an_inheriting_parent_refuses_private_parents` |
//! | Unix | `durable: false` | `create_dir_all` (or `ensure_private_dir`), no sync | the directories may be lost with the file, as the mode says | none | `atomic_publish.rs::parents::a_non_durable_write_makes_parents_without_syncing` |
//! | Windows | any | `std::fs::create_dir_all` (or `ensure_private_dir`): **not** write-through, no flush exists. A later write-through move (W3, W5, W6, W7) is a journal commit that follows the directory creation in the NTFS log, which orders them; this crate has not run a power-loss test of that ordering, and for W1 and W4 nothing is committed after the creation. Directories made by `StateDir::create` do use write-through handles | not durable on Windows | none | cannot be tested without power-loss injection |
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
    /// With `durable: false` no flush is done on any platform, except that [`AtomicWriter::commit_checked`] still file-syncs the temp file before its check. [`publication_durability`] reports
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
    /// destination is not intact, the result is [`FsError::ReplaceInterrupted`]: the backup is left
    /// in place and the temp, which may hold the only copy of the new content, is moved to a
    /// `.<name>.rktnew-...` recovery name that no sweep reclaims.
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
                // Publish the link of every directory on the path this process has not yet
                // published (new ones, and ones another process or a crashed run made) before
                // the file is written below them.
                create_dir_all_durable(&parent, private_dirs)?;
            } else if private_dirs {
                crate::private::ensure_private_dir(&parent)?;
            } else {
                fs::create_dir_all(&parent).map_err(|e| FsError::io(&parent, e))?;
            }
        }
        // macOS: a parent whose ACL would hand an `allow` entry to the new temp file makes the
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
                    #[cfg(unix)]
                    if options.unix_owner_only() {
                        if let Err(error) = crate::acl::scrub_created(&file, &temp) {
                            drop(file);
                            let _ = fs::remove_file(&temp);
                            return Err(error);
                        }
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
    /// Until the replace succeeds nothing is published: any error is an ordinary one, the
    /// destination is unchanged, and the temp file is removed. Once it has succeeded the
    /// destination holds the new content, the directory sync (durable writes) still runs, and a
    /// failure of any later step is reported as [`FsError::ReplacedButDirSyncFailed`] or
    /// [`FsError::ReplacedButPostStepFailed`]. Check [`FsError::target_replaced`] before deciding
    /// to write again. What a durable write guarantees per platform is on [`ReplaceMode`].
    pub fn commit(self) -> Result<()> {
        self.commit_inner(false, false, |_| Ok(())).map(|_| ())
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
        mut self,
        want_digest: bool,
        checked: bool,
        check: impl FnOnce(&Path) -> std::io::Result<()>,
    ) -> Result<Option<String>> {
        let file = self.file.take().expect("writer is open until commit");
        let result = self.finish(file, want_digest, checked, check);
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
        checked: bool,
        check: impl FnOnce(&Path) -> std::io::Result<()>,
    ) -> Result<Option<String>> {
        if self.options.durable {
            durable_sync(&file).map_err(|e| FsError::io(&self.temp, e))?;
            #[cfg(feature = "testing")]
            testing::record(testing::Step::TempFlushed);
        } else if checked {
            // A caller's check is about to vouch for these bytes: they must be on stable
            // storage (the file only, never the directory) before it reads them, in every mode.
            file.sync_all().map_err(|e| FsError::io(&self.temp, e))?;
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
///
/// On Windows the check also runs on the name the filesystem will store: Win32 drops trailing
/// spaces and periods from the last path component (and `name:stream` addresses a stream of
/// `name`), so `".record.rktmp-123-1-0 "` is created as the reserved `".record.rktmp-123-1-0"`.
/// Every name check in the crate funnels through here, so no creation API sees only the raw text.
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
/// `:stream` suffix, without trailing spaces and periods.
fn windows_stored_name(name: &str) -> String {
    let base = name.split(':').next().unwrap_or(name);
    base.trim_end_matches([' ', '.']).to_string()
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

/// Directories whose link in their parent this process has already made durable, so a later
/// create-or-open of the same directory need not sync again.
///
/// This is a *positive* record: absence means "unknown", and unknown is synced. (The earlier
/// design recorded the opposite, the directories known to be unpublished, so a directory made by
/// another process, by a crashed run, or by a thread between its `mkdir` and its bookkeeping was
/// assumed published and skipped.) A key is added only after the sync of the parent succeeded, so
/// a failed sync leaves nothing behind and the retry syncs again.
///
/// The key is `(dev, ino, birth time)`; the birth time, where the filesystem has one, keeps an
/// inode number that was freed and reused for a new directory from matching. Bounded: past
/// `CAP` keys the record is cleared, which only costs repeat syncs. In-process only.
#[cfg(unix)]
pub(crate) mod published {
    use std::collections::HashSet;
    use std::fs::Metadata;
    use std::os::unix::fs::MetadataExt;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    pub(crate) type Key = (u64, u64, Option<u128>);
    const CAP: usize = 4096;

    fn set() -> MutexGuard<'static, HashSet<Key>> {
        static SET: OnceLock<Mutex<HashSet<Key>>> = OnceLock::new();
        SET.get_or_init(|| Mutex::new(HashSet::new()))
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    /// The key of a directory, `None` for anything that is not one (a link is never keyed).
    pub(crate) fn key_of(meta: &Metadata) -> Option<Key> {
        if !meta.is_dir() {
            return None;
        }
        let birth = meta
            .created()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos());
        Some((meta.dev(), meta.ino(), birth))
    }

    /// Whether this process owes the sync of the link to the directory `meta` describes, and the
    /// key to record once it has been done.
    ///
    /// Owed when `created` (this call made it) or when the directory is owned by the effective
    /// user and not yet recorded. A directory owned by someone else (`/`, `/Users`, `/home`) was
    /// not made by this user, so publishing its link is not this user's to do and is skipped.
    pub(crate) fn owed(meta: &Metadata, created: bool) -> (bool, Option<Key>) {
        let key = key_of(meta);
        if let Some(key) = key {
            if set().contains(&key) {
                return (false, Some(key));
            }
        }
        // SAFETY: geteuid has no preconditions.
        let ours = created || meta.uid() == unsafe { libc::geteuid() };
        (ours, key)
    }

    pub(crate) fn mark(key: Key) {
        let mut set = set();
        if set.len() >= CAP {
            set.clear();
        }
        set.insert(key);
    }
}

/// Creates `dir` and its missing ancestors one component at a time, top down, and makes the link
/// of every component this process has not yet published durable by syncing its parent before
/// going on (Unix). That covers the directories this call makes **and** the ones it merely finds:
/// a directory another process, a crashed earlier run or a concurrent thread made may have been
/// created without its parent having been synced, and nothing on disk says so. The sync is owed
/// for a directory owned by the effective user (see `published::owed`) and happens once per
/// directory per process. A failed sync is [`FsError::DirSyncFailed`]; the directories already
/// made stay, and a retry syncs again. New directories are `0700` when `owner_only`, and on macOS
/// are refused ([`FsError::UnsafeAcl`]) under a parent whose ACL would give them an `allow`
/// entry. Elsewhere there is no directory flush, so only the ordinary creation runs.
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
        let mut created = false;
        match fs::symlink_metadata(path) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if owner_only {
                    crate::acl::guard_parent_path(&parent_of(path), path)?;
                }
                let mut builder = fs::DirBuilder::new();
                if owner_only {
                    builder.mode(0o700);
                }
                match builder.create(path) {
                    Ok(()) => {
                        created = true;
                        if owner_only {
                            if let Err(error) = crate::acl::scrub_created_path(path) {
                                let _ = fs::remove_dir(path);
                                return Err(error);
                            }
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(e) => return Err(FsError::io(path, e)),
                }
            }
            Err(e) => return Err(FsError::io(path, e)),
        }
        // Looked at again after the creation: only a real directory (not a link to one) is keyed.
        let meta = fs::symlink_metadata(path).ok().filter(|m| m.is_dir());
        let (needed, key) = match &meta {
            Some(meta) => published::owed(meta, created),
            None => (false, None),
        };
        if !needed || path.parent().is_none() {
            continue;
        }
        sync_parent_dir(path).map_err(|source| FsError::DirSyncFailed {
            path: path.to_path_buf(),
            source,
        })?;
        if let Some(key) = key {
            published::mark(key);
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
    outcome
        .map_err(|error| classify_partial(error, temp, target, backup.as_deref()))
        .map_err(|error| protect_partial(error, target))
}

/// A unique sibling name the new content of an interrupted replace is moved to:
/// `.<name>.rktnew-<pid>-<n>-<nanos in hex>`. Like the backup name it is not a `.rktmp-` name, so
/// no sweep ever reclaims it; unlike a temp name it is not a claim that a live writer owns it.
fn recovery_path(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    parent_of(target).join(format!(
        ".{name}.rktnew-{}-{n}-{nanos:x}",
        std::process::id()
    ))
}

/// Moves the temp file that holds the only copy of the new content to a [`recovery_path`] name,
/// exclusively (never over anything). On the `link` fallback of the exclusive rename the old
/// name may remain as a second name for the same inode; the recovery name has the full content
/// either way, which is all this needs.
fn move_to_recovery_name(temp: &Path, target: &Path) -> std::io::Result<PathBuf> {
    #[cfg(feature = "testing")]
    if let Some(error) = testing::recovery_move_failure() {
        return Err(error);
    }
    let mut last = None;
    for _ in 0..TEMP_ATTEMPTS {
        let to = recovery_path(target);
        match rename_exclusive_io(temp, &to) {
            Ok(_) => return Ok(to),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("recovery name collisions")))
}

/// For an interrupted replace, moves the new content out of its sweepable temp name, so that
/// neither a sweep nor the writer's own cleanup can delete the only copy, and rewrites the error
/// to name where it now is. If the move fails the temp stays where it is (and stays sweepable):
/// the error then says so, in its message, because there is nothing else left to protect it with.
/// Any other error passes through unchanged.
fn protect_partial(error: std::io::Error, target: &Path) -> std::io::Error {
    let kind = error.kind();
    let mut partial = match PartialReplace::take(error) {
        Err(error) => return error,
        Ok(partial) => partial,
    };
    if let Some(temp) = partial.new_content.take() {
        partial.new_content = Some(match move_to_recovery_name(&temp, target) {
            Ok(recovery) => recovery,
            Err(moved) => {
                partial.source = std::io::Error::new(
                    partial.source.kind(),
                    format!(
                        "{}; the new content could not be moved out of the sweepable temp name {} ({moved}) and a temp sweep may delete it",
                        partial.source,
                        temp.display()
                    ),
                );
                temp
            }
        });
    }
    std::io::Error::new(kind, partial)
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
        /// The temp file was flushed before publication (durable writes, and checked writes in any mode).
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
        static RECOVERY_MOVE_FAILS: Cell<bool> = const { Cell::new(false) };
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

    /// Make the move of an interrupted replace's new content to its recovery name
    /// (`.<name>.rktnew-...`) fail on this thread, so the "could not be protected" outcome of
    /// [`FsError::ReplaceInterrupted`](crate::FsError::ReplaceInterrupted) can be produced.
    pub fn fail_recovery_move() -> HookGuard {
        RECOVERY_MOVE_FAILS.with(|f| f.set(true));
        HookGuard(|| RECOVERY_MOVE_FAILS.with(|f| f.set(false)))
    }

    /// The flushes, replaces and directory syncs this thread's atomic writes made since the last
    /// call, in order. Empties the log.
    pub fn take_steps() -> Vec<Step> {
        STEPS.with(|steps| std::mem::take(&mut *steps.borrow_mut()))
    }

    pub(super) fn recovery_move_failure() -> Option<std::io::Error> {
        RECOVERY_MOVE_FAILS
            .with(Cell::get)
            .then(|| std::io::Error::other("injected recovery move failure"))
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
    fn the_new_content_is_moved_to_a_name_no_sweep_matches() {
        let (_d, target, temp, _backup) = setup(false, true, false);
        // A temp name as `temp_name` makes it, holding the only copy of the new content.
        let temp_named = temp.with_file_name(temp_name("t", 0));
        fs::rename(&temp, &temp_named).unwrap();
        let error = protect_partial(
            classify_partial(code(1177), &temp_named, &target, None),
            &target,
        );
        let partial = PartialReplace::take(error).expect("still a partial replace");
        let recovery = partial.new_content.expect("the new content is named");
        assert_ne!(recovery, temp_named);
        assert!(!temp_named.exists(), "the sweepable name is gone");
        assert_eq!(fs::read(&recovery).unwrap(), b"x");
        let name = recovery.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with(".t.rktnew-"), "{name}");
        assert_eq!(generated_temp_pid(&name), None, "{name}");
        // The sweep, at zero age, leaves it alone.
        assert_eq!(
            sweep_stale_temps(recovery.parent().unwrap(), Duration::ZERO).unwrap(),
            0
        );
        assert!(recovery.exists());
    }

    #[test]
    fn recovery_names_are_unique_and_never_swept_even_for_names_that_contain_the_marker() {
        for target in ["/d/state.json", "/d/x.rktmp-1-2-3"] {
            let target = Path::new(target);
            let (a, b) = (recovery_path(target), recovery_path(target));
            assert_ne!(a, b);
            let name = a.file_name().unwrap().to_string_lossy().into_owned();
            assert!(name.contains(".rktnew-"), "{name}");
            assert_eq!(generated_temp_pid(&name), None, "{name}");
            assert!(reject_reserved_name(&name).is_ok(), "{name}");
        }
    }

    #[test]
    fn an_ordinary_error_is_not_touched_by_protection() {
        let (_d, target, _temp, _backup) = setup(true, true, false);
        let error = protect_partial(code(5), &target);
        assert_eq!(error.raw_os_error(), Some(5));
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
    fn windows_names_that_normalize_to_a_temp_name_are_reserved() {
        // Win32 drops trailing spaces and periods and addresses `name:stream` as a stream of
        // `name`: each of these is created as the reserved `.record.rktmp-123-1-0`.
        for name in [
            ".record.rktmp-123-1-0 ",
            ".record.rktmp-123-1-0.",
            ".record.rktmp-123-1-0. . ",
            ".record.rktmp-123-1-0:stream",
            ".record.rktmp-123-1-0::$DATA",
            ".record.rktmp-123-1-ff-0 ",
        ] {
            assert!(
                matches!(
                    reject_reserved_name_on(true, name),
                    Err(FsError::BadName(_))
                ),
                "{name:?} must be refused on Windows"
            );
            // Elsewhere those are different, legitimate names.
            assert!(reject_reserved_name_on(false, name).is_ok(), "{name:?}");
        }
        // Ordinary names, including ones with a space or period inside, are untouched.
        for name in [
            "record.json",
            "my record.json",
            "a.b.c",
            ".record.rktmp-1-final ",
        ] {
            assert!(reject_reserved_name_on(true, name).is_ok(), "{name:?}");
        }
        assert_eq!(windows_stored_name("a. ."), "a");
        assert_eq!(windows_stored_name("a:b"), "a");
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

#[cfg(all(test, unix))]
mod published_tests {
    use super::published;
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn a_foreign_owned_directory_is_never_owed() {
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } == 0 {
            return; // as root every directory is owned by the caller
        }
        let root = std::fs::metadata("/").unwrap();
        assert_ne!(root.uid(), unsafe { libc::geteuid() }, "`/` is not ours");
        assert!(
            !published::owed(&root, false).0,
            "not this user's link to publish"
        );
        assert!(
            published::owed(&root, true).0,
            "a directory this call made is always owed"
        );
    }

    #[test]
    fn a_published_directory_is_not_owed_again_and_an_unknown_one_is() {
        let dir = tempfile::tempdir().unwrap();
        let meta = std::fs::symlink_metadata(dir.path()).unwrap();
        let (owed, key) = published::owed(&meta, false);
        assert!(owed, "unknown and ours: owed");
        published::mark(key.expect("a directory has a key"));
        assert!(!published::owed(&meta, false).0, "recorded after its sync");
        assert!(!published::owed(&meta, true).0, "recorded is recorded");
        let file = dir.path().join("f");
        std::fs::write(&file, b"x").unwrap();
        let file_meta = std::fs::symlink_metadata(&file).unwrap();
        assert_eq!(
            published::owed(&file_meta, false).1,
            None,
            "files are never keyed"
        );
    }
}

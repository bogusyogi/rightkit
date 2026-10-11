//! Safe filesystem state for Right Suite apps.
//!
//! - [`atomic`]: temp + `fsync` + rename replacement, streaming writer, crash-orphan sweep.
//!   [`atomic_write`] is the one-call entry point.
//! - [`lock`]: OS advisory cross-process locks with owner/stale reporting. Two primitives:
//!   [`FileLock::acquire`] / [`FileLock::try_acquire`] take the exclusive lock, and
//!   [`FileLock::acquire_shared`] takes a shared (reader) lock. [`LockKind`] picks POSIX record
//!   locks (`Fcntl`, the default) or `flock(2)` (`Flock`) on Unix; [`probe_owner`] reads the
//!   owner record without locking.
//! - [`digest`] (feature `digest`): SHA-256 of bytes or files, and lowercase hex.
//! - [`private`]: owner-only files and directories (0600/0700, Windows DACL).
//! - [`state`]: [`StateDir`], a descriptor-pinned state directory (`O_NOFOLLOW`, fd-relative
//!   `*at` operations) that a swapped or symlinked path cannot redirect.
//! - [`identity`] (feature `json`): stable file identity across renames and atomic replacement.
//! - [`clone`]: macOS `clonefile(2)` with `CLONE_NOFOLLOW`; [`clone_file_nofollow`] never falls
//!   back to a byte copy, [`clone_or_link_or_copy_nofollow`] tries clone, hard link, then copy.
//! - [`swap`]: macOS `renamex_np(RENAME_SWAP)`; atomically exchanges two paths ([`swap_paths`]).

#[cfg(all(feature = "testing", not(debug_assertions)))]
compile_error!(
    "rightkit-fs feature `testing` injects faults into atomic writes and is for debug and test builds only; \
     remove it from release builds"
);

#[cfg(all(feature = "fs", unix))]
pub(crate) mod acl;
#[cfg(feature = "fs")]
pub mod atomic;
#[cfg(feature = "fs")]
pub mod clone;
#[cfg(feature = "digest")]
pub mod digest;
#[cfg(feature = "fs")]
pub mod error;
#[cfg(feature = "json")]
pub mod identity;
#[cfg(feature = "lock")]
pub mod lock;
#[cfg(feature = "fs")]
pub mod netfs;
#[cfg(feature = "fs")]
pub mod private;
#[cfg(feature = "fs")]
pub mod state;
#[cfg(feature = "fs")]
pub mod swap;

#[cfg(all(feature = "fs", feature = "digest"))]
pub use atomic::atomic_write_with_digest;
#[cfg(feature = "fs")]
pub use atomic::{
    atomic_write, atomic_write_private, atomic_write_with, atomic_write_with_options, durable_sync,
    durable_sync_with, publication_durability, rename_exclusive, sweep_stale_temps, AtomicOptions,
    AtomicWriteOptions, AtomicWriter, Privacy, PublicationDurability, ReplaceMode, RetryPolicy,
    SyncLevel, SyncPolicy,
};
#[cfg(feature = "fs")]
pub use clone::{
    clone_file_nofollow, clone_or_link_or_copy_nofollow, clone_or_link_or_copy_nofollow_with,
    CloneError, CloneOptions, CloneOutcome,
};
#[cfg(feature = "fs")]
pub use error::{FsError, PostPublishStep, Result};
#[cfg(feature = "json")]
pub use identity::{FileKey, IdentityStore, Resolution, StableId, Tracked};
#[cfg(feature = "lock")]
pub use lock::{probe_owner, with_lock, FileLock, LockKind, LockOptions, LockOwner};
#[cfg(feature = "fs")]
pub use netfs::is_network_filesystem;
#[cfg(feature = "fs")]
pub use private::{
    create_private_new, ensure_private_dir, restrict_owner_only, sweep_owner_only,
    write_private_file, write_private_text,
};
#[cfg(feature = "fs")]
pub use state::{DirEntry, EntryKind, EntryStat, StateDir, StateOptions};
#[cfg(feature = "fs")]
pub use swap::{is_swap_unsupported, swap_paths};

/// Whether process `pid` currently exists.
#[cfg(feature = "fs")]
pub fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: signal 0 performs only the existence/permission check.
        let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
        rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
            fn GetExitCodeProcess(handle: *mut std::ffi::c_void, code: *mut u32) -> i32;
            fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
        }
        const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
        const STILL_ACTIVE: u32 = 259;
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return false;
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(handle, &mut code);
            CloseHandle(handle);
            ok != 0 && code == STILL_ACTIVE
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        true
    }
}

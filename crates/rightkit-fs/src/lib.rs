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
#[cfg(all(feature = "fs", windows))]
mod windows_temp_sweep;

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

/// Conservative liveness result used by crash recovery.
///
/// `Unknown` is deliberately distinct from `Alive`: permission and query failures do not prove
/// either state, and cleanup must never turn them into a deletion decision.
#[cfg(feature = "fs")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProcessLiveness {
    Alive,
    Dead,
    Unknown,
}

/// Probe `pid` without treating a failed probe as proof of death.
#[cfg(feature = "fs")]
pub(crate) fn process_liveness(pid: u32) -> ProcessLiveness {
    #[cfg(unix)]
    {
        if pid == 0 || pid > libc::pid_t::MAX as u32 {
            return ProcessLiveness::Unknown;
        }
        // SAFETY: signal 0 performs only the existence/permission check.
        let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
        if rc == 0 {
            ProcessLiveness::Alive
        } else if std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            ProcessLiveness::Dead
        } else {
            ProcessLiveness::Unknown
        }
    }
    #[cfg(windows)]
    {
        if pid == 0 {
            return ProcessLiveness::Unknown;
        }
        const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
        const STILL_ACTIVE: u32 = 259;
        // SAFETY: the PID is scalar; the exit-code pointer stays live through the call.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                // Error 87 alone is not proof that the process vanished. A complete bounded
                // process snapshot may prove absence; all other failed probes remain Unknown.
                if std::io::Error::last_os_error().raw_os_error() == Some(ERROR_INVALID_PARAMETER) {
                    return snapshot_liveness(pid);
                }
                return ProcessLiveness::Unknown;
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(handle, &mut code);
            CloseHandle(handle);
            if ok == 0 {
                ProcessLiveness::Unknown
            } else if code == STILL_ACTIVE {
                ProcessLiveness::Alive
            } else {
                ProcessLiveness::Dead
            }
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        ProcessLiveness::Unknown
    }
}

#[cfg(all(feature = "fs", windows))]
const ERROR_INVALID_PARAMETER: i32 = 87;
#[cfg(all(feature = "fs", windows))]
const ERROR_NO_MORE_FILES: i32 = 18;
#[cfg(all(feature = "fs", windows))]
const PROCESS_SNAPSHOT_LIMIT: usize = 4096;

#[cfg(all(feature = "fs", windows))]
#[repr(C)]
struct ProcessEntry32W {
    size: u32,
    usage: u32,
    process_id: u32,
    default_heap_id: usize,
    module_id: u32,
    threads: u32,
    parent_process_id: u32,
    priority_class: i32,
    flags: u32,
    executable: [u16; 260],
}

/// Return `Dead` only after a complete, bounded snapshot did not list `pid`.
#[cfg(all(feature = "fs", windows))]
fn snapshot_liveness_from_pids(pid: u32, pids: &[u32], complete: bool) -> ProcessLiveness {
    if complete && pids.len() < PROCESS_SNAPSHOT_LIMIT && !pids.contains(&pid) {
        ProcessLiveness::Dead
    } else {
        ProcessLiveness::Unknown
    }
}

/// A failed `OpenProcess` with ERROR_INVALID_PARAMETER still needs independent absence proof.
#[cfg(all(feature = "fs", windows))]
fn snapshot_liveness(pid: u32) -> ProcessLiveness {
    const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;
    // SAFETY: process ID zero selects every process; no pointers are dereferenced here.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot as isize == -1 {
        return ProcessLiveness::Unknown;
    }
    let mut pids = Vec::with_capacity(64);
    // SAFETY: zero initialization is valid for this C record after setting its required size.
    let mut entry: ProcessEntry32W = unsafe { std::mem::zeroed() };
    entry.size = std::mem::size_of::<ProcessEntry32W>() as u32;
    // SAFETY: snapshot is valid and `entry` remains valid for both enumeration calls.
    if unsafe { Process32FirstW(snapshot, &mut entry) } == 0 {
        // SAFETY: snapshot came from CreateToolhelp32Snapshot and is closed exactly once.
        unsafe { CloseHandle(snapshot) };
        return ProcessLiveness::Unknown;
    }
    let complete = loop {
        pids.push(entry.process_id);
        if pids.len() >= PROCESS_SNAPSHOT_LIMIT {
            break false;
        }
        entry.size = std::mem::size_of::<ProcessEntry32W>() as u32;
        // SAFETY: snapshot is valid and `entry` remains valid for the call.
        if unsafe { Process32NextW(snapshot, &mut entry) } == 0 {
            break std::io::Error::last_os_error().raw_os_error() == Some(ERROR_NO_MORE_FILES);
        }
    };
    // SAFETY: snapshot came from CreateToolhelp32Snapshot and is closed exactly once.
    unsafe { CloseHandle(snapshot) };
    snapshot_liveness_from_pids(pid, &pids, complete)
}

#[cfg(all(feature = "fs", windows))]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
    fn GetExitCodeProcess(handle: *mut std::ffi::c_void, code: *mut u32) -> i32;
    fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
    fn CreateToolhelp32Snapshot(flags: u32, process_id: u32) -> *mut std::ffi::c_void;
    fn Process32FirstW(snapshot: *mut std::ffi::c_void, entry: *mut ProcessEntry32W) -> i32;
    fn Process32NextW(snapshot: *mut std::ffi::c_void, entry: *mut ProcessEntry32W) -> i32;
}

/// Whether process `pid` is not confirmed dead.
///
/// This remains a boolean compatibility helper. `true` includes an unavailable liveness probe;
/// callers that reclaim files must use the internal tri-state probe and require `Dead`.
#[cfg(feature = "fs")]
pub fn process_alive(pid: u32) -> bool {
    !matches!(process_liveness(pid), ProcessLiveness::Dead)
}

#[cfg(all(test, feature = "fs"))]
mod tests {
    use super::{process_alive, process_liveness, ProcessLiveness};

    #[test]
    fn current_process_is_alive() {
        assert_eq!(process_liveness(std::process::id()), ProcessLiveness::Alive);
        assert!(process_alive(std::process::id()));
    }

    #[test]
    fn invalid_pid_is_unknown_and_conservatively_alive() {
        assert_eq!(process_liveness(0), ProcessLiveness::Unknown);
        assert!(process_alive(0));
    }

    #[cfg(windows)]
    #[test]
    fn complete_snapshot_absence_confirms_death() {
        assert_eq!(
            super::snapshot_liveness_from_pids(42, &[1, 2, 3], true),
            ProcessLiveness::Dead
        );
    }

    #[cfg(windows)]
    #[test]
    fn snapshot_presence_is_unknown() {
        assert_eq!(
            super::snapshot_liveness_from_pids(42, &[1, 42, 3], true),
            ProcessLiveness::Unknown
        );
    }

    #[cfg(windows)]
    #[test]
    fn failed_snapshot_is_unknown() {
        assert_eq!(
            super::snapshot_liveness_from_pids(42, &[], false),
            ProcessLiveness::Unknown
        );
    }

    #[cfg(windows)]
    #[test]
    fn capped_snapshot_is_unknown() {
        assert_eq!(
            super::snapshot_liveness_from_pids(42, &[1; super::PROCESS_SNAPSHOT_LIMIT], true),
            ProcessLiveness::Unknown
        );
    }
}

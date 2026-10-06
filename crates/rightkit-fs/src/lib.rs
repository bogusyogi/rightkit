//! Safe filesystem state for Right Suite apps.
//!
//! - [`atomic`]: temp + `fsync` + rename replacement, streaming writer, crash-orphan sweep.
//! - [`lock`]: OS advisory cross-process locks with owner/stale reporting.
//! - [`private`]: owner-only files and directories (0600/0700, Windows DACL).
//! - [`state`]: [`StateDir`], a descriptor-pinned state directory (`O_NOFOLLOW`, fd-relative
//!   `*at` operations) that a swapped or symlinked path cannot redirect.
//! - [`identity`]: stable file identity across renames and atomic replacement.

pub mod atomic;
pub mod error;
pub mod identity;
pub mod lock;
pub mod netfs;
pub mod private;
pub mod state;

pub use atomic::{atomic_write, atomic_write_private, atomic_write_with, sweep_stale_temps, AtomicOptions, AtomicWriter};
pub use error::{FsError, Result};
pub use identity::{FileKey, IdentityStore, Resolution, StableId, Tracked};
pub use netfs::is_network_filesystem;
pub use lock::{with_lock, FileLock, LockOptions, LockOwner};
pub use private::{
    create_private_new, ensure_private_dir, restrict_owner_only, sweep_owner_only, write_private_file,
    write_private_text,
};
pub use state::{EntryKind, EntryStat, StateDir};

/// Whether process `pid` currently exists.
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

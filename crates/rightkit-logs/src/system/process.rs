//! Content-free resource snapshot of any same-user process, by pid (issue #1279).
//!
//! [`metrics::snapshot`](super::metrics::snapshot) covers only the calling process. A supervisor
//! that measures all of its own processes from one place uses [`snapshot_pid`], or
//! [`snapshot_identity`] for a pid it remembered, which returns `None` once the pid names another
//! process (a reused pid is never misread).
//!
//! Figures come from `rightkit-child-process` (`process_usage_of`): resident bytes, private
//! footprint (macOS `phys_footprint`, Windows `PrivateUsage`), CPU seconds, disk bytes and
//! operations, wakeups (macOS) and threads. This module adds the open handle count (Windows
//! handles, Unix file descriptors). A figure the host cannot report is `None`, never zero. GPU
//! time is not exposed per foreign process by any supported OS API, so `gpu_time_ns` is always
//! `None` for a pid snapshot.

use rightkit_child_process::{process_usage_of, process_usage_of_identity, AdoptedIdentity};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io;
use std::num::NonZeroU32;
use std::time::UNIX_EPOCH;

/// A pid with the start time that makes it a name for exactly one process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessIdentity {
    pub pid: u32,
    /// The host's start token (macOS: seconds and microseconds; Linux: clock ticks since boot;
    /// Windows: 100 ns ticks since 1601). Comparable only on the host that produced it.
    pub start_major: u64,
    pub start_minor: u64,
    /// Start as Unix milliseconds, when the host reports it.
    pub start_unix_ms: Option<u64>,
}

impl ProcessIdentity {
    /// The identity of `pid` now. `Ok(None)` when no process has that pid. The caller vouches that
    /// the pid names the intended process at this moment.
    pub fn capture(pid: u32) -> io::Result<Option<Self>> {
        let pid = nonzero(pid)?;
        Ok(AdoptedIdentity::capture(pid)?.map(|adopted| Self::from_adopted(&adopted)))
    }

    fn from_adopted(adopted: &AdoptedIdentity) -> Self {
        let token = adopted.start_token();
        Self {
            pid: adopted.pid(),
            start_major: token.major,
            start_minor: token.minor,
            start_unix_ms: adopted
                .start_time()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok()),
        }
    }

    fn to_adopted(&self) -> io::Result<AdoptedIdentity> {
        Ok(AdoptedIdentity::from_recorded(
            nonzero(self.pid)?,
            rightkit_child_process::StartToken {
                major: self.start_major,
                minor: self.start_minor,
            },
            None,
        ))
    }
}

/// Resource counters of one process at one moment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessSnapshot {
    pub identity: ProcessIdentity,
    pub resident_bytes: u64,
    /// Private footprint (macOS `phys_footprint`) or private bytes (Windows `PrivateUsage`).
    pub footprint_bytes: u64,
    pub cpu_seconds: f64,
    pub thread_count: Option<u32>,
    /// Open handles (Windows) or file descriptors (Unix).
    pub handle_count: Option<u32>,
    /// Thread wakeups since start (macOS).
    pub wakeups_total: Option<u64>,
    pub disk_ops_total: Option<u64>,
    pub disk_read_bytes: Option<u64>,
    pub disk_write_bytes: Option<u64>,
    /// Always `None` for a pid snapshot (see the module documentation).
    pub gpu_time_ns: Option<u64>,
}

impl ProcessSnapshot {
    /// Numbers only, as a JSON object (no paths, names or command lines).
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

fn nonzero(pid: u32) -> io::Result<NonZeroU32> {
    NonZeroU32::new(pid)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "pid 0 names no process"))
}

/// Snapshot of `pid` now, with the identity read just before the counters so a caller can store
/// it and pass it to [`snapshot_identity`] later. An exited pid is `ErrorKind::NotFound`; another
/// user's process or one that denies the query is `PermissionDenied`.
pub fn snapshot_pid(pid: u32) -> io::Result<ProcessSnapshot> {
    let identity = ProcessIdentity::capture(pid)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no process with that pid"))?;
    snapshot_identity(&identity)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "the process exited or its pid was reused while it was read",
        )
    })
}

/// Snapshot of a remembered process: `Ok(None)` when the pid is gone or now names a different
/// process (its start time differs).
pub fn snapshot_identity(identity: &ProcessIdentity) -> io::Result<Option<ProcessSnapshot>> {
    let adopted = identity.to_adopted()?;
    let Some(sampled) = process_usage_of_identity(&adopted)? else {
        return Ok(None);
    };
    let usage = sampled.usage;
    Ok(Some(ProcessSnapshot {
        identity: identity.clone(),
        resident_bytes: usage.resident_bytes,
        footprint_bytes: usage.footprint_bytes,
        cpu_seconds: usage.cpu_seconds,
        thread_count: sampled.thread_count,
        handle_count: handle_count(identity.pid),
        wakeups_total: usage.wakeups_total,
        disk_ops_total: usage.disk_ops_total,
        disk_read_bytes: usage.disk_read_bytes,
        disk_write_bytes: usage.disk_write_bytes,
        gpu_time_ns: None,
    }))
}

/// Same as [`snapshot_pid`] without the identity round trip, for a caller that only samples the
/// current pid of a process it just spawned.
pub fn snapshot_unchecked(pid: u32) -> io::Result<ProcessSnapshot> {
    let identity = ProcessIdentity {
        pid,
        start_major: 0,
        start_minor: 0,
        start_unix_ms: None,
    };
    let sampled = process_usage_of(nonzero(pid)?)?;
    let usage = sampled.usage;
    Ok(ProcessSnapshot {
        identity,
        resident_bytes: usage.resident_bytes,
        footprint_bytes: usage.footprint_bytes,
        cpu_seconds: usage.cpu_seconds,
        thread_count: sampled.thread_count,
        handle_count: handle_count(pid),
        wakeups_total: usage.wakeups_total,
        disk_ops_total: usage.disk_ops_total,
        disk_read_bytes: usage.disk_read_bytes,
        disk_write_bytes: usage.disk_write_bytes,
        gpu_time_ns: None,
    })
}

#[cfg(target_os = "macos")]
fn handle_count(pid: u32) -> Option<u32> {
    // SAFETY: a null buffer with size 0 asks proc_pidinfo for the byte size of the fd list.
    let bytes = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDLISTFDS,
            0,
            std::ptr::null_mut(),
            0,
        )
    };
    let entry = std::mem::size_of::<libc::proc_fdinfo>() as i32;
    (bytes >= 0 && entry > 0).then(|| (bytes / entry) as u32)
}

#[cfg(target_os = "windows")]
fn handle_count(pid: u32) -> Option<u32> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        GetProcessHandleCount, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // SAFETY: the handle is opened, used for one query and closed here; `count` is writable.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut count = 0u32;
        let ok = GetProcessHandleCount(handle, &mut count);
        CloseHandle(handle);
        (ok != 0).then_some(count)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn handle_count(pid: u32) -> Option<u32> {
    std::fs::read_dir(format!("/proc/{pid}/fd"))
        .ok()
        .map(|entries| entries.count() as u32)
}

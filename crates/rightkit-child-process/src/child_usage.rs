//! What a child cost, read when it was reaped (rightkit#1281).
//!
//! An app that spawns helpers (`smartctl`, PowerShell, `git`) can count what they cost by reading
//! [`ChildUsage`] from [`OwnedChild::resource_usage`](crate::OwnedChild::resource_usage) or
//! [`ProcessHandle::resource_usage`](crate::ProcessHandle::resource_usage) after the child's exit
//! status was collected, or from [`RunOutput::usage`](crate::RunOutput::usage).
//!
//! | Figure | Unix | Windows |
//! |---|---|---|
//! | `user_cpu`, `system_cpu` | `wait4` `ru_utime`, `ru_stime` of the reaped child and the descendants it waited for | `GetProcessTimes` user and kernel of the child process alone |
//! | `max_rss_bytes` | `ru_maxrss` (bytes on macOS, KiB on other Unix, converted) | `PeakWorkingSetSize`, `None` when the OS refuses it for an exited process |
//!
//! Unix reads the figures from the same `wait4` call that reaps the child, so they are exact and
//! exist only once this crate reaped it. Windows reads them from the retained process handle once
//! the process has exited.

use std::time::Duration;

/// CPU time and peak memory of a reaped child.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct ChildUsage {
    /// CPU time spent in user mode.
    pub user_cpu: Duration,
    /// CPU time spent in the kernel on the child's behalf.
    pub system_cpu: Duration,
    /// Peak resident set (peak working set on Windows) in bytes; `None` when the host did not
    /// report it.
    pub max_rss_bytes: Option<u64>,
}

impl ChildUsage {
    /// `user_cpu + system_cpu`.
    pub fn cpu(&self) -> Duration {
        self.user_cpu.saturating_add(self.system_cpu)
    }

    #[cfg(unix)]
    pub(crate) fn from_rusage(usage: &libc::rusage) -> Self {
        let time = |t: libc::timeval| {
            Duration::new(
                u64::try_from(t.tv_sec).unwrap_or(0),
                (u32::try_from(t.tv_usec).unwrap_or(0).min(999_999)) * 1_000,
            )
        };
        let raw = u64::try_from(usage.ru_maxrss).unwrap_or(0);
        // macOS reports bytes, Linux and the other Unixes kibibytes.
        let max_rss = if cfg!(any(target_os = "macos", target_os = "ios")) {
            raw
        } else {
            raw.saturating_mul(1024)
        };
        Self {
            user_cpu: time(usage.ru_utime),
            system_cpu: time(usage.ru_stime),
            max_rss_bytes: (max_rss > 0).then_some(max_rss),
        }
    }

    /// Reads the figures from a process handle whose process has exited. `None` when even the
    /// CPU times cannot be read.
    #[cfg(windows)]
    pub(crate) fn from_exited_process(handle: &std::os::windows::io::OwnedHandle) -> Option<Self> {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::{
            Foundation::{FILETIME, HANDLE},
            System::{
                ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
                Threading::GetProcessTimes,
            },
        };
        let process = HANDLE(handle.as_raw_handle() as _);
        let (mut created, mut exited, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        // SAFETY: the handle is open for query (it came from CreateProcess); outputs are valid.
        unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) }
            .ok()?;
        // FILETIME ticks are 100 ns.
        let time = |t: FILETIME| {
            let ticks = (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
            Duration::new(ticks / 10_000_000, ((ticks % 10_000_000) * 100) as u32)
        };
        let mut counters = PROCESS_MEMORY_COUNTERS {
            cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
            ..Default::default()
        };
        // SAFETY: the handle is open for query; the buffer is a sized PROCESS_MEMORY_COUNTERS.
        let peak = unsafe {
            GetProcessMemoryInfo(
                process,
                &mut counters,
                std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
            )
        }
        .ok()
        .map(|()| counters.PeakWorkingSetSize as u64);
        Some(Self {
            user_cpu: time(user),
            system_cpu: time(kernel),
            max_rss_bytes: peak.filter(|bytes| *bytes > 0),
        })
    }
}

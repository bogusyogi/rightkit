//! Resource usage of the current process: the one shared source for the figures a runtime budget
//! names (docs/RUNTIME-BUDGETS.md): resident memory, CPU time, wake-ups and disk operations.
//!
//! Computed on call from the operating system; no thread, no polling, no state. A host or a crate's
//! own `status()` calls [`process_usage`]; two snapshots give rates through [`ProcessUsage::rates_since`].
//!
//! | Figure | macOS | Windows | Linux, Android |
//! |---|---|---|---|
//! | `resident_bytes` | `proc_pid_rusage` `ri_resident_size` | `WorkingSetSize` | `/proc/self/statm` |
//! | `footprint_bytes` | `ri_phys_footprint` | `PrivateUsage` | same as resident |
//! | `cpu_seconds` | `getrusage(RUSAGE_SELF)` user + system | `GetProcessTimes` user + kernel | `getrusage` |
//! | `wakeups_total` | `ri_interrupt_wkups` (thread wake-ups from timer and other interrupts; `ri_pkg_idle_wkups` stays 0 whenever the machine is busy, so it cannot be used) | sum of `ContextSwitchCount` over the live threads of this process (`NtQuerySystemInformation(SystemProcessInformation)`); a thread is switched in once per wake. A thread that exits takes its count with it, so the sum can dip when threads end; 64-bit only | not available |
//! | `disk_ops_total` | not available: `ru_inblock`/`ru_oublock` stay 0 on macOS and no per-process operation count exists; bytes are reported instead | `GetProcessIoCounters` read + write operation counts (all I/O, including pipes and sockets) | `ru_inblock + ru_oublock` |
//!
//! Other Unix targets compile, and `process_usage` returns `ErrorKind::Unsupported` there.
//!
//! A figure the host cannot report is `None`; it is never reported as zero.

use std::io;
use std::time::Duration;

/// One snapshot of the current process's resource usage.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ProcessUsage {
    /// Resident set (working set on Windows) in bytes.
    pub resident_bytes: u64,
    /// Private footprint in bytes: macOS `phys_footprint`, Windows `PrivateUsage`.
    pub footprint_bytes: u64,
    /// Process CPU time (user plus system) since process start, in seconds.
    pub cpu_seconds: f64,
    /// Thread wake-ups (timer and other interrupts) since process start, when the host reports them.
    pub wakeups_total: Option<u64>,
    /// Disk (block or I/O) operations since process start, when the host reports them.
    pub disk_ops_total: Option<u64>,
    /// Bytes read from storage since process start, when the host reports them.
    pub disk_read_bytes: Option<u64>,
    /// Bytes written to storage since process start, when the host reports them.
    pub disk_write_bytes: Option<u64>,
}

/// Rates between two snapshots. Each rate is `None` when either snapshot lacks the counter.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct UsageRates {
    /// Process CPU percent of one core (100 = one full core) over the interval.
    pub cpu_percent: f64,
    pub wakeups_per_second: Option<f64>,
    pub disk_ops_per_second: Option<f64>,
    pub disk_read_bytes_per_second: Option<f64>,
    pub disk_write_bytes_per_second: Option<f64>,
}

impl ProcessUsage {
    /// Rates of change from `earlier` (an older snapshot) to `self` over `elapsed`.
    /// A zero `elapsed` yields zero rates rather than dividing by zero.
    pub fn rates_since(&self, earlier: &ProcessUsage, elapsed: Duration) -> UsageRates {
        let secs = elapsed.as_secs_f64();
        let rate = |now: Option<u64>, then: Option<u64>| match (now, then) {
            (Some(now), Some(then)) if secs > 0.0 => Some(now.saturating_sub(then) as f64 / secs),
            (Some(_), Some(_)) => Some(0.0),
            _ => None,
        };
        UsageRates {
            cpu_percent: if secs > 0.0 {
                (self.cpu_seconds - earlier.cpu_seconds).max(0.0) / secs * 100.0
            } else {
                0.0
            },
            wakeups_per_second: rate(self.wakeups_total, earlier.wakeups_total),
            disk_ops_per_second: rate(self.disk_ops_total, earlier.disk_ops_total),
            disk_read_bytes_per_second: rate(self.disk_read_bytes, earlier.disk_read_bytes),
            disk_write_bytes_per_second: rate(self.disk_write_bytes, earlier.disk_write_bytes),
        }
    }
}

/// Resource usage of the current process, computed now.
pub fn process_usage() -> io::Result<ProcessUsage> {
    imp::sample()
}

/// What a process did over a quiet window: the idle figures a runtime budget names.
#[derive(Clone, Copy, Debug)]
pub struct IdleObservation {
    pub elapsed: Duration,
    pub start: ProcessUsage,
    pub end: ProcessUsage,
    pub rates: UsageRates,
}

/// Sleeps `duration` on the calling thread and reports the process's usage over it. Run it after
/// the load has stopped; it is for benches and tests, so a host reads [`process_usage`] instead.
pub fn observe_idle(duration: Duration) -> io::Result<IdleObservation> {
    let start = process_usage()?;
    let began = std::time::Instant::now();
    std::thread::sleep(duration);
    let end = process_usage()?;
    let elapsed = began.elapsed();
    Ok(IdleObservation {
        elapsed,
        start,
        end,
        rates: end.rates_since(&start, elapsed),
    })
}

/// Names and scale of the load behind an [`IdleObservation`], for [`IdleObservation::result_lines`].
#[derive(Clone, Copy, Debug)]
pub struct BudgetLabels<'a> {
    /// Crate being measured, e.g. `rightkit-cache`.
    pub krate: &'a str,
    pub version: &'a str,
    /// Unit of `scale`, e.g. `entries`.
    pub scale_unit: &'a str,
    /// How much load the crate carried.
    pub scale: u64,
    /// Plain-words description of that load.
    pub what: &'a str,
    /// `synthetic`, `real` or `installed`.
    pub scope: &'a str,
}

impl IdleObservation {
    /// The stable machine-readable lines the runtime-budget gate records, one figure per line:
    /// `RESULT <key>=<number> <unit>`, or `RESULT <key>=unavailable <reason>` when the host cannot
    /// report the figure (never a stand-in zero). `resident_memory_bytes` is the resident set at the
    /// end of the idle window. `scale` and `what` state the load the crate carried; `scope` is
    /// `synthetic`, `real` or `installed`.
    ///
    /// `resident_memory_bytes` is the larger of the resident set `warm` (taken right after the load)
    /// and at the end of the idle window: an OS may compress or page out idle memory, and a ceiling
    /// must cover the loaded state, not only the reclaimed one. The two are also printed separately.
    ///
    /// It also prints the single gate line `RESULT budget crate=.. version=.. resident_bytes=..
    /// idle_cpu_percent=.. wakeups_per_s=.. disk_ops_per_s=.. scale_unit=.. scale_count=..`
    /// (docs/RUNTIME-BUDGETS.md); a figure the host cannot report prints `unavailable` there, which
    /// the gate refuses, and is never replaced by zero.
    pub fn result_lines(&self, warm: &ProcessUsage, labels: &BudgetLabels<'_>) -> String {
        let BudgetLabels {
            krate,
            version,
            scale_unit,
            scale,
            what,
            scope,
        } = *labels;
        let mut out = String::new();
        let mut line = |text: String| {
            out.push_str("RESULT ");
            out.push_str(&text);
            out.push('\n');
        };
        let resident_max = warm.resident_bytes.max(self.end.resident_bytes);
        line(format!("warm_resident_bytes={} bytes", warm.resident_bytes));
        line(format!(
            "idle_resident_bytes={} bytes",
            self.end.resident_bytes
        ));
        if resident_max > 0 {
            line(format!("resident_memory_bytes={resident_max} bytes"));
        } else {
            line("resident_memory_bytes=unavailable host reported a zero resident set".to_string());
        }
        line(format!(
            "footprint_bytes={} bytes",
            warm.footprint_bytes.max(self.end.footprint_bytes)
        ));
        line(format!(
            "idle_window_seconds={:.1} seconds",
            self.elapsed.as_secs_f64()
        ));
        line(format!(
            "idle_cpu_percent={:.3} percent",
            self.rates.cpu_percent
        ));
        let opt = |name: &str,
                   value: Option<f64>,
                   unit: &str,
                   reason: &str,
                   line: &mut dyn FnMut(String)| match value {
            Some(v) => line(format!("{name}={v:.3} {unit}")),
            None => line(format!("{name}=unavailable {reason}")),
        };
        opt(
            "wakeups_per_second",
            self.rates.wakeups_per_second,
            "per_second",
            "the host exposes no process wake-up counter",
            &mut line,
        );
        opt(
            "disk_ops_per_second",
            self.rates.disk_ops_per_second,
            "per_second",
            "the host exposes no process disk-operation counter",
            &mut line,
        );
        opt(
            "disk_read_bytes_per_second",
            self.rates.disk_read_bytes_per_second,
            "bytes_per_second",
            "the host exposes no process disk-byte counter",
            &mut line,
        );
        opt(
            "disk_write_bytes_per_second",
            self.rates.disk_write_bytes_per_second,
            "bytes_per_second",
            "the host exposes no process disk-byte counter",
            &mut line,
        );
        line(format!("scale={scale} {what}"));
        line(format!("scope={scope}"));
        let num =
            |v: Option<f64>| v.map_or_else(|| "unavailable".to_string(), |v| format!("{v:.3}"));
        let resident = if resident_max > 0 {
            resident_max.to_string()
        } else {
            "unavailable".to_string()
        };
        line(format!(
            "budget crate={krate} version={version} resident_bytes={resident} idle_cpu_percent={:.3} wakeups_per_s={} disk_ops_per_s={} scale_unit={scale_unit} scale_count={scale}",
            self.rates.cpu_percent,
            num(self.rates.wakeups_per_second),
            num(self.rates.disk_ops_per_second),
        ));
        out
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::ProcessUsage;
    use std::io;

    extern "C" {
        fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut u8) -> i32;
    }

    /// RUSAGE_INFO_V2 words after the 16-byte uuid (offsets from the SDK's libproc.h
    /// `struct rusage_info_v2`).
    const W_INTERRUPT_WKUPS: usize = 2 + 3;
    const W_RESIDENT: usize = 2 + 6;
    const W_FOOTPRINT: usize = 2 + 7;
    const W_DISK_READ: usize = 2 + 16;
    const W_DISK_WRITE: usize = 2 + 17;

    pub fn sample() -> io::Result<ProcessUsage> {
        let mut words = [0u64; 64];
        // RUSAGE_INFO_V2 = 2.
        let rc = unsafe { proc_pid_rusage(libc::getpid(), 2, words.as_mut_ptr().cast()) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let secs = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
        Ok(ProcessUsage {
            resident_bytes: words[W_RESIDENT],
            footprint_bytes: words[W_FOOTPRINT],
            cpu_seconds: secs(ru.ru_utime) + secs(ru.ru_stime),
            wakeups_total: Some(words[W_INTERRUPT_WKUPS]),
            disk_ops_total: None,
            disk_read_bytes: Some(words[W_DISK_READ]),
            disk_write_bytes: Some(words[W_DISK_WRITE]),
        })
    }
}

#[cfg(all(
    unix,
    not(target_os = "macos"),
    not(any(target_os = "linux", target_os = "android"))
))]
mod imp {
    use super::ProcessUsage;
    use std::io;

    /// Chosen when the crate is compiled for a Unix this module has no sampler for: an
    /// explicit, typed error rather than a procfs read that may not exist.
    pub fn sample() -> io::Result<ProcessUsage> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "process usage sampling is implemented for macOS, Linux, Android and Windows only",
        ))
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod imp {
    use super::ProcessUsage;
    use std::io;

    pub fn sample() -> io::Result<ProcessUsage> {
        let statm = std::fs::read_to_string("/proc/self/statm")?;
        let pages: u64 = statm
            .split_whitespace()
            .nth(1)
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "unreadable /proc/self/statm")
            })?;
        let page = u64::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(1))
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "page size out of range"))?;
        let overflow = || io::Error::new(io::ErrorKind::InvalidData, "memory size overflows u64");
        let resident = pages.checked_mul(page).ok_or_else(overflow)?;
        // Block counters are signed C longs (32-bit on 32-bit targets): widen before adding.
        let blocks = |count: libc::c_long| u64::try_from(count.max(0)).unwrap_or(0);
        let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let secs = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
        Ok(ProcessUsage {
            resident_bytes: resident,
            footprint_bytes: resident,
            cpu_seconds: secs(ru.ru_utime) + secs(ru.ru_stime),
            wakeups_total: None,
            disk_ops_total: blocks(ru.ru_inblock).checked_add(blocks(ru.ru_oublock)),
            disk_read_bytes: None,
            disk_write_bytes: None,
        })
    }
}

#[cfg(windows)]
mod imp {
    use super::ProcessUsage;
    use std::io;
    use windows::Wdk::System::SystemInformation::{
        NtQuerySystemInformation, SystemProcessInformation,
    };
    use windows::Win32::Foundation::{FILETIME, STATUS_INFO_LENGTH_MISMATCH};
    use windows::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows::Win32::System::Threading::{
        GetCurrentProcess, GetCurrentProcessId, GetProcessIoCounters, GetProcessTimes, IO_COUNTERS,
    };
    use windows::Win32::System::WindowsProgramming::{
        SYSTEM_PROCESS_INFORMATION, SYSTEM_THREAD_INFORMATION,
    };

    fn os(error: windows::core::Error) -> io::Error {
        io::Error::from_raw_os_error(error.code().0 & 0xFFFF)
    }

    fn ticks(t: FILETIME) -> u64 {
        ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64
    }

    /// Bytes from the start of a `SYSTEM_PROCESS_INFORMATION` entry to its first
    /// `SYSTEM_THREAD_INFORMATION` (the Windows 10/11 x64 layout; the `windows` crate's public
    /// struct is 8 bytes shorter than the real one, so `size_of` cannot be used).
    #[cfg(target_pointer_width = "64")]
    const THREADS_OFFSET: usize = 0x100;
    /// Upper bound on the process-table buffer.
    #[cfg(target_pointer_width = "64")]
    const MAX_QUERY_BYTES: usize = 256 * 1024 * 1024;

    /// Total context switches (switch-ins, one per thread wake) over every live thread of this
    /// process. `ContextSwitchCount` is the `u32` the crate names `Reserved3` in
    /// `SYSTEM_THREAD_INFORMATION` (offset 0x40, right after `BasePriority`). `None` on any
    /// failure or unexpected layout; never panics.
    #[cfg(target_pointer_width = "64")]
    fn wakeups_total() -> Option<u64> {
        // 8-byte aligned backing store; entry offsets are multiples of 8.
        let mut words: Vec<u64> = vec![0; (1 << 20) / 8];
        let mut length: u32 = 0;
        let mut attempts = 0;
        loop {
            let capacity = words.len() * 8;
            let status = unsafe {
                NtQuerySystemInformation(
                    SystemProcessInformation,
                    words.as_mut_ptr().cast(),
                    u32::try_from(capacity).ok()?,
                    &mut length,
                )
            };
            if status.0 >= 0 {
                break;
            }
            attempts += 1;
            if status != STATUS_INFO_LENGTH_MISMATCH || attempts > 8 {
                return None;
            }
            // The process table can grow between calls: ask for a margin over the reported size.
            let want = (length as usize).checked_add(256 * 1024)?;
            if want <= capacity || want > MAX_QUERY_BYTES {
                return None;
            }
            words = vec![0; want.div_ceil(8)];
        }
        let valid = (length as usize).min(words.len() * 8);
        let base = words.as_ptr().cast::<u8>();
        let pid = unsafe { GetCurrentProcessId() } as usize;
        let thread_size = std::mem::size_of::<SYSTEM_THREAD_INFORMATION>();
        let mut offset = 0usize;
        loop {
            if offset.checked_add(THREADS_OFFSET)? > valid {
                return None;
            }
            // SAFETY: `offset + THREADS_OFFSET <= valid <= buffer size` covers the header read.
            let entry = unsafe {
                std::ptr::read_unaligned(base.add(offset).cast::<SYSTEM_PROCESS_INFORMATION>())
            };
            if entry.UniqueProcessId.0 as usize == pid {
                let count = entry.NumberOfThreads as usize;
                let first = offset + THREADS_OFFSET;
                let end = first.checked_add(count.checked_mul(thread_size)?)?;
                if end > valid {
                    return None;
                }
                let mut total = 0u64;
                for i in 0..count {
                    // SAFETY: `first + (i + 1) * thread_size <= end <= valid`.
                    let thread = unsafe {
                        std::ptr::read_unaligned(
                            base.add(first + i * thread_size)
                                .cast::<SYSTEM_THREAD_INFORMATION>(),
                        )
                    };
                    // Layout guard: every thread of this process names this process.
                    if thread.ClientId.UniqueProcess.0 as usize != pid {
                        return None;
                    }
                    total = total.checked_add(u64::from(thread.Reserved3))?;
                }
                return Some(total);
            }
            let next = entry.NextEntryOffset as usize;
            if next == 0 {
                return None;
            }
            offset = offset.checked_add(next)?;
        }
    }

    #[cfg(not(target_pointer_width = "64"))]
    fn wakeups_total() -> Option<u64> {
        None
    }

    pub fn sample() -> io::Result<ProcessUsage> {
        unsafe {
            let process = GetCurrentProcess();
            let mut mem = PROCESS_MEMORY_COUNTERS_EX::default();
            GetProcessMemoryInfo(
                process,
                (&mut mem as *mut PROCESS_MEMORY_COUNTERS_EX).cast::<PROCESS_MEMORY_COUNTERS>(),
                std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            )
            .map_err(os)?;
            let (mut created, mut exited, mut kernel, mut user) = (
                FILETIME::default(),
                FILETIME::default(),
                FILETIME::default(),
                FILETIME::default(),
            );
            GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user)
                .map_err(os)?;
            let mut io_counters = IO_COUNTERS::default();
            GetProcessIoCounters(process, &mut io_counters).map_err(os)?;
            Ok(ProcessUsage {
                resident_bytes: mem.WorkingSetSize as u64,
                footprint_bytes: mem.PrivateUsage as u64,
                // FILETIME ticks are 100 ns.
                cpu_seconds: (ticks(kernel) + ticks(user)) as f64 / 1e7,
                wakeups_total: wakeups_total(),
                disk_ops_total: Some(
                    io_counters.ReadOperationCount + io_counters.WriteOperationCount,
                ),
                disk_read_bytes: Some(io_counters.ReadTransferCount),
                disk_write_bytes: Some(io_counters.WriteTransferCount),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sampling exists where an implementation does: macOS, Linux, Android and Windows.
    #[cfg(any(
        target_os = "macos",
        target_os = "linux",
        target_os = "android",
        windows
    ))]
    #[test]
    fn sample_reports_resident_memory_and_cpu() {
        let first = process_usage().unwrap();
        assert!(first.resident_bytes > 0);
        assert!(first.footprint_bytes > 0);
        let mut sink = 0u64;
        for i in 0..20_000_000u64 {
            sink = sink.wrapping_add(i * 31);
        }
        std::hint::black_box(sink);
        let second = process_usage().unwrap();
        assert!(second.cpu_seconds >= first.cpu_seconds);
        let rates = second.rates_since(&first, Duration::from_secs(1));
        assert!(rates.cpu_percent >= 0.0);
    }

    /// Windows reports wake-ups as summed thread context switches: present, and not decreasing
    /// while the threads that did the waking are still alive.
    #[cfg(all(windows, target_pointer_width = "64"))]
    #[test]
    fn windows_wakeups_count_thread_context_switches() {
        use std::sync::{mpsc, Arc, Mutex};
        let (tx, rx) = mpsc::channel::<()>();
        let rx = Arc::new(Mutex::new(rx));
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let workers: Vec<_> = (0..3)
            .map(|_| {
                let rx = rx.clone();
                let done = done_tx.clone();
                std::thread::spawn(move || {
                    // Each message wakes one worker.
                    while rx.lock().map(|r| r.recv().is_ok()).unwrap_or(false) {
                        let _ = done.send(());
                    }
                })
            })
            .collect();
        // Other tests in this process may end threads between samples (their counts leave the
        // sum), so allow a few attempts for a non-decreasing pair.
        let mut ok = false;
        for _ in 0..5 {
            let first = process_usage()
                .unwrap()
                .wakeups_total
                .expect("windows reports wake-ups");
            for _ in 0..5 {
                std::thread::sleep(Duration::from_millis(10));
                tx.send(()).unwrap();
                done_rx.recv().unwrap();
            }
            let second = process_usage()
                .unwrap()
                .wakeups_total
                .expect("windows reports wake-ups");
            if second >= first {
                ok = true;
                break;
            }
        }
        drop(tx);
        for worker in workers {
            let _ = worker.join();
        }
        assert!(ok, "wake-up total decreased on every attempt");
    }

    /// Every other Unix target compiles and reports `Unsupported`, never a made-up sample.
    #[cfg(all(
        unix,
        not(target_os = "macos"),
        not(any(target_os = "linux", target_os = "android"))
    ))]
    #[test]
    fn unsupported_unix_targets_report_unsupported_not_a_sample() {
        let error = process_usage().expect_err("no sampler exists for this target");
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        let error = observe_idle(Duration::from_millis(1)).expect_err("idle needs sampling");
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    }

    #[test]
    fn rates_are_none_without_counters_and_zero_for_no_time() {
        let a = ProcessUsage {
            wakeups_total: None,
            ..Default::default()
        };
        let b = ProcessUsage {
            wakeups_total: Some(10),
            ..Default::default()
        };
        assert_eq!(
            b.rates_since(&a, Duration::from_secs(1)).wakeups_per_second,
            None
        );
        let c = ProcessUsage {
            wakeups_total: Some(30),
            ..Default::default()
        };
        assert_eq!(
            c.rates_since(&b, Duration::from_secs(2)).wakeups_per_second,
            Some(10.0)
        );
        assert_eq!(c.rates_since(&b, Duration::ZERO).cpu_percent, 0.0);
    }
}

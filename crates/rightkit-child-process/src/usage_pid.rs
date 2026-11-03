//! Resource usage of another process, by pid, and the NDJSON soak sampler built on it
//! (rightkit#403, rightkit#694).
//!
//! [`process_usage_of`] is [`process_usage`](crate::process_usage) for any pid the caller may
//! inspect (the same user's processes; a process of another user or one that denies the query is
//! `PermissionDenied`), plus the thread count. [`process_usage_of_identity`] does the same for an
//! [`AdoptedIdentity`] and refuses a pid that changed hands. [`SoakSampler`] and [`run_soak`]
//! write one JSON object per sample, one per line, for a long-running observation of an external
//! process (a footprint soak).
//!
//! | Figure | macOS | Windows | Linux, Android |
//! |---|---|---|---|
//! | `resident_bytes` | `proc_pid_rusage` `ri_resident_size` | `WorkingSetSize` | `/proc/<pid>/statm` |
//! | `footprint_bytes` (private / commit) | `ri_phys_footprint` | `PrivateUsage` | `Private_Clean + Private_Dirty` of `/proc/<pid>/smaps_rollup`, else resident |
//! | `cpu_seconds` | `ri_user_time + ri_system_time` (mach ticks converted with the timebase) | `GetProcessTimes` user + kernel | `/proc/<pid>/stat` `utime + stime` |
//! | `thread_count` | `PROC_PIDTASKINFO` `pti_threadnum` | Toolhelp process snapshot `cntThreads` | `/proc/<pid>/stat` `num_threads` |
//! | `wakeups_total` | `ri_interrupt_wkups` | not available | not available |
//! | `disk_*` | `ri_diskio_bytesread`/`written` | `GetProcessIoCounters` | `/proc/<pid>/io` `read_bytes`/`write_bytes` when readable |
//!
//! A figure the host cannot report is `None` (or, for the three fields `ProcessUsage` always
//! carries, the call fails): it is never reported as zero. Other Unix targets return
//! `ErrorKind::Unsupported`.

use crate::{AdoptedIdentity, IdentityCheck, ProcessUsage};
use std::{
    io::{self, Write},
    num::NonZeroU32,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// [`ProcessUsage`] of one process plus its thread count.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct PidUsage {
    /// The figures [`process_usage`](crate::process_usage) reports for the current process,
    /// read for this pid.
    pub usage: ProcessUsage,
    /// Live threads of the process; `None` when the host does not report it.
    pub thread_count: Option<u32>,
}

/// Resource usage of the process `pid`, computed now. An exited pid is `ErrorKind::NotFound`.
/// The pid alone is a name the kernel reuses: pass an [`AdoptedIdentity`] to
/// [`process_usage_of_identity`] when the number was remembered rather than just read.
pub fn process_usage_of(pid: NonZeroU32) -> io::Result<PidUsage> {
    imp::sample(pid.get())
}

/// [`process_usage_of`] for a remembered process: `Ok(None)` when the pid no longer names it
/// (gone or replaced), checked before and after the read so a figure that may belong to another
/// process is never returned.
pub fn process_usage_of_identity(identity: &AdoptedIdentity) -> io::Result<Option<PidUsage>> {
    if identity.verify()? != IdentityCheck::Same {
        return Ok(None);
    }
    let usage = match imp::sample(identity.pid()) {
        Ok(usage) => usage,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    Ok((identity.verify()? == IdentityCheck::Same).then_some(usage))
}

/// Takes [`PidUsage`] samples of one pid and renders each as one NDJSON line, with the CPU
/// percent since the previous sample.
#[derive(Debug)]
pub struct SoakSampler {
    pid: NonZeroU32,
    previous: Option<(Instant, f64)>,
}

impl SoakSampler {
    pub fn new(pid: NonZeroU32) -> Self {
        Self {
            pid,
            previous: None,
        }
    }

    /// One sample as a single-line JSON object (no trailing newline):
    /// `{"t_ms":<unix ms>,"pid":..,"resident_bytes":..,"footprint_bytes":..,"cpu_seconds":..,
    /// "cpu_percent":..,"thread_count":..,"wakeups_total":..,"disk_ops_total":..,
    /// "disk_read_bytes":..,"disk_write_bytes":..}`. Every figure is a number or `null`
    /// (unavailable, or `cpu_percent` on the first sample). `cpu_percent` is percent of one
    /// core since the previous call. Fails like [`process_usage_of`], so an exited process ends
    /// the soak with `ErrorKind::NotFound`.
    pub fn sample_line(&mut self) -> io::Result<String> {
        let sampled = process_usage_of(self.pid)?;
        let now = Instant::now();
        let usage = sampled.usage;
        let cpu_percent = self.previous.and_then(|(then, cpu)| {
            let secs = now.duration_since(then).as_secs_f64();
            (secs > 0.0).then(|| (usage.cpu_seconds - cpu).max(0.0) / secs * 100.0)
        });
        self.previous = Some((now, usage.cpu_seconds));
        let unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis());
        let number =
            |value: Option<u64>| value.map_or_else(|| "null".to_owned(), |v| v.to_string());
        let real = |value: Option<f64>| match value {
            Some(v) if v.is_finite() => format!("{v:.6}"),
            _ => "null".to_owned(),
        };
        Ok(format!(
            "{{\"t_ms\":{unix_ms},\"pid\":{},\"resident_bytes\":{},\"footprint_bytes\":{},\
             \"cpu_seconds\":{},\"cpu_percent\":{},\"thread_count\":{},\"wakeups_total\":{},\
             \"disk_ops_total\":{},\"disk_read_bytes\":{},\"disk_write_bytes\":{}}}",
            self.pid,
            usage.resident_bytes,
            usage.footprint_bytes,
            real(Some(usage.cpu_seconds)),
            real(cpu_percent),
            number(sampled.thread_count.map(u64::from)),
            number(usage.wakeups_total),
            number(usage.disk_ops_total),
            number(usage.disk_read_bytes),
            number(usage.disk_write_bytes),
        ))
    }
}

/// How a [`run_soak`] ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SoakEnd {
    /// `stop` was set.
    Stopped,
    /// The sampled process exited (or the pid stopped naming a readable process).
    ProcessGone,
    /// `max_samples` lines were written.
    SampleLimit,
}

/// Writes one NDJSON sample line of `pid` to `out` every `interval` (the first one at once),
/// flushing after each line, until `stop` is set, the process is gone, or `max_samples` lines
/// were written. Returns how it ended and the number of lines written. The wait between
/// samples is sliced to at most 100 ms so `stop` is honoured promptly. A write or flush failure,
/// or a sample failure other than the process being gone, is returned as `Err`.
pub fn run_soak(
    pid: NonZeroU32,
    interval: Duration,
    stop: &AtomicBool,
    max_samples: Option<u64>,
    out: &mut impl Write,
) -> io::Result<(SoakEnd, u64)> {
    let mut sampler = SoakSampler::new(pid);
    let mut written = 0u64;
    loop {
        if stop.load(Ordering::Acquire) {
            return Ok((SoakEnd::Stopped, written));
        }
        let began = Instant::now();
        let line = match sampler.sample_line() {
            Ok(line) => line,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok((SoakEnd::ProcessGone, written))
            }
            Err(error) => return Err(error),
        };
        out.write_all(line.as_bytes())?;
        out.write_all(b"\n")?;
        out.flush()?;
        written += 1;
        if max_samples.is_some_and(|limit| written >= limit) {
            return Ok((SoakEnd::SampleLimit, written));
        }
        let due = began + interval;
        loop {
            if stop.load(Ordering::Acquire) {
                return Ok((SoakEnd::Stopped, written));
            }
            let left = due.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            std::thread::sleep(left.min(Duration::from_millis(100)));
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::PidUsage;
    use crate::ProcessUsage;
    use std::io;

    extern "C" {
        fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut u8) -> i32;
    }

    #[repr(C)]
    struct TimebaseInfo {
        numer: u32,
        denom: u32,
    }
    extern "C" {
        fn mach_timebase_info(info: *mut TimebaseInfo) -> i32;
    }

    /// Words after the 16-byte uuid of `struct rusage_info_v2` (see `usage.rs`).
    const W_USER_TIME: usize = 2;
    const W_SYSTEM_TIME: usize = 3;
    const W_INTERRUPT_WKUPS: usize = 2 + 3;
    const W_RESIDENT: usize = 2 + 6;
    const W_FOOTPRINT: usize = 2 + 7;
    const W_DISK_READ: usize = 2 + 16;
    const W_DISK_WRITE: usize = 2 + 17;

    pub(super) fn sample(pid: u32) -> io::Result<PidUsage> {
        let pid = i32::try_from(pid)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "pid out of range"))?;
        let mut words = [0u64; 64];
        // SAFETY: `words` is 512 writable bytes, larger than rusage_info_v2; flavor 2 is V2.
        if unsafe { proc_pid_rusage(pid, 2, words.as_mut_ptr().cast()) } != 0 {
            let error = io::Error::last_os_error();
            return Err(if error.raw_os_error() == Some(libc::ESRCH) {
                io::Error::from(io::ErrorKind::NotFound)
            } else {
                error
            });
        }
        let mut timebase = TimebaseInfo { numer: 0, denom: 0 };
        // SAFETY: the out pointer is valid.
        if unsafe { mach_timebase_info(&mut timebase) } != 0 || timebase.denom == 0 {
            return Err(io::Error::other("mach_timebase_info failed"));
        }
        let nanos = |ticks: u64| {
            (u128::from(ticks) * u128::from(timebase.numer) / u128::from(timebase.denom)) as f64
        };
        let mut task: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_taskinfo>() as i32;
        // SAFETY: `task` is a writable `proc_taskinfo` of exactly `size` bytes.
        let read = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTASKINFO,
                0,
                (&mut task as *mut libc::proc_taskinfo).cast(),
                size,
            )
        };
        let thread_count = (read == size)
            .then(|| u32::try_from(task.pti_threadnum).ok())
            .flatten();
        Ok(PidUsage {
            usage: ProcessUsage {
                resident_bytes: words[W_RESIDENT],
                footprint_bytes: words[W_FOOTPRINT],
                cpu_seconds: (nanos(words[W_USER_TIME]) + nanos(words[W_SYSTEM_TIME])) / 1e9,
                wakeups_total: Some(words[W_INTERRUPT_WKUPS]),
                disk_ops_total: None,
                disk_read_bytes: Some(words[W_DISK_READ]),
                disk_write_bytes: Some(words[W_DISK_WRITE]),
            },
            thread_count,
        })
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod imp {
    use super::PidUsage;
    use crate::ProcessUsage;
    use std::io;

    fn read(pid: u32, file: &str) -> io::Result<String> {
        std::fs::read_to_string(format!("/proc/{pid}/{file}")).map_err(|error| {
            // A pid that exited reads as ENOENT or ESRCH.
            if error.raw_os_error() == Some(libc::ESRCH) {
                io::Error::from(io::ErrorKind::NotFound)
            } else {
                error
            }
        })
    }

    fn invalid(what: &'static str) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, what)
    }

    /// `Key:   123 kB` lines of a /proc status-style file, summed over `keys`.
    fn sum_kib(text: &str, keys: &[&str]) -> Option<u64> {
        let mut total = 0u64;
        let mut seen = false;
        for line in text.lines() {
            let Some((key, rest)) = line.split_once(':') else {
                continue;
            };
            if keys.contains(&key) {
                let kib: u64 = rest.split_whitespace().next()?.parse().ok()?;
                total = total.checked_add(kib.checked_mul(1024)?)?;
                seen = true;
            }
        }
        seen.then_some(total)
    }

    pub(super) fn sample(pid: u32) -> io::Result<PidUsage> {
        let statm = read(pid, "statm")?;
        let pages: u64 = statm
            .split_whitespace()
            .nth(1)
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| invalid("unreadable /proc/<pid>/statm"))?;
        // SAFETY: sysconf takes a plain constant.
        let page = u64::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(1))
            .map_err(|_| invalid("page size out of range"))?;
        let resident = pages
            .checked_mul(page)
            .ok_or_else(|| invalid("memory size overflows u64"))?;
        // `stat`: the command name is parenthesised and may hold spaces and parentheses, so
        // split after the last ')'. The first field after it is the state (field 3).
        let stat = read(pid, "stat")?;
        let after = stat
            .rsplit_once(')')
            .map(|(_, rest)| rest)
            .ok_or_else(|| invalid("unreadable /proc/<pid>/stat"))?;
        let fields: Vec<&str> = after.split_whitespace().collect();
        // fields[0] is field 3, so field n is fields[n - 3].
        let field = |n: usize| -> Option<u64> { fields.get(n - 3)?.parse().ok() };
        let ticks_per_second = {
            // SAFETY: sysconf takes a plain constant.
            let value = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
            u64::try_from(value).ok().filter(|v| *v > 0)
        }
        .ok_or_else(|| invalid("clock tick rate unavailable"))?;
        let cpu_ticks = field(14)
            .and_then(|user| user.checked_add(field(15)?))
            .ok_or_else(|| invalid("unreadable cpu time in /proc/<pid>/stat"))?;
        let thread_count = field(20).and_then(|n| u32::try_from(n).ok());
        let footprint = read(pid, "smaps_rollup")
            .ok()
            .and_then(|text| sum_kib(&text, &["Private_Clean", "Private_Dirty"]))
            .unwrap_or(resident);
        // `io` needs the same user and ptrace access; absent or denied is not an error.
        let (disk_read, disk_write) = match read(pid, "io") {
            Ok(text) => {
                let value = |key: &str| {
                    text.lines()
                        .find_map(|line| line.strip_prefix(key)?.trim().parse::<u64>().ok())
                };
                (value("read_bytes:"), value("write_bytes:"))
            }
            Err(_) => (None, None),
        };
        Ok(PidUsage {
            usage: ProcessUsage {
                resident_bytes: resident,
                footprint_bytes: footprint,
                cpu_seconds: cpu_ticks as f64 / ticks_per_second as f64,
                wakeups_total: None,
                disk_ops_total: None,
                disk_read_bytes: disk_read,
                disk_write_bytes: disk_write,
            },
            thread_count,
        })
    }
}

#[cfg(all(
    unix,
    not(target_os = "macos"),
    not(any(target_os = "linux", target_os = "android"))
))]
mod imp {
    use super::PidUsage;
    use std::io;

    pub(super) fn sample(_pid: u32) -> io::Result<PidUsage> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "per-pid usage sampling is implemented for macOS, Linux, Android and Windows only",
        ))
    }
}

#[cfg(windows)]
mod imp {
    use super::PidUsage;
    use crate::process_windows::os_error;
    use crate::ProcessUsage;
    use std::io;
    use windows::Win32::{
        Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, FILETIME, HANDLE},
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
                TH32CS_SNAPPROCESS,
            },
            ProcessStatus::{
                GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
            },
            Threading::{
                GetProcessIoCounters, GetProcessTimes, OpenProcess, IO_COUNTERS,
                PROCESS_QUERY_LIMITED_INFORMATION,
            },
        },
    };

    /// A kernel handle closed on drop.
    struct Owned(HANDLE);
    impl Drop for Owned {
        fn drop(&mut self) {
            // SAFETY: the handle came from a successful open or snapshot and is closed once.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }

    /// Thread count of `pid` from one process snapshot; `None` when the snapshot or the entry
    /// is unavailable.
    fn thread_count(pid: u32) -> Option<u32> {
        // SAFETY: a snapshot request takes plain values.
        let snapshot = Owned(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }.ok()?);
        // SAFETY: PROCESSENTRY32W is plain data; dwSize is set as the API requires.
        let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        // SAFETY: the snapshot is open and `entry` is a valid, sized entry.
        unsafe { Process32FirstW(snapshot.0, &mut entry) }.ok()?;
        loop {
            if entry.th32ProcessID == pid {
                return Some(entry.cntThreads);
            }
            // SAFETY: as above.
            if unsafe { Process32NextW(snapshot.0, &mut entry) }.is_err() {
                return None;
            }
        }
    }

    pub(super) fn sample(pid: u32) -> io::Result<PidUsage> {
        // SAFETY: OpenProcess takes plain values and returns a new handle.
        let process = match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
            Ok(handle) => Owned(handle),
            Err(error) if error.code() == ERROR_INVALID_PARAMETER.to_hresult() => {
                return Err(io::Error::from(io::ErrorKind::NotFound))
            }
            Err(error) => return Err(os_error(error)),
        };
        let mut mem = PROCESS_MEMORY_COUNTERS_EX::default();
        // SAFETY: the handle is open for query; the buffer is a sized PROCESS_MEMORY_COUNTERS_EX
        // (its first member is the PROCESS_MEMORY_COUNTERS the call fills).
        unsafe {
            GetProcessMemoryInfo(
                process.0,
                (&mut mem as *mut PROCESS_MEMORY_COUNTERS_EX).cast::<PROCESS_MEMORY_COUNTERS>(),
                std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            )
        }
        .map_err(os_error)?;
        let (mut created, mut exited, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        // SAFETY: the handle is open for query; the outputs are valid.
        unsafe { GetProcessTimes(process.0, &mut created, &mut exited, &mut kernel, &mut user) }
            .map_err(os_error)?;
        let ticks = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
        let mut counters = IO_COUNTERS::default();
        // SAFETY: the output is valid. Documented as needing more than limited access, so a
        // refusal is an unavailable figure, not a failure of the sample.
        let io_known = unsafe { GetProcessIoCounters(process.0, &mut counters) }.is_ok();
        Ok(PidUsage {
            usage: ProcessUsage {
                resident_bytes: mem.WorkingSetSize as u64,
                footprint_bytes: mem.PrivateUsage as u64,
                // FILETIME ticks are 100 ns.
                cpu_seconds: (ticks(kernel) + ticks(user)) as f64 / 1e7,
                wakeups_total: None,
                disk_ops_total: io_known
                    .then(|| counters.ReadOperationCount + counters.WriteOperationCount),
                disk_read_bytes: io_known.then_some(counters.ReadTransferCount),
                disk_write_bytes: io_known.then_some(counters.WriteTransferCount),
            },
            thread_count: thread_count(pid),
        })
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use super::PidUsage;
    use std::io;

    pub(super) fn sample(_pid: u32) -> io::Result<PidUsage> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

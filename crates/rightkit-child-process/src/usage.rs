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
//! [`process_usage_with_threads`] and [`observe_idle_with_threads`] add a per-thread breakdown for
//! finding which thread causes wake-ups. On Windows the per-thread `ContextSwitchCount` values come
//! from the very table the total is summed from, so their sum is exactly `wakeups_total`. macOS and
//! Linux list the threads (id and name) but report no per-thread wake-up counter (`None`): the macOS
//! figure is a process-level interrupt count that does not decompose by thread.
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

/// Resident memory summed over one remembered process and the descendants found while it
/// lived. Members are counted only while their pid-plus-start-time identity still verifies
/// after the table snapshot, so a pid recycled during the scan contributes nothing.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TreeUsage {
    /// The root plus descendants that were still the remembered processes at recheck.
    pub processes: usize,
    /// Summed resident (working-set) bytes of the members the OS reported a figure for.
    pub resident_bytes: u64,
    /// Members still present but with no readable resident figure.
    pub unreadable: usize,
}

/// [`TreeUsage`] for `root` and its current descendants (see
/// [`AdoptedIdentity::descendants`](crate::AdoptedIdentity::descendants) for which
/// descendants can be found). A root that is gone or replaced yields an empty usage, not
/// an error. Member figures come from one [`process_table`](crate::process_table)
/// snapshot, then every member is verified again before it is counted.
pub fn tree_usage(root: &crate::AdoptedIdentity) -> io::Result<TreeUsage> {
    if root.verify()? != crate::IdentityCheck::Same {
        return Ok(TreeUsage::default());
    }
    let mut members = vec![root.clone()];
    members.extend(root.descendants()?);
    let table = crate::process_table()?;
    let mut usage = TreeUsage::default();
    for member in &members {
        if member.verify()? != crate::IdentityCheck::Same {
            continue;
        }
        usage.processes += 1;
        match table
            .iter()
            .find(|entry| entry.pid == member.pid())
            .and_then(|entry| entry.working_set_bytes)
        {
            Some(bytes) => usage.resident_bytes = usage.resident_bytes.saturating_add(bytes),
            None => usage.unreadable += 1,
        }
    }
    Ok(usage)
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

/// One thread of the current process at the moment of a snapshot.
///
/// A figure the host cannot report for a thread is `None`, never zero.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct ThreadSample {
    /// OS thread id: the Windows thread id, the Linux tid, or the macOS libproc thread handle.
    pub thread_id: u64,
    /// Creation time (Windows FILETIME ticks). Thread ids are reused, so `(thread_id, started)` is
    /// what identifies a thread across snapshots. `None` where the host reports no creation time.
    pub started: Option<u64>,
    /// Wake-ups since the thread started: Windows `ContextSwitchCount` (one switch-in per wake).
    /// `None` on macOS and Linux.
    pub wakeups: Option<u64>,
    /// CPU time (user plus kernel) since the thread started, in seconds. Windows only.
    pub cpu_seconds: Option<f64>,
    /// Thread name when the thread set one (Windows `SetThreadDescription`, Linux `comm`, macOS
    /// `pth_name`); `None` when it has none or the thread was gone before it could be read.
    pub name: Option<String>,
    /// Windows Win32 start address of the thread (the routine handed to `CreateThread`, which for
    /// a Rust thread is the std trampoline, so it tells a pool from a one-off but not the closure);
    /// `None` elsewhere or when unreadable.
    pub start_address: Option<u64>,
}

/// A [`ProcessUsage`] together with the threads it was summed from.
#[derive(Clone, Debug, PartialEq)]
pub struct ProcessThreads {
    pub usage: ProcessUsage,
    /// Live threads of this process. On Windows `usage.wakeups_total` is exactly the sum of their
    /// `wakeups` (see [`ProcessThreads::thread_wakeups_sum`]).
    pub threads: Vec<ThreadSample>,
}

impl ProcessThreads {
    /// Sum of the per-thread wake-up counters; `None` when any thread lacks one.
    pub fn thread_wakeups_sum(&self) -> Option<u64> {
        sum_wakeups(&self.threads)
    }
}

fn sum_wakeups(threads: &[ThreadSample]) -> Option<u64> {
    threads
        .iter()
        .try_fold(0u64, |total, thread| total.checked_add(thread.wakeups?))
}

/// Resource usage of the current process and its per-thread breakdown, from one snapshot.
///
/// More expensive than [`process_usage`] (it opens each thread to read its name and start
/// address), so it is for benches and diagnosis. A thread that exits while the names are read
/// keeps its counters and has no name. Fails when the host cannot list threads (Windows when the
/// process table is unreadable, other Unix targets with `ErrorKind::Unsupported`).
pub fn process_usage_with_threads() -> io::Result<ProcessThreads> {
    let (usage, mut threads) = imp::sample_threads()?;
    imp::enrich(&mut threads);
    Ok(ProcessThreads { usage, threads })
}

/// What one thread did between two snapshots.
#[derive(Clone, Debug, PartialEq)]
pub struct ThreadDelta {
    pub thread_id: u64,
    /// Wake-ups in the window; `None` when either snapshot lacks the counter.
    pub wakeups: Option<u64>,
    /// CPU seconds in the window; `None` when either snapshot lacks it.
    pub cpu_seconds: Option<f64>,
    pub name: Option<String>,
    pub start_address: Option<u64>,
    /// The thread was not in the first snapshot: it began inside the window, so its whole count
    /// (and CPU time) is the window's.
    pub started_in_window: bool,
}

/// Per-thread change between two snapshots (see [`thread_deltas`]).
#[derive(Clone, Debug, PartialEq)]
pub struct ThreadDeltas {
    /// Threads present in the second snapshot, most wake-ups first (unknown counts last, ties by
    /// thread id).
    pub threads: Vec<ThreadDelta>,
    /// Threads in the first snapshot that were gone by the second (as they were at the first).
    /// Their wake-ups inside the window are unknowable; that is a normal outcome, not an error.
    pub exited: Vec<ThreadSample>,
}

/// Per-thread deltas from `start` to `end`. Threads are matched by `(thread_id, started)`, so a
/// reused id is a new thread, not a continuation. A counter that went backwards counts as zero.
pub fn thread_deltas(start: &[ThreadSample], end: &[ThreadSample]) -> ThreadDeltas {
    use std::collections::HashMap;
    let key = |thread: &ThreadSample| (thread.thread_id, thread.started);
    let mut earlier: HashMap<(u64, Option<u64>), &ThreadSample> =
        start.iter().map(|thread| (key(thread), thread)).collect();
    let mut threads: Vec<ThreadDelta> = end
        .iter()
        .map(|now| {
            let before = earlier.remove(&key(now));
            ThreadDelta {
                thread_id: now.thread_id,
                wakeups: match before {
                    Some(then) => counter_delta(now.wakeups, then.wakeups),
                    None => now.wakeups,
                },
                cpu_seconds: match before {
                    Some(then) => match (now.cpu_seconds, then.cpu_seconds) {
                        (Some(now), Some(then)) => Some((now - then).max(0.0)),
                        _ => None,
                    },
                    None => now.cpu_seconds,
                },
                name: now
                    .name
                    .clone()
                    .or_else(|| before.and_then(|then| then.name.clone())),
                start_address: now
                    .start_address
                    .or_else(|| before.and_then(|then| then.start_address)),
                started_in_window: before.is_none(),
            }
        })
        .collect();
    threads.sort_by(|a, b| {
        b.wakeups
            .cmp(&a.wakeups)
            .then(a.thread_id.cmp(&b.thread_id))
    });
    let mut exited: Vec<ThreadSample> = earlier.into_values().cloned().collect();
    exited.sort_by_key(|thread| thread.thread_id);
    ThreadDeltas { threads, exited }
}

fn counter_delta(now: Option<u64>, then: Option<u64>) -> Option<u64> {
    Some(now?.saturating_sub(then?))
}

impl ThreadDeltas {
    /// Sum of the listed threads' wake-ups; `None` when any is unknown. Minus the exited threads'
    /// counts at the first snapshot ([`ThreadDeltas::exited_wakeups_at_start`]) it equals the
    /// whole-process wake-up delta between the same two snapshots.
    pub fn total_wakeups(&self) -> Option<u64> {
        self.threads
            .iter()
            .try_fold(0u64, |total, thread| total.checked_add(thread.wakeups?))
    }

    /// Sum of the exited threads' wake-up counters at the first snapshot; `None` when unknown.
    pub fn exited_wakeups_at_start(&self) -> Option<u64> {
        sum_wakeups(&self.exited)
    }

    /// The `n` threads with the most wake-ups.
    pub fn top(&self, n: usize) -> &[ThreadDelta] {
        &self.threads[..n.min(self.threads.len())]
    }

    /// Evidence lines for the top `top` threads over a window of `elapsed`, in the
    /// `RESULT <key> ...` style: one `RESULT thread_wakeups_summary ...` line, then one
    /// `RESULT thread_wakeups rank=.. tid=.. wakeups=.. per_second=.. cpu_ms=.. born=0|1
    /// start=0x.. name=".."` line per thread with a non-zero or unknown delta. Neither key is
    /// `budget`, so the runtime-budget gate (which reads only the `RESULT budget` line) ignores them.
    pub fn result_lines(&self, elapsed: Duration, top: usize) -> String {
        let secs = elapsed.as_secs_f64();
        let count = |v: Option<u64>| v.map_or_else(|| "unavailable".to_string(), |v| v.to_string());
        let mut out = format!(
            "RESULT thread_wakeups_summary threads={} born={} exited={} listed_wakeups={} exited_wakeups_at_start={} window_seconds={:.1}\n",
            self.threads.len(),
            self.threads.iter().filter(|t| t.started_in_window).count(),
            self.exited.len(),
            count(self.total_wakeups()),
            count(self.exited_wakeups_at_start()),
            secs,
        );
        let listed = self
            .threads
            .iter()
            .filter(|t| t.wakeups != Some(0))
            .take(top);
        for (index, thread) in listed.enumerate() {
            let per_second = match thread.wakeups {
                Some(w) if secs > 0.0 => format!("{:.3}", w as f64 / secs),
                Some(_) => "0.000".to_string(),
                None => "unavailable".to_string(),
            };
            out.push_str(&format!(
                "RESULT thread_wakeups rank={} tid={} wakeups={} per_second={per_second} cpu_ms={} born={} start={} name={}\n",
                index + 1,
                thread.thread_id,
                count(thread.wakeups),
                thread
                    .cpu_seconds
                    .map_or_else(|| "unavailable".to_string(), |s| format!("{:.1}", s * 1e3)),
                u8::from(thread.started_in_window),
                thread
                    .start_address
                    .map_or_else(|| "unavailable".to_string(), |a| format!("0x{a:x}")),
                thread
                    .name
                    .as_ref()
                    .map_or_else(|| "unavailable".to_string(), |n| format!("{n:?}")),
            ));
        }
        out
    }
}

/// An [`IdleObservation`] plus what each thread did over the same window.
#[derive(Clone, Debug)]
pub struct IdleThreadObservation {
    /// Rates are computed from the same two snapshots the thread deltas come from.
    pub observation: IdleObservation,
    pub threads: ThreadDeltas,
}

/// [`observe_idle`] that also records per-thread deltas, so a wake-up overage can be attributed to
/// threads. The thread snapshots and the process figures are the same two samples; names and
/// start addresses are read after each sample, outside the measured counts.
pub fn observe_idle_with_threads(duration: Duration) -> io::Result<IdleThreadObservation> {
    let (start, mut start_threads) = imp::sample_threads()?;
    let began = std::time::Instant::now();
    imp::enrich(&mut start_threads);
    std::thread::sleep(duration.saturating_sub(began.elapsed()));
    let (end, mut end_threads) = imp::sample_threads()?;
    let elapsed = began.elapsed();
    imp::enrich(&mut end_threads);
    Ok(IdleThreadObservation {
        observation: IdleObservation {
            elapsed,
            start,
            end,
            rates: end.rates_since(&start, elapsed),
        },
        threads: thread_deltas(&start_threads, &end_threads),
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
    use super::{ProcessUsage, ThreadSample};
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

    /// `PROC_PIDLISTTHREADS` from libproc.h: the thread handles of a process (`libc` does not
    /// export it; `PROC_PIDTHREADINFO` is the flavor that takes one of those handles).
    const PROC_PIDLISTTHREADS: i32 = 6;
    /// Upper bound on the thread handles read (a process with more is refused, not truncated).
    const MAX_THREADS: usize = 1 << 16;

    /// The live threads of this process with their names. libproc offers no per-thread wake-up
    /// counter, so `wakeups` is `None`. A thread whose info cannot be read (it exited after the
    /// listing) is left out: it counts as exited, not as an error.
    fn threads() -> io::Result<Vec<ThreadSample>> {
        let pid = unsafe { libc::getpid() };
        let mut capacity = 64usize;
        let handles = loop {
            let mut handles = vec![0u64; capacity];
            let bytes = i32::try_from(capacity * std::mem::size_of::<u64>())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "thread list too large"))?;
            // SAFETY: `handles` is a writable buffer of exactly `bytes` bytes.
            let written = unsafe {
                libc::proc_pidinfo(
                    pid,
                    PROC_PIDLISTTHREADS,
                    0,
                    handles.as_mut_ptr().cast(),
                    bytes,
                )
            };
            if written <= 0 {
                return Err(io::Error::last_os_error());
            }
            let count = written as usize / std::mem::size_of::<u64>();
            // A full buffer may have been truncated: grow and ask again.
            if count < capacity {
                handles.truncate(count);
                break handles;
            }
            capacity *= 2;
            if capacity > MAX_THREADS {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "too many threads to list",
                ));
            }
        };
        let size = std::mem::size_of::<libc::proc_threadinfo>() as i32;
        let mut threads = Vec::with_capacity(handles.len());
        for handle in handles {
            let mut info: libc::proc_threadinfo = unsafe { std::mem::zeroed() };
            // SAFETY: `info` is a writable `proc_threadinfo` of exactly `size` bytes.
            let read = unsafe {
                libc::proc_pidinfo(
                    pid,
                    libc::PROC_PIDTHREADINFO,
                    handle,
                    (&mut info as *mut libc::proc_threadinfo).cast(),
                    size,
                )
            };
            if read != size {
                continue;
            }
            let bytes: Vec<u8> = info
                .pth_name
                .iter()
                .map(|&c| c as u8)
                .take_while(|&c| c != 0)
                .collect();
            let name = String::from_utf8_lossy(&bytes).into_owned();
            threads.push(ThreadSample {
                thread_id: handle,
                started: None,
                wakeups: None,
                cpu_seconds: None,
                name: (!name.is_empty()).then_some(name),
                start_address: None,
            });
        }
        Ok(threads)
    }

    /// The process figures, then the thread list (two reads, not one atomic snapshot: macOS
    /// reports no per-thread counter for them to reconcile with).
    pub fn sample_threads() -> io::Result<(ProcessUsage, Vec<ThreadSample>)> {
        let usage = sample()?;
        Ok((usage, threads()?))
    }

    /// Names are read while listing; nothing more is available.
    pub fn enrich(_threads: &mut [ThreadSample]) {}
}

#[cfg(all(
    unix,
    not(target_os = "macos"),
    not(any(target_os = "linux", target_os = "android"))
))]
mod imp {
    use super::{ProcessUsage, ThreadSample};
    use std::io;

    /// Chosen when the crate is compiled for a Unix this module has no sampler for: an
    /// explicit, typed error rather than a procfs read that may not exist.
    pub fn sample() -> io::Result<ProcessUsage> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "process usage sampling is implemented for macOS, Linux, Android and Windows only",
        ))
    }

    pub fn sample_threads() -> io::Result<(ProcessUsage, Vec<ThreadSample>)> {
        sample().map(|usage| (usage, Vec::new()))
    }

    pub fn enrich(_threads: &mut [ThreadSample]) {}
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod imp {
    use super::{ProcessUsage, ThreadSample};
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

    /// The live threads (`/proc/self/task`) with their `comm` names. No per-thread wake-up
    /// counter is reported (the process total is `None` here too). A thread that exits while
    /// the directory is walked is skipped: it counts as exited, not as an error.
    fn threads() -> io::Result<Vec<ThreadSample>> {
        let mut threads = Vec::new();
        for entry in std::fs::read_dir("/proc/self/task")? {
            let Ok(entry) = entry else { continue };
            let Some(tid) = entry
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<u64>().ok())
            else {
                continue;
            };
            let Ok(comm) = std::fs::read_to_string(entry.path().join("comm")) else {
                continue;
            };
            let name = comm.trim_end().to_string();
            threads.push(ThreadSample {
                thread_id: tid,
                started: None,
                wakeups: None,
                cpu_seconds: None,
                name: (!name.is_empty()).then_some(name),
                start_address: None,
            });
        }
        Ok(threads)
    }

    pub fn sample_threads() -> io::Result<(ProcessUsage, Vec<ThreadSample>)> {
        let usage = sample()?;
        Ok((usage, threads()?))
    }

    /// Names are read while listing; nothing more is available.
    pub fn enrich(_threads: &mut [ThreadSample]) {}
}

#[cfg(windows)]
mod imp {
    use super::{ProcessUsage, ThreadSample};
    use std::io;
    use windows::Wdk::System::SystemInformation::{
        NtQuerySystemInformation, SystemProcessInformation,
    };
    use windows::Win32::Foundation::{
        CloseHandle, LocalFree, FILETIME, HANDLE, HLOCAL, STATUS_INFO_LENGTH_MISMATCH,
    };
    use windows::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows::Win32::System::Threading::{
        GetCurrentProcess, GetCurrentProcessId, GetProcessIoCounters, GetProcessTimes,
        GetThreadDescription, GetThreadTimes, OpenThread, IO_COUNTERS, THREAD_QUERY_INFORMATION,
        THREAD_QUERY_LIMITED_INFORMATION,
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

    /// Every live thread of this process from one `SystemProcessInformation` snapshot, with the
    /// counters that snapshot holds. `ContextSwitchCount` is the `u32` the crate names `Reserved3`
    /// in `SYSTEM_THREAD_INFORMATION` (offset 0x40, right after `BasePriority`); the two times and
    /// the creation time are the three `i64` the crate names `Reserved1` (kernel, user, create).
    /// `wakeups` is always `Some` here. `None` on any failure or unexpected layout; never panics.
    /// The total wake-up figure is the sum over this same vector, so the two can never disagree.
    #[cfg(target_pointer_width = "64")]
    fn thread_table() -> Option<Vec<ThreadSample>> {
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
                let mut threads = Vec::with_capacity(count);
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
                    let [kernel, user, created] = thread.Reserved1;
                    threads.push(ThreadSample {
                        thread_id: thread.ClientId.UniqueThread.0 as usize as u64,
                        started: u64::try_from(created).ok(),
                        wakeups: Some(u64::from(thread.Reserved3)),
                        // FILETIME-style ticks are 100 ns.
                        cpu_seconds: Some(
                            (kernel.max(0) as u64).saturating_add(user.max(0) as u64) as f64 / 1e7,
                        ),
                        name: None,
                        start_address: None,
                    });
                }
                return Some(threads);
            }
            let next = entry.NextEntryOffset as usize;
            if next == 0 {
                return None;
            }
            offset = offset.checked_add(next)?;
        }
    }

    #[cfg(not(target_pointer_width = "64"))]
    fn thread_table() -> Option<Vec<ThreadSample>> {
        None
    }

    /// Closes the thread handle it owns, exactly once.
    struct ThreadHandle(HANDLE);

    impl Drop for ThreadHandle {
        fn drop(&mut self) {
            // SAFETY: the handle came from `OpenThread` and is closed only here.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }

    /// Opens a thread for querying. A thread that exited since the snapshot cannot be opened:
    /// that is a normal outcome (`None`), not an error. Asks for `THREAD_QUERY_INFORMATION`
    /// (the start-address query needs it) and falls back to the limited right (enough for the
    /// description) when that is denied.
    fn open_thread(id: u32) -> Option<ThreadHandle> {
        let full = THREAD_QUERY_INFORMATION | THREAD_QUERY_LIMITED_INFORMATION;
        // SAFETY: plain handle-returning calls; a result is wrapped so it is closed.
        let opened = unsafe { OpenThread(full, false, id) }
            .or_else(|_| unsafe { OpenThread(THREAD_QUERY_LIMITED_INFORMATION, false, id) });
        opened.ok().map(ThreadHandle)
    }

    /// Creation time of an open thread in FILETIME ticks.
    fn thread_created(thread: HANDLE) -> Option<u64> {
        let (mut created, mut exited, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        // SAFETY: four valid out-pointers; `thread` is an open handle.
        let read =
            unsafe { GetThreadTimes(thread, &mut created, &mut exited, &mut kernel, &mut user) };
        read.ok()?;
        Some(ticks(created))
    }

    /// The name a thread gave itself (`SetThreadDescription`), or `None`.
    fn thread_name(thread: HANDLE) -> Option<String> {
        // SAFETY: `thread` is open with `THREAD_QUERY_LIMITED_INFORMATION`.
        let description = unsafe { GetThreadDescription(thread) }.ok()?;
        if description.is_null() {
            return None;
        }
        // SAFETY: a non-null result is a NUL-terminated UTF-16 string allocated for us.
        let name = unsafe { description.to_string() }.ok();
        // SAFETY: the string must be released with `LocalFree`, once.
        unsafe { LocalFree(Some(HLOCAL(description.0.cast()))) };
        name.filter(|name| !name.is_empty())
    }

    /// `ThreadQuerySetWin32StartAddress`, the `NtQueryInformationThread` class that returns the
    /// routine handed to `CreateThread`. The system process table's own start address is the
    /// kernel-side one (`RtlUserThreadStart` for every thread) and says nothing.
    #[cfg(target_pointer_width = "64")]
    const THREAD_QUERY_SET_WIN32_START_ADDRESS: u32 = 9;

    // Declared here rather than taken from the `windows` crate, whose `Wdk_System_Threading`
    // feature this crate does not enable (std links `ntdll` already on every Windows target).
    #[cfg(target_pointer_width = "64")]
    #[link(name = "ntdll")]
    extern "system" {
        fn NtQueryInformationThread(
            thread: HANDLE,
            class: u32,
            information: *mut std::ffi::c_void,
            length: u32,
            returned: *mut u32,
        ) -> i32;
    }

    #[cfg(target_pointer_width = "64")]
    fn win32_start_address(thread: HANDLE) -> Option<u64> {
        let mut address: usize = 0;
        // SAFETY: `address` is a writable pointer-sized out value and the length says so.
        let status = unsafe {
            NtQueryInformationThread(
                thread,
                THREAD_QUERY_SET_WIN32_START_ADDRESS,
                (&mut address as *mut usize).cast(),
                std::mem::size_of::<usize>() as u32,
                std::ptr::null_mut(),
            )
        };
        (status >= 0 && address != 0).then_some(address as u64)
    }

    #[cfg(not(target_pointer_width = "64"))]
    fn win32_start_address(_thread: HANDLE) -> Option<u64> {
        None
    }

    /// The process figures and the thread table from one table read: `wakeups_total` is the sum
    /// of the returned threads' `wakeups`. Fails when the table cannot be read.
    pub fn sample_threads() -> io::Result<(ProcessUsage, Vec<ThreadSample>)> {
        let threads = thread_table().ok_or_else(|| io::Error::other("thread table unavailable"))?;
        let usage = sample_with(super::sum_wakeups(&threads))?;
        Ok((usage, threads))
    }

    /// Fills each thread's name and Win32 start address by opening it. A thread that exited
    /// since the snapshot, was replaced by a new one with the same id (its creation time no
    /// longer matches), or cannot be opened keeps `None` there; its counters are untouched.
    pub fn enrich(threads: &mut [ThreadSample]) {
        for thread in threads.iter_mut() {
            let Some(handle) = u32::try_from(thread.thread_id).ok().and_then(open_thread) else {
                continue;
            };
            if thread.started.is_some() && thread_created(handle.0) != thread.started {
                continue;
            }
            thread.name = thread_name(handle.0);
            thread.start_address = win32_start_address(handle.0);
        }
    }

    pub fn sample() -> io::Result<ProcessUsage> {
        let table = thread_table();
        sample_with(table.as_deref().and_then(super::sum_wakeups))
    }

    fn sample_with(wakeups_total: Option<u64>) -> io::Result<ProcessUsage> {
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
                wakeups_total,
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

    #[cfg(any(
        target_os = "macos",
        target_os = "linux",
        target_os = "android",
        windows
    ))]
    #[test]
    fn tree_usage_counts_the_remembered_root_and_refuses_a_replaced_one() {
        let pid = std::num::NonZeroU32::new(std::process::id()).unwrap();
        let root = crate::AdoptedIdentity::capture(pid)
            .unwrap()
            .expect("this process has an identity");
        let usage = tree_usage(&root).unwrap();
        assert!(usage.processes >= 1, "{usage:?}");
        assert!(usage.resident_bytes > 0, "{usage:?}");
        // Same pid, a start token that is not this process's: counted as nobody.
        let replaced = crate::AdoptedIdentity::from_recorded(
            pid,
            crate::StartToken {
                major: root.start_token().major.wrapping_add(1),
                minor: root.start_token().minor,
            },
            None,
        );
        assert_eq!(tree_usage(&replaced).unwrap(), TreeUsage::default());
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

    fn thread(id: u64, started: u64, wakeups: Option<u64>) -> ThreadSample {
        ThreadSample {
            thread_id: id,
            started: Some(started),
            wakeups,
            cpu_seconds: wakeups.map(|w| w as f64 / 1000.0),
            name: None,
            start_address: None,
        }
    }

    #[test]
    fn deltas_match_by_identity_sort_by_wakeups_and_list_exited_threads() {
        let start = [
            thread(1, 100, Some(10)),
            thread(2, 200, Some(50)),
            thread(3, 300, Some(7)),
        ];
        let end = [
            thread(1, 100, Some(14)),
            thread(2, 200, Some(51)),
            thread(4, 400, Some(30)),
        ];
        let deltas = thread_deltas(&start, &end);
        let listed: Vec<(u64, Option<u64>, bool)> = deltas
            .threads
            .iter()
            .map(|d| (d.thread_id, d.wakeups, d.started_in_window))
            .collect();
        // Thread 4 began inside the window: its whole count is the window's.
        assert_eq!(
            listed,
            vec![
                (4, Some(30), true),
                (1, Some(4), false),
                (2, Some(1), false),
            ]
        );
        assert_eq!(deltas.exited.len(), 1);
        assert_eq!(deltas.exited[0].thread_id, 3);
        assert_eq!(deltas.total_wakeups(), Some(35));
        assert_eq!(deltas.exited_wakeups_at_start(), Some(7));
        // Listed minus exited-at-start is the whole-process delta between the same snapshots.
        assert_eq!(
            deltas.total_wakeups().unwrap() - deltas.exited_wakeups_at_start().unwrap(),
            sum_wakeups(&end).unwrap() - sum_wakeups(&start).unwrap()
        );
        let cpu = deltas.threads.iter().find(|d| d.thread_id == 1).unwrap();
        assert!((cpu.cpu_seconds.unwrap() - 0.004).abs() < 1e-9);
        assert_eq!(deltas.top(2).len(), 2);
        assert_eq!(deltas.top(99).len(), 3);
        assert_eq!(deltas.top(0).len(), 0);
    }

    #[test]
    fn a_reused_thread_id_with_a_new_creation_time_is_a_new_thread() {
        let start = [thread(9, 100, Some(500))];
        let end = [thread(9, 999, Some(3))];
        let deltas = thread_deltas(&start, &end);
        assert_eq!(deltas.threads.len(), 1);
        assert_eq!(deltas.threads[0].wakeups, Some(3));
        assert!(deltas.threads[0].started_in_window);
        assert_eq!(deltas.exited.len(), 1);
        assert_eq!(deltas.exited_wakeups_at_start(), Some(500));
    }

    #[test]
    fn a_counter_that_went_backwards_counts_as_zero_not_a_wrapped_value() {
        let deltas = thread_deltas(&[thread(1, 1, Some(10))], &[thread(1, 1, Some(4))]);
        assert_eq!(deltas.threads[0].wakeups, Some(0));
        assert_eq!(deltas.total_wakeups(), Some(0));
    }

    #[test]
    fn unknown_counters_stay_unknown_and_sort_last() {
        let start = [thread(1, 1, None), thread(2, 2, Some(1))];
        let end = [thread(1, 1, None), thread(2, 2, Some(5))];
        let deltas = thread_deltas(&start, &end);
        assert_eq!(deltas.threads[0].thread_id, 2);
        assert_eq!(deltas.threads[0].wakeups, Some(4));
        assert_eq!(deltas.threads[1].wakeups, None);
        assert_eq!(deltas.threads[1].cpu_seconds, None);
        assert_eq!(deltas.total_wakeups(), None);
        let none = ThreadDeltas {
            threads: Vec::new(),
            exited: Vec::new(),
        };
        assert_eq!(none.total_wakeups(), Some(0));
        assert_eq!(
            sum_wakeups(&[thread(1, 1, Some(2)), thread(2, 2, None)]),
            None
        );
        assert_eq!(
            sum_wakeups(&[thread(1, 1, Some(2)), thread(2, 2, Some(3))]),
            Some(5)
        );
        assert_eq!(
            sum_wakeups(&[thread(1, 1, Some(u64::MAX)), thread(2, 2, Some(1))]),
            None
        );
    }

    #[test]
    fn a_name_or_address_read_only_at_one_end_is_kept() {
        let mut first = thread(1, 1, Some(1));
        first.name = Some("worker".to_string());
        let mut last = thread(1, 1, Some(2));
        last.start_address = Some(0x1000);
        let deltas = thread_deltas(&[first], &[last]);
        assert_eq!(deltas.threads[0].name.as_deref(), Some("worker"));
        assert_eq!(deltas.threads[0].start_address, Some(0x1000));
    }

    #[test]
    fn thread_result_lines_are_not_budget_lines_and_list_only_active_threads() {
        let mut busy = thread(7, 1, Some(100));
        busy.name = Some("tokio-runtime-worker".to_string());
        busy.start_address = Some(0x7ff6_1234);
        let deltas = thread_deltas(
            &[thread(7, 1, Some(0)), thread(8, 2, Some(5))],
            &[busy, thread(8, 2, Some(5))],
        );
        let text = deltas.result_lines(Duration::from_secs(50), 5);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert!(lines
            .iter()
            .all(|line| line.starts_with("RESULT thread_wakeups") && !line.contains("budget")));
        assert_eq!(
            lines[0],
            "RESULT thread_wakeups_summary threads=2 born=0 exited=0 listed_wakeups=100 exited_wakeups_at_start=0 window_seconds=50.0"
        );
        assert!(lines[1]
            .starts_with("RESULT thread_wakeups rank=1 tid=7 wakeups=100 per_second=2.000 "));
        assert!(lines[1].ends_with(" born=0 start=0x7ff61234 name=\"tokio-runtime-worker\""));
        // A zero window never divides by zero, and `top = 0` lists no thread.
        let zero = deltas.result_lines(Duration::ZERO, 5);
        assert!(zero.contains("per_second=0.000"), "{zero}");
        assert_eq!(
            deltas
                .result_lines(Duration::from_secs(1), 0)
                .lines()
                .count(),
            1
        );
    }

    #[cfg(any(
        target_os = "macos",
        target_os = "linux",
        target_os = "android",
        windows
    ))]
    #[test]
    fn thread_snapshot_lists_unique_live_threads() {
        let snapshot = process_usage_with_threads().unwrap();
        assert!(!snapshot.threads.is_empty());
        let mut ids: Vec<u64> = snapshot.threads.iter().map(|t| t.thread_id).collect();
        let listed = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), listed, "a thread was listed twice");
    }

    /// On Windows the total and the per-thread counters come from one table: they agree exactly,
    /// and a named thread shows up with its name, start address and the wake-ups it was given.
    #[cfg(all(windows, target_pointer_width = "64"))]
    #[test]
    fn windows_threads_sum_to_the_total_and_attribute_wakeups_to_the_woken_thread() {
        use std::sync::mpsc;
        let (id_tx, id_rx) = mpsc::channel::<u32>();
        let (wake_tx, wake_rx) = mpsc::channel::<()>();
        let (ack_tx, ack_rx) = mpsc::channel::<()>();
        let probe = std::thread::Builder::new()
            .name("rk-usage-probe".to_string())
            .spawn(move || {
                // SAFETY: no arguments; returns the calling thread's id.
                id_tx
                    .send(unsafe { windows::Win32::System::Threading::GetCurrentThreadId() })
                    .unwrap();
                while wake_rx.recv().is_ok() {
                    let _ = ack_tx.send(());
                }
            })
            .unwrap();
        let id = u64::from(id_rx.recv().unwrap());
        let first = process_usage_with_threads().unwrap();
        for _ in 0..5 {
            std::thread::sleep(Duration::from_millis(10));
            wake_tx.send(()).unwrap();
            ack_rx.recv().unwrap();
        }
        let second = process_usage_with_threads().unwrap();
        drop(wake_tx);
        probe.join().unwrap();

        for snapshot in [&first, &second] {
            assert!(snapshot.usage.wakeups_total.is_some());
            assert_eq!(snapshot.usage.wakeups_total, snapshot.thread_wakeups_sum());
        }
        let probe_now = second
            .threads
            .iter()
            .find(|t| t.thread_id == id)
            .expect("the probe thread is listed");
        assert_eq!(probe_now.name.as_deref(), Some("rk-usage-probe"));
        assert!(probe_now.start_address.is_some());
        assert!(probe_now.started.is_some());
        let deltas = thread_deltas(&first.threads, &second.threads);
        let delta = deltas
            .threads
            .iter()
            .find(|d| d.thread_id == id)
            .expect("the probe thread has a delta");
        assert!(!delta.started_in_window);
        assert!(delta.wakeups.unwrap() >= 1, "{delta:?}");
    }

    /// The idle observation's process delta is what the per-thread deltas add up to.
    #[cfg(all(windows, target_pointer_width = "64"))]
    #[test]
    fn windows_idle_thread_deltas_reconcile_with_the_process_delta() {
        let observed = observe_idle_with_threads(Duration::from_millis(50)).unwrap();
        assert!(observed.observation.elapsed >= Duration::from_millis(50));
        let start = observed.observation.start.wakeups_total.unwrap() as i128;
        let end = observed.observation.end.wakeups_total.unwrap() as i128;
        let listed = observed.threads.total_wakeups().unwrap() as i128;
        let exited = observed.threads.exited_wakeups_at_start().unwrap() as i128;
        assert_eq!(listed - exited, end - start);
    }
}

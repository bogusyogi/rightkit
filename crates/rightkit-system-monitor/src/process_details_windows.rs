//! What a Windows process row adds beyond CPU and memory: thread counts from one toolhelp
//! snapshot, executable paths from sysinfo and GPU use from the PDH "GPU Engine" counters.
//! Windows only; the unix rows come from `process_control`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::process_sampler;

/// Each field is None when unreadable.
#[derive(Clone, Debug, Default)]
pub struct ProcessDetail {
    pub threads: Option<u32>,
    pub executable: Option<PathBuf>,
    /// Share of the busiest GPU engine this process used since the previous reading, 0..=100.
    /// None when the counters are missing or have no earlier reading to compare with: never 0.
    pub gpu_percent: Option<f32>,
}

/// Windows thread counts from one toolhelp snapshot (`PROCESSENTRY32W.cntThreads`). Declared
/// directly, like the power query in monitor.rs, so no Cargo feature is needed.
mod toolhelp {
    use std::collections::HashMap;

    const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;
    const INVALID_HANDLE_VALUE: isize = -1;

    /// `PROCESSENTRY32W`; only `process_id` and `threads` are read.
    #[allow(dead_code)]
    #[repr(C)]
    struct ProcessEntry {
        size: u32,
        usage: u32,
        process_id: u32,
        default_heap_id: usize,
        module_id: u32,
        threads: u32,
        parent_process_id: u32,
        priority_base: i32,
        flags: u32,
        exe_file: [u16; 260],
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateToolhelp32Snapshot(flags: u32, process_id: u32) -> isize;
        fn Process32FirstW(snapshot: isize, entry: *mut ProcessEntry) -> i32;
        fn Process32NextW(snapshot: isize, entry: *mut ProcessEntry) -> i32;
        // Same signature as every other `CloseHandle` in this crate (clashing_extern_declarations).
        fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
    }

    /// Thread count by pid for every process in one snapshot. Empty when the snapshot fails.
    pub fn thread_counts() -> HashMap<u32, u32> {
        let mut counts = HashMap::new();
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return counts;
        }
        let mut entry: ProcessEntry = unsafe { std::mem::zeroed() };
        entry.size = std::mem::size_of::<ProcessEntry>() as u32;
        let mut more = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
        while more {
            counts.insert(entry.process_id, entry.threads);
            more = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
        }
        unsafe { CloseHandle(snapshot as *mut std::ffi::c_void) };
        counts
    }
}

/// Per-process GPU use from the documented "GPU Engine" performance counters (the ones Task
/// Manager's GPU column reads; no elevation). An instance is named
/// `pid_<pid>_luid_0x<high>_0x<low>_phys_<n>_eng_<n>_engtype_<type>`. "Running Time" is the
/// cumulative time the engine ran that process's work, in 100 ns, so use is
/// `100 * (running now - running before) / (time now - time before)`. It is read from raw
/// values on a fresh query each time: a wildcard query only holds the instances that existed
/// when it was opened, so a kept one would never see a process that started using the GPU later.
mod gpu {
    use std::collections::{HashMap, HashSet};
    use std::sync::{Mutex, PoisonError};
    use std::time::{Duration, Instant};

    use windows::Win32::System::Performance::{
        PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA, PDH_NO_DATA, PDH_RAW_COUNTER_ITEM_W,
        PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetRawCounterArrayW,
        PdhOpenQueryW,
    };
    use windows::core::PCWSTR;

    /// A second listing inside this span reuses the reading: a rate needs time between reads.
    const MIN_SPAN: Duration = Duration::from_secs(2);

    /// One instance's cumulative running time and the PDH timestamp it was read at.
    type Raw = (i64, u64);

    struct State {
        read_at: Option<Instant>,
        /// The last collection, by instance name; None until one succeeded.
        previous: Option<HashMap<String, Raw>>,
        /// The last answer: None when the counters cannot be read or nothing earlier exists.
        answer: Option<Answer>,
    }

    /// pid -> use. A pid absent from `by_pid` has no GPU engine instance, which is a real 0.
    #[derive(Clone)]
    pub struct Answer {
        by_pid: HashMap<u32, f32>,
        unknown: HashSet<u32>,
    }

    impl Answer {
        /// `None` for a process whose instances all appeared since the last reading.
        pub fn of(&self, pid: u32) -> Option<f32> {
            if self.unknown.contains(&pid) {
                return None;
            }
            Some(self.by_pid.get(&pid).copied().unwrap_or(0.0))
        }
    }

    struct Query(PDH_HQUERY);

    impl Drop for Query {
        fn drop(&mut self) {
            // SAFETY: the handle came from PdhOpenQueryW and is closed once.
            unsafe { PdhCloseQuery(self.0) };
        }
    }

    /// Every GPU engine instance with its raw running time, or the PDH status that failed.
    fn collect() -> Result<HashMap<String, Raw>, u32> {
        let mut handle = PDH_HQUERY(std::ptr::null_mut());
        // SAFETY: out-pointer to a local handle.
        let status = unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &mut handle) };
        if status != 0 {
            return Err(status);
        }
        let query = Query(handle);
        let path: Vec<u16> = "\\GPU Engine(*)\\Running Time"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut counter = PDH_HCOUNTER(std::ptr::null_mut());
        // SAFETY: `path` is NUL-terminated and outlives the call; the counter dies with the query.
        let status =
            unsafe { PdhAddEnglishCounterW(query.0, PCWSTR(path.as_ptr()), 0, &mut counter) };
        if status != 0 {
            return Err(status);
        }
        // SAFETY: a valid open query.
        let status = unsafe { PdhCollectQueryData(query.0) };
        if status != 0 {
            return Err(status);
        }
        let mut size = 0u32;
        let mut count = 0u32;
        // SAFETY: size query with no buffer.
        let status = unsafe { PdhGetRawCounterArrayW(counter, &mut size, &mut count, None) };
        // No instance exists: nothing is using the GPU.
        if status == PDH_NO_DATA {
            return Ok(HashMap::new());
        }
        if status != PDH_MORE_DATA {
            return Err(status);
        }
        // 8-byte aligned storage for the item array and the names that follow it.
        let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
        let items = buffer.as_mut_ptr() as *mut PDH_RAW_COUNTER_ITEM_W;
        // SAFETY: `buffer` holds `size` bytes as PDH asked.
        let status = unsafe { PdhGetRawCounterArrayW(counter, &mut size, &mut count, Some(items)) };
        if status != 0 {
            return Err(status);
        }
        // SAFETY: PDH filled `count` items inside `buffer`, which outlives this borrow.
        let items = unsafe { std::slice::from_raw_parts(items, count as usize) };
        let mut out = HashMap::with_capacity(items.len());
        for item in items {
            // CStatus 0 is valid data, 1 is valid new data.
            if item.RawValue.CStatus > 1 {
                continue;
            }
            // SAFETY: the name is a NUL-terminated string inside `buffer`.
            let Ok(name) = (unsafe { item.szName.to_string() }) else {
                continue;
            };
            let stamp = &item.RawValue.TimeStamp;
            let time = (u64::from(stamp.dwHighDateTime) << 32) | u64::from(stamp.dwLowDateTime);
            out.insert(name, (item.RawValue.FirstValue, time));
        }
        Ok(out)
    }

    /// `(pid, adapter, engine type)` of an instance name, or None for a name of another shape.
    fn parse(name: &str) -> Option<(u32, String, String)> {
        let pid = name.strip_prefix("pid_")?.split('_').next()?.parse().ok()?;
        let adapter_at = name.find("luid_")?;
        let adapter_end = name[adapter_at..]
            .find("_phys")
            .map_or(name.len(), |n| adapter_at + n);
        let kind = name.rsplit_once("engtype_")?.1;
        Some((
            pid,
            name[adapter_at..adapter_end].to_string(),
            kind.to_string(),
        ))
    }

    fn answer(before: &HashMap<String, Raw>, now: &HashMap<String, Raw>) -> Answer {
        // Engines of one type on one adapter add up; a process's figure is its busiest type, as
        // in Task Manager's GPU column, so it cannot pass 100.
        let mut engines: HashMap<(u32, String, String), f64> = HashMap::new();
        let mut pids: HashSet<u32> = HashSet::new();
        let mut known: HashSet<u32> = HashSet::new();
        for (name, &(running, time)) in now {
            let Some((pid, adapter, kind)) = parse(name) else {
                continue;
            };
            pids.insert(pid);
            let Some(&(was_running, was_time)) = before.get(name) else {
                continue;
            };
            if time <= was_time || running < was_running {
                continue;
            }
            known.insert(pid);
            let share = 100.0 * (running - was_running) as f64 / (time - was_time) as f64;
            *engines.entry((pid, adapter, kind)).or_default() += share;
        }
        let mut by_pid: HashMap<u32, f32> = HashMap::new();
        for ((pid, _, _), share) in engines {
            let share = share.clamp(0.0, 100.0) as f32;
            let slot = by_pid.entry(pid).or_default();
            *slot = slot.max(share);
        }
        Answer {
            by_pid,
            unknown: pids.difference(&known).copied().collect(),
        }
    }

    /// Use per pid since the previous reading. None when the counter set cannot be read (no WDDM
    /// GPU driver, PDH error) and on the first reading, which has nothing to compare with.
    pub fn read() -> Option<Answer> {
        read_spaced(MIN_SPAN)
    }

    /// `read` that reuses the last reading only if it is younger than `min_span`;
    /// `Duration::ZERO` always takes a new one.
    pub fn read_spaced(min_span: Duration) -> Option<Answer> {
        static STATE: Mutex<State> = Mutex::new(State {
            read_at: None,
            previous: None,
            answer: None,
        });
        let mut state = STATE.lock().unwrap_or_else(PoisonError::into_inner);
        if state.read_at.is_some_and(|at| at.elapsed() < min_span) {
            return state.answer.clone();
        }
        let now = collect().ok();
        state.read_at = Some(Instant::now());
        state.answer = match (&state.previous, &now) {
            (Some(before), Some(now)) => Some(answer(before, now)),
            _ => None,
        };
        state.previous = now;
        state.answer.clone()
    }
}

static PROCESS_GPU: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Turn the per-process GPU reading on or off (default on). The host app passes its own setting.
pub fn set_process_gpu_enabled(enabled: bool) {
    PROCESS_GPU.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

fn gpu_switch_on() -> bool {
    PROCESS_GPU.load(std::sync::atomic::Ordering::Relaxed)
}

/// Gap between the two GPU readings of a one-shot CLI read (#380).
const SECOND_GPU_READING_AFTER: Duration = Duration::from_secs(1);

/// Thread counts, executable paths and GPU use by pid for the Windows process rows. Thread counts come from
/// one toolhelp snapshot; executable paths come from the shared process table, which reads each path once.
/// Used by the engine's poll, which keeps its own cadence.
pub fn windows_details() -> HashMap<u32, ProcessDetail> {
    let gpu = if gpu_switch_on() { gpu::read() } else { None };
    details_with(gpu)
}

/// `windows_details` for one-shot CLI reads that show the GPU column (a process list sorted by GPU,
/// a one-shot monitor read). The GPU reader needs two readings, so this takes a baseline, waits about a
/// second and takes a second one; the rate is then real on the first printed output. It adds one
/// second when the GPU switch is on.
pub fn windows_details_settled() -> HashMap<u32, ProcessDetail> {
    let gpu = if gpu_switch_on() {
        let _ = gpu::read();
        std::thread::sleep(SECOND_GPU_READING_AFTER);
        gpu::read_spaced(Duration::ZERO)
    } else {
        None
    };
    details_with(gpu)
}

fn details_with(gpu: Option<gpu::Answer>) -> HashMap<u32, ProcessDetail> {
    let threads = toolhelp::thread_counts();
    process_sampler::with_processes(|system| {
        system
            .processes()
            .iter()
            .map(|(pid, process)| {
                let pid = pid.as_u32();
                let detail = ProcessDetail {
                    threads: threads.get(&pid).copied(),
                    executable: process.exe().map(Path::to_path_buf),
                    gpu_percent: gpu.as_ref().and_then(|g| g.of(pid)),
                };
                (pid, detail)
            })
            .collect()
    })
}

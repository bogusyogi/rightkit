//! Live system monitoring: CPU, memory and memory pressure, disks (with a hang-proof listing),
//! per-process rows with footprint, disk I/O and GPU use, process groups and inspection,
//! network counters, sensors (GPU temperature) and drive health.
//!
//! The host app may give a state directory with [`set_state_directory`]; the last completed drive
//! listing is then remembered there so a fresh launch whose first listing hangs still knows its
//! drives. Without one nothing is written.

pub mod drive_health;
pub mod gpu_temp;
pub mod monitor;
#[cfg(windows)]
pub mod process_details_windows;
pub mod process_inspect;
pub mod process_sampler;
pub mod processes;
mod snapshot;

static STATE_DIRECTORY: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// Name the folder where this crate may keep its small state (the last drive listing). Only the
/// first call takes effect; returns `false` when a directory was already set.
pub fn set_state_directory(dir: std::path::PathBuf) -> bool {
    STATE_DIRECTORY.set(dir).is_ok()
}

fn state_directory() -> Option<std::path::PathBuf> {
    STATE_DIRECTORY.get().cloned()
}

use serde::{Deserialize, Serialize};
use sysinfo::{Disks, System};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Capability {
    Available,
    Unavailable,
    PermissionDenied,
    Unsupported,
    /// The drive listing did not answer in time; sizes are withheld.
    NotResponding,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Metric<T> {
    pub value: Option<T>,
    pub capability: Capability,
    pub label: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SystemStatus {
    pub cpu_usage_percent: Metric<f32>,
    pub memory_used_bytes: Metric<u64>,
    pub memory_total_bytes: Metric<u64>,
    pub memory_pressure: Metric<String>,
    pub swap_used_bytes: Metric<u64>,
    pub swap_total_bytes: Metric<u64>,
    pub disks: Vec<DiskStatus>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiskStatus {
    pub mount_point: String,
    pub total_bytes: Option<u64>,
    /// Physically free bytes: what the file system would let a write use now.
    /// On macOS this excludes space the system could reclaim (local snapshots,
    /// caches), which is reported separately in `reclaimable_bytes`.
    pub available_bytes: Option<u64>,
    /// Bytes macOS could free on demand beyond `available_bytes`; never added
    /// to it. `None` when not reported or none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reclaimable_bytes: Option<u64>,
    pub removable: bool,
    pub capability: Capability,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub start_time: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub identity: ProcessIdentity,
    pub name: String,
    pub parent_pid: Option<u32>,
    pub cpu_usage_percent: f32,
    pub memory: Metric<u64>,
    pub gpu_usage_percent: Metric<f32>,
    /// Disk bytes this process moved (#324 item 10). `read_bytes` and `written_bytes` cover the
    /// same window as `cpu_usage_percent` (since the shared table's previous refresh); the totals
    /// cover the process's life. Absent in output from older builds.
    #[serde(default)]
    pub disk_io: Option<ProcessDiskIo>,
}

/// Bytes one process read and wrote, as the OS reports them through sysinfo. On macOS these are
/// disk bytes, and a process whose counters the OS withholds (`proc_pid_rusage` fails, typically
/// another user's process) reads as zero. On Windows they are `GetProcessIoCounters` transfer
/// counts, which include pipe, network and device I/O as well as files, and a process this crate
/// cannot open reads as zero.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessDiskIo {
    pub read_bytes: u64,
    pub written_bytes: u64,
    pub total_read_bytes: u64,
    pub total_written_bytes: u64,
}

impl ProcessDiskIo {
    /// Saturating sum of two readings (a group's total).
    pub fn plus(self, other: Self) -> Self {
        Self {
            read_bytes: self.read_bytes.saturating_add(other.read_bytes),
            written_bytes: self.written_bytes.saturating_add(other.written_bytes),
            total_read_bytes: self.total_read_bytes.saturating_add(other.total_read_bytes),
            total_written_bytes: self
                .total_written_bytes
                .saturating_add(other.total_written_bytes),
        }
    }
}

/// The disk counters sysinfo holds for one process (see `ProcessDiskIo`).
fn disk_io(process: &sysinfo::Process) -> ProcessDiskIo {
    let usage = process.disk_usage();
    ProcessDiskIo {
        read_bytes: usage.read_bytes,
        written_bytes: usage.written_bytes,
        total_read_bytes: usage.total_read_bytes,
        total_written_bytes: usage.total_written_bytes,
    }
}

/// One cheap reading of CPU, memory and swap. No process walk and no disk
/// list, so it can run every couple of seconds.
#[derive(Clone, Debug)]
pub struct LiveReading {
    pub cpu_percent: f32,
    pub memory_used_bytes: u64,
    pub memory_total_bytes: u64,
    pub swap_used_bytes: u64,
    pub swap_total_bytes: u64,
    /// "Normal", "Elevated" or "Critical"; `None` where it is not read.
    pub memory_pressure: Option<String>,
}

/// Keeps a `System` between readings: sysinfo computes CPU deltas between
/// refreshes, so the first reading after `new` is not a measurement.
pub struct LiveSampler {
    system: System,
}

impl Default for LiveSampler {
    fn default() -> Self {
        Self::new()
    }
}

impl LiveSampler {
    pub fn new() -> Self {
        let mut system = System::new();
        system.refresh_cpu_usage();
        system.refresh_memory();
        Self { system }
    }

    pub fn sample(&mut self) -> LiveReading {
        self.system.refresh_cpu_usage();
        self.system.refresh_memory();
        LiveReading {
            cpu_percent: self.system.global_cpu_usage(),
            memory_used_bytes: self.system.used_memory(),
            memory_total_bytes: self.system.total_memory(),
            swap_used_bytes: self.system.used_swap(),
            swap_total_bytes: self.system.total_swap(),
            memory_pressure: memory_pressure(),
        }
    }
}

/// The platform's memory pressure, or `None` where it is not read.
pub fn memory_pressure() -> Option<String> {
    memory_pressure_metric().value
}

/// Longest a caller waits for the drive listing. An unresponsive network
/// share can block sysinfo indefinitely.
const DISK_LIST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Drive-listing state shared by every caller.
struct DiskState {
    /// Last list that completed.
    cache: Option<Vec<DiskStatus>>,
    /// When the outstanding listing began; `None` when none is running.
    started: Option<std::time::Instant>,
    /// Bumped each time a listing completes, so waiters can tell theirs finished.
    generation: u64,
}

static DISK_STATE: std::sync::Mutex<DiskState> = std::sync::Mutex::new(DiskState {
    cache: None,
    started: None,
    generation: 0,
});
static DISK_DONE: std::sync::Condvar = std::sync::Condvar::new();

fn lock_disk_state() -> std::sync::MutexGuard<'static, DiskState> {
    DISK_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Marks the listing finished when its thread ends, even by panic.
struct DiskListingGuard;

impl Drop for DiskListingGuard {
    fn drop(&mut self) {
        lock_disk_state().started = None;
        DISK_DONE.notify_all();
    }
}

/// macOS mounts that are plumbing, not drives (`/dev`, the VM, Preboot, Update, xarts and
/// similar volumes under `/System/Volumes`): no free-space question is asked of them, which
/// the system logged as a failure each time. `/System/Volumes/Data` is the user's data and stays.
#[cfg(target_os = "macos")]
fn is_plumbing_mount(mount: &str) -> bool {
    mount == "/dev"
        || mount
            .strip_prefix("/System/Volumes/")
            .is_some_and(|name| name != "Data")
}

#[cfg(not(target_os = "macos"))]
fn is_plumbing_mount(_mount: &str) -> bool {
    false
}

fn read_disks() -> Vec<DiskStatus> {
    // The list first with no volume asked anything, then the free-space and size reading for
    // the mounts that are drives.
    let mut disks = Disks::new_with_refreshed_list_specifics(sysinfo::DiskRefreshKind::nothing());
    for disk in disks.list_mut() {
        if !is_plumbing_mount(&disk.mount_point().to_string_lossy()) {
            disk.refresh_specifics(sysinfo::DiskRefreshKind::everything());
        }
    }
    disks
        .list()
        .iter()
        .filter(|disk| !is_plumbing_mount(&disk.mount_point().to_string_lossy()))
        .map(|disk| {
            let (available_bytes, reclaimable_bytes) = free_and_reclaimable(disk);
            DiskStatus {
                mount_point: disk.mount_point().to_string_lossy().into_owned(),
                total_bytes: Some(disk.total_space()),
                available_bytes,
                reclaimable_bytes,
                removable: disk.is_removable(),
                capability: Capability::Available,
            }
        })
        .collect()
}

/// Physically free bytes of the mount and, on macOS, the extra bytes the
/// system could reclaim. sysinfo's `available_space()` on macOS is the
/// "available capacity for important usage" figure (falling back to plain
/// available capacity), which counts purgeable space such as local snapshots;
/// `statvfs` `f_bavail` is the physical figure. Where `statvfs` fails the free
/// figure is unknown rather than the larger reclaimable-inclusive one.
#[cfg(target_os = "macos")]
fn free_and_reclaimable(disk: &sysinfo::Disk) -> (Option<u64>, Option<u64>) {
    use std::os::unix::ffi::OsStrExt;
    let Ok(path) = std::ffi::CString::new(disk.mount_point().as_os_str().as_bytes()) else {
        return (None, None);
    };
    let mut statistics = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // statvfs reads mount accounting only, never file contents.
    if unsafe { libc::statvfs(path.as_ptr(), statistics.as_mut_ptr()) } != 0 {
        return (None, None);
    }
    let statistics = unsafe { statistics.assume_init() };
    #[allow(clippy::unnecessary_cast)]
    let free = (statistics.f_bavail as u64).saturating_mul(statistics.f_frsize as u64);
    let reclaimable = disk.available_space().saturating_sub(free);
    (Some(free), Some(reclaimable).filter(|bytes| *bytes > 0))
}

/// Windows and other platforms: the reported figure is already the free space
/// available to the caller, and nothing is reclaimable.
#[cfg(not(target_os = "macos"))]
fn free_and_reclaimable(disk: &sysinfo::Disk) -> (Option<u64>, Option<u64>) {
    (Some(disk.available_space()), None)
}

/// File holding the drive list from the last completed listing, kept in the state
/// directory so a fresh launch whose first listing hangs still knows its drives.
const LAST_DISKS_FILE: &str = "last-disks.json";

/// The drive list saved by the last completed listing this install ever made, or
/// empty when there is none or it cannot be read. Only mount points and the
/// removable flag are saved; no sizes, so no stale number is shown as current.
fn remembered_disks() -> Vec<DiskStatus> {
    let Some(dir) = state_directory() else {
        return Vec::new();
    };
    std::fs::read(dir.join(LAST_DISKS_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Saves a completed listing for the next launch. A failed save is ignored: the
/// listing itself is still returned.
fn remember_disks(list: &[DiskStatus]) {
    let Some(dir) = state_directory() else {
        return;
    };
    let Ok(bytes) = serde_json::to_vec(list) else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    let _ = rightkit_fs::atomic_write_with_options(
        &dir.join(LAST_DISKS_FILE),
        rightkit_fs::AtomicWriteOptions {
            privacy: rightkit_fs::Privacy::OwnerOnlyUnixInheritWindows,
            ..Default::default()
        },
        |file| std::io::Write::write_all(file, &bytes),
    );
}

/// The last good list with every drive marked `NotResponding` and no sizes, so
/// stale numbers are never shown as current. When no listing has finished in this
/// launch, the list saved by an earlier launch is used. Empty when neither exists.
fn stale_disks(state: &DiskState) -> Vec<DiskStatus> {
    let mut known = match &state.cache {
        Some(list) => list.clone(),
        None => remembered_disks(),
    };
    if known.is_empty() {
        // First listing ever and it hangs: name the drives from the mount table (or the drive
        // letter mask), which asks no volume anything, so the list is never empty.
        known = mounted_without_asking();
    }
    known
        .iter()
        .map(|disk| DiskStatus {
            mount_point: disk.mount_point.clone(),
            total_bytes: None,
            available_bytes: None,
            reclaimable_bytes: None,
            removable: disk.removable,
            capability: Capability::NotResponding,
        })
        .collect()
}

/// Drives named without asking any of them for a size, so a hung share cannot block this.
/// macOS reads the kernel's mount table with `getfsstat(MNT_NOWAIT)`, which returns each mount as
/// the kernel last recorded it and asks no volume anything (#324 item 13). Elsewhere than Windows
/// and macOS there is no such list: sysinfo's mount listing asks every volume for its properties,
/// so a first listing that hangs shows no drives.
#[cfg(target_os = "macos")]
fn mounted_without_asking() -> Vec<DiskStatus> {
    // SAFETY: a null buffer with size zero only counts the mounts.
    let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
    if count <= 0 {
        return Vec::new();
    }
    // Headroom for mounts added between the two calls; anything beyond it is picked up next listing.
    let mut table: Vec<libc::statfs> = Vec::with_capacity(count as usize + 8);
    // SAFETY: statfs is a plain C record; all-zero bytes are a valid value that the call overwrites.
    table.resize_with(count as usize + 8, || unsafe { std::mem::zeroed() });
    let bytes = (table.len() * std::mem::size_of::<libc::statfs>()) as libc::c_int;
    // SAFETY: the buffer holds `bytes` bytes of statfs records, as the size argument states.
    let filled = unsafe { libc::getfsstat(table.as_mut_ptr(), bytes, libc::MNT_NOWAIT) };
    if filled <= 0 {
        return Vec::new();
    }
    table.truncate(filled as usize);
    table
        .iter()
        .filter_map(|entry| {
            // SAFETY: getfsstat fills f_mntonname with a NUL-terminated mount path.
            let mount = unsafe { std::ffi::CStr::from_ptr(entry.f_mntonname.as_ptr()) }
                .to_string_lossy()
                .into_owned();
            // Mounts Finder hides (simulator runtimes, CoreDevice) are not drives; sysinfo's own
            // list skips them too.
            let hidden = entry.f_flags & (libc::MNT_DONTBROWSE as u32) != 0;
            (!hidden && !is_plumbing_mount(&mount)).then_some(DiskStatus {
                mount_point: mount,
                total_bytes: None,
                available_bytes: None,
                reclaimable_bytes: None,
                removable: false,
                capability: Capability::NotResponding,
            })
        })
        .collect()
}

#[cfg(not(any(windows, target_os = "macos")))]
fn mounted_without_asking() -> Vec<DiskStatus> {
    Vec::new()
}

#[cfg(windows)]
fn mounted_without_asking() -> Vec<DiskStatus> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetLogicalDrives() -> u32;
    }
    // SAFETY: a plain call returning a bit mask of lettered drives; it touches no volume.
    let mask = unsafe { GetLogicalDrives() };
    (0..26u8)
        .filter(|bit| mask & (1u32 << bit) != 0)
        .map(|bit| DiskStatus {
            mount_point: format!("{}:\\", char::from(b'A' + bit)),
            total_bytes: None,
            available_bytes: None,
            reclaimable_bytes: None,
            removable: false,
            capability: Capability::NotResponding,
        })
        .collect()
}

fn list_disks() -> Vec<DiskStatus> {
    let mut state = lock_disk_state();
    let generation = state.generation;
    let elapsed = match state.started {
        Some(started) => started.elapsed(),
        None => {
            state.started = Some(std::time::Instant::now());
            let spawned = std::thread::Builder::new()
                .name("rightkit-disk-list".into())
                .spawn(|| {
                    let _guard = DiskListingGuard;
                    let list = read_disks();
                    remember_disks(&list);
                    let mut state = lock_disk_state();
                    state.cache = Some(list);
                    state.generation += 1;
                    state.started = None;
                    drop(state);
                    DISK_DONE.notify_all();
                });
            if spawned.is_err() {
                state.started = None;
                return stale_disks(&state);
            }
            std::time::Duration::ZERO
        }
    };
    let Some(remaining) = DISK_LIST_TIMEOUT.checked_sub(elapsed) else {
        return stale_disks(&state);
    };
    let (state, _) = DISK_DONE
        .wait_timeout_while(state, remaining, |s| {
            s.generation == generation && s.started.is_some()
        })
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if state.generation != generation {
        return state.cache.clone().unwrap_or_default();
    }
    stale_disks(&state)
}

/// Mount points only: no CPU or memory reading, no 200 ms wait and no free-space call on any
/// mount (the full listing asks every mount, including system and network volumes).
#[cfg(not(windows))]
pub fn mount_points() -> Vec<String> {
    Disks::new_with_refreshed_list_specifics(sysinfo::DiskRefreshKind::nothing())
        .list()
        .iter()
        .map(|disk| disk.mount_point().to_string_lossy().into_owned())
        .collect()
}

/// Windows: the lettered drives from the drive mask. The engine's background drive-health pass
/// needs only these names, so it asks no volume for free space (#311 item 2).
#[cfg(windows)]
pub fn mount_points() -> Vec<String> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetDriveTypeW(root: *const u16) -> u32;
    }
    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_FIXED: u32 = 3;
    // Fixed and removable letters only, as sysinfo listed before: a mapped network letter, an
    // optical drive or a subst would otherwise be opened by drive health on every sample, and an
    // unreachable share can hold that open. GetDriveTypeW reads the type without asking the volume.
    mounted_without_asking()
        .into_iter()
        .map(|disk| disk.mount_point)
        .filter(|mount| {
            let wide: Vec<u16> = mount.encode_utf16().chain(std::iter::once(0)).collect();
            // SAFETY: `wide` is a NUL-terminated root path ("C:\\") valid for the call.
            matches!(
                unsafe { GetDriveTypeW(wide.as_ptr()) },
                DRIVE_REMOVABLE | DRIVE_FIXED
            )
        })
        .collect()
}

pub fn system_status() -> SystemStatus {
    let mut sampler = LiveSampler::new();
    // sysinfo computes CPU deltas between refreshes. Keep this bounded and
    // avoid publishing an immediate zero as a reading.
    std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
    let reading = sampler.sample();
    let cpu = reading.cpu_percent;
    let memory_label = "OS-reported system memory";
    let swap_total = reading.swap_total_bytes;
    let swap_used = reading.swap_used_bytes;
    let disks = list_disks();
    SystemStatus {
        cpu_usage_percent: Metric {
            value: Some(cpu),
            capability: Capability::Available,
            label: "total CPU usage".into(),
        },
        memory_used_bytes: Metric {
            value: Some(reading.memory_used_bytes),
            capability: Capability::Available,
            label: memory_label.into(),
        },
        memory_total_bytes: Metric {
            value: Some(reading.memory_total_bytes),
            capability: Capability::Available,
            label: memory_label.into(),
        },
        memory_pressure: memory_pressure_metric(),
        swap_used_bytes: Metric {
            value: Some(swap_used),
            capability: Capability::Available,
            label: "swap used".into(),
        },
        swap_total_bytes: Metric {
            value: Some(swap_total),
            capability: Capability::Available,
            label: "swap total".into(),
        },
        disks,
    }
}

/// Physical footprint (Activity Monitor's Memory column) of one of the current user's processes.
/// macOS only; `None` for other users' processes, which libproc will not report.
#[cfg(target_os = "macos")]
fn footprint(process: &sysinfo::Process, me: u32) -> Option<u64> {
    let mine = process.user_id().is_some_and(|uid| **uid == me);
    if mine {
        process_control::footprint_of(process.pid().as_u32())
    } else {
        None
    }
}

#[cfg(not(target_os = "macos"))]
fn footprint(_process: &sysinfo::Process, _me: u32) -> Option<u64> {
    None
}

/// Private bytes of one process (`PROCESS_MEMORY_COUNTERS_EX.PrivateUsage`): what the process has
/// committed for itself, the Windows counterpart of the Mac's physical footprint (#333). `None`
/// for a process Windows will not let this one query, which then keeps its working set.
#[cfg(windows)]
fn private_bytes(pid: u32) -> Option<u64> {
    use std::ffi::c_void;

    #[repr(C)]
    struct ProcessMemoryCountersEx {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
        private_usage: usize,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        fn CloseHandle(handle: *mut c_void) -> i32;
        fn K32GetProcessMemoryInfo(
            process: *mut c_void,
            counters: *mut ProcessMemoryCountersEx,
            size: u32,
        ) -> i32;
    }
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    // SAFETY: a handle opened for query only, closed before returning; the counters struct is
    // the size it is told.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut counters = ProcessMemoryCountersEx {
            cb: std::mem::size_of::<ProcessMemoryCountersEx>() as u32,
            page_fault_count: 0,
            peak_working_set_size: 0,
            working_set_size: 0,
            quota_peak_paged_pool_usage: 0,
            quota_paged_pool_usage: 0,
            quota_peak_non_paged_pool_usage: 0,
            quota_non_paged_pool_usage: 0,
            pagefile_usage: 0,
            peak_pagefile_usage: 0,
            private_usage: 0,
        };
        let read = K32GetProcessMemoryInfo(handle, &mut counters, counters.cb);
        CloseHandle(handle);
        (read != 0).then_some(counters.private_usage as u64)
    }
}

#[cfg(not(windows))]
fn private_bytes(_pid: u32) -> Option<u64> {
    None
}

/// Per-process GPU share for one row (#380). Windows reads the PDH GPU Engine counters through
/// `process_details_windows`, which needs an earlier reading to compare with, so the first row set
/// reports no value. macOS has no public per-process GPU API, so its row says so.
#[cfg(windows)]
fn gpu_metric(gpu_percent: Option<f32>) -> Metric<f32> {
    match gpu_percent {
        Some(value) => Metric {
            value: Some(value),
            capability: Capability::Available,
            label: "share of the busiest GPU engine".into(),
        },
        None => Metric {
            value: None,
            capability: Capability::Unsupported,
            label: "per-process GPU has no reading yet (first sample, or counters unavailable)"
                .into(),
        },
    }
}

#[cfg(not(windows))]
fn gpu_metric(_gpu_percent: Option<f32>) -> Metric<f32> {
    Metric {
        value: None,
        capability: Capability::Unsupported,
        label: "per-process GPU unavailable: macOS has no public per-process GPU API".into(),
    }
}

pub fn procs() -> Vec<ProcessInfo> {
    procs_with(false)
}

/// `procs` for one-shot CLI reads that print the GPU column: on Windows the GPU reading is taken
/// twice, about a second apart, so the rows carry real per-process GPU use (#380).
pub fn procs_gpu_settled() -> Vec<ProcessInfo> {
    procs_with(true)
}

fn procs_with(settle_gpu: bool) -> Vec<ProcessInfo> {
    #[cfg(target_os = "macos")]
    let me = unsafe { libc::getuid() };
    #[cfg(not(target_os = "macos"))]
    let me = 0;
    // Read before the process table is locked: the Windows details take that same table.
    #[cfg(windows)]
    let gpu_by_pid = if settle_gpu {
        process_details_windows::windows_details_settled()
    } else {
        process_details_windows::windows_details()
    };
    #[cfg(not(windows))]
    let _ = settle_gpu;
    let mut processes: Vec<_> = process_sampler::with_processes(|system| {
        system
            .processes()
            .values()
            .map(|process| {
                let footprint = footprint(process, me);
                let private = private_bytes(process.pid().as_u32());
                ProcessInfo {
                    identity: ProcessIdentity {
                        pid: process.pid().as_u32(),
                        start_time: process.start_time(),
                    },
                    name: process.name().to_string_lossy().into_owned(),
                    parent_pid: process.parent().map(|pid| pid.as_u32()),
                    cpu_usage_percent: process.cpu_usage(),
                    memory: Metric {
                        value: Some(footprint.or(private).unwrap_or_else(|| process.memory())),
                        capability: Capability::Available,
                        label: if footprint.is_some() {
                            crate::processes::FOOTPRINT_LABEL
                        } else if private.is_some() {
                            crate::processes::PRIVATE_LABEL
                        } else if cfg!(windows) {
                            "working set; private bytes unavailable"
                        } else {
                            "resident memory (RSS); physical footprint unavailable"
                        }
                        .into(),
                    },
                    #[cfg(windows)]
                    gpu_usage_percent: gpu_metric(
                        gpu_by_pid
                            .get(&process.pid().as_u32())
                            .and_then(|detail| detail.gpu_percent),
                    ),
                    #[cfg(not(windows))]
                    gpu_usage_percent: gpu_metric(None),
                    disk_io: Some(disk_io(process)),
                }
            })
            .collect()
    });
    processes.sort_by_key(|process| (process.identity.pid, process.identity.start_time));
    processes
}

/// macOS reports memory pressure as 1 (normal), 2 (warning) or 4 (critical).
#[cfg(target_os = "macos")]
fn read_memory_pressure() -> Option<String> {
    let mut level: libc::c_int = 0;
    let mut size = std::mem::size_of::<libc::c_int>();
    let rc = unsafe {
        libc::sysctlbyname(
            c"kern.memorystatus_vm_pressure_level".as_ptr(),
            (&mut level as *mut libc::c_int).cast::<libc::c_void>(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    match level {
        1 => Some("Normal".into()),
        2 => Some("Elevated".into()),
        4 => Some("Critical".into()),
        _ => None,
    }
}

/// Windows has no pressure level, so it is derived from `GlobalMemoryStatusEx`:
/// commit charge against the commit limit (RAM plus page files), and available
/// physical memory. Critical at 95% commit or under 5% of RAM available;
/// Elevated at 85% commit or under 10% of RAM available; otherwise Normal.
#[cfg(target_os = "windows")]
fn read_memory_pressure() -> Option<String> {
    #[repr(C)]
    struct MemoryStatusEx {
        length: u32,
        memory_load: u32,
        total_phys: u64,
        avail_phys: u64,
        total_page_file: u64,
        avail_page_file: u64,
        total_virtual: u64,
        avail_virtual: u64,
        avail_extended_virtual: u64,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalMemoryStatusEx(status: *mut MemoryStatusEx) -> i32;
    }
    let mut status = MemoryStatusEx {
        length: std::mem::size_of::<MemoryStatusEx>() as u32,
        memory_load: 0,
        total_phys: 0,
        avail_phys: 0,
        total_page_file: 0,
        avail_page_file: 0,
        total_virtual: 0,
        avail_virtual: 0,
        avail_extended_virtual: 0,
    };
    // SAFETY: `status` is a live, correctly sized MEMORYSTATUSEX with `length` set.
    if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 || status.total_phys == 0 {
        return None;
    }
    let commit_limit = status.total_page_file;
    let committed = commit_limit.saturating_sub(status.avail_page_file);
    let commit_ratio = if commit_limit == 0 {
        0.0
    } else {
        committed as f64 / commit_limit as f64
    };
    let available_ratio = status.avail_phys as f64 / status.total_phys as f64;
    Some(
        if commit_ratio >= 0.95 || available_ratio < 0.05 {
            "Critical"
        } else if commit_ratio >= 0.85 || available_ratio < 0.10 {
            "Elevated"
        } else {
            "Normal"
        }
        .into(),
    )
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn read_memory_pressure() -> Option<String> {
    None
}

fn memory_pressure_metric() -> Metric<String> {
    #[cfg(target_os = "macos")]
    let label = "macOS memory pressure";
    #[cfg(target_os = "windows")]
    let label = "Windows commit pressure";
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let label = "platform memory pressure";
    let value = read_memory_pressure();
    Metric {
        capability: if value.is_some() {
            Capability::Available
        } else {
            Capability::Unavailable
        },
        value,
        label: label.into(),
    }
}

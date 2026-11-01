//! Live process list grouped by application, with Quit and Force Quit.
//!
//! Identity is `(pid, start_time)` and is re-checked against a fresh snapshot
//! immediately before any signal or quit request. Quit is always graceful and
//! never escalates; Force Quit is a separate, explicit call. Other users'
//! processes, system processes and the host app itself are never listed or touched.

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use rightkit_child_process::{AdoptedIdentity, AdoptedSignal};
use serde::{Deserialize, Serialize};
use sysinfo::{Pid, Process, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use crate::app_manager::{app_root_of, bundle_info, quit_bundle};
use rightkit_system_monitor::process_sampler;
use rightkit_system_monitor::{ProcessDiskIo, ProcessIdentity};

const SYSTEM_PREFIXES: [&str; 6] = [
    "/System/",
    "/usr/",
    "/sbin/",
    "/bin/",
    "/Library/Apple/",
    "/private/",
];
const NEVER_QUIT_BUNDLES: [&str; 4] = [
    "com.apple.finder",
    "com.apple.dock",
    "com.apple.systemuiserver",
    "com.apple.loginwindow",
];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessMember {
    pub identity: ProcessIdentity,
    pub name: String,
    pub cpu_usage_percent: f32,
    /// Memory this process is responsible for. On macOS this is the physical footprint
    /// (Activity Monitor's Memory column), which counts compressed and swapped pages; elsewhere,
    /// or when the footprint cannot be read, it is the resident size.
    pub memory_bytes: u64,
    /// Resident size (in RAM now), when `memory_bytes` is a different figure; omitted when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resident_bytes: Option<u64>,
    /// None when the platform did not report a thread count for this process.
    pub threads: Option<u32>,
    /// Unix seconds the process started; None when unreadable.
    pub started_at: Option<u64>,
    /// Disk counters for this process (#324 item 10); omitted when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_io: Option<ProcessDiskIo>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessRow {
    /// Stable key: the app bundle path, or `pid:start` for a plain process.
    pub key: String,
    pub name: String,
    pub bundle_id: Option<String>,
    pub app_path: Option<String>,
    /// The main process of the group; identity for Quit.
    pub lead: ProcessIdentity,
    pub cpu_usage_percent: f32,
    /// Sum of the members' `memory_bytes`.
    pub memory_bytes: u64,
    /// Sum of the members' resident sizes, when any member reports one separately; omitted otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resident_bytes: Option<u64>,
    /// Threads across all members; None when any member's count is unreadable.
    pub threads: Option<u32>,
    /// The members' disk counters added up (#324 item 10); omitted when no member has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_io: Option<ProcessDiskIo>,
    /// Unix seconds the lead process started; None when unreadable.
    pub started_at: Option<u64>,
    /// The app bundle for an app, or the executable for a plain process; None when unknown.
    pub executable: Option<String>,
    pub members: Vec<ProcessMember>,
    /// False when Quit/Force Quit must not be offered, with the reason.
    pub can_act: bool,
    pub refusal: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuitOutcome {
    /// The process or app is gone.
    Quit,
    /// A graceful request was sent but the target is still running.
    StillRunning,
}

#[derive(Clone)]
struct Snap {
    identity: ProcessIdentity,
    name: String,
    parent: Option<u32>,
    exe: Option<PathBuf>,
    cpu: f32,
    /// Physical footprint where readable, else the resident size.
    memory: u64,
    /// Resident size, set only when `memory` is the footprint.
    resident: Option<u64>,
    threads: Option<u32>,
    disk: Option<ProcessDiskIo>,
    mine: bool,
}

fn current_uid() -> u32 {
    unsafe { libc::getuid() }
}

/// The live listing: CPU, memory, footprint and threads, from the shared long-lived table.
fn snapshot() -> Vec<Snap> {
    let me = current_uid();
    process_sampler::with_processes(|system| {
        let threads = thread_table(
            system
                .processes()
                .values()
                .filter(|p| is_mine(p, me))
                .map(|p| p.pid().as_u32()),
        );
        system
            .processes()
            .values()
            .map(|p| snap_of(p, me, Some(&threads)))
            .collect()
    })
}

/// Identity, owner and executable only, read fresh from the system, for the check made at
/// action time. Nothing cached: the pid, start time and owner it reports are live.
fn identity_snapshot() -> Vec<Snap> {
    let mut system = System::new();
    let kind = ProcessRefreshKind::nothing()
        .with_exe(UpdateKind::Always)
        .with_user(UpdateKind::Always);
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
    let me = current_uid();
    system
        .processes()
        .values()
        .map(|p| snap_of(p, me, None))
        .collect()
}

fn is_mine(p: &Process, me: u32) -> bool {
    p.user_id().map(|u| **u == me).unwrap_or(false)
}

/// sysinfo reports 0 when it has no start time for a process.
fn known_start(start_time: u64) -> Option<u64> {
    (start_time > 0).then_some(start_time)
}

/// The disk counters sysinfo holds for one process: the window since the shared table's previous
/// refresh, and the lifetime totals.
fn disk_of(p: &Process) -> ProcessDiskIo {
    let usage = p.disk_usage();
    ProcessDiskIo {
        read_bytes: usage.read_bytes,
        written_bytes: usage.written_bytes,
        total_read_bytes: usage.total_read_bytes,
        total_written_bytes: usage.total_written_bytes,
    }
}

/// The members' disk counters added up; None only when no member has one.
fn sum_disk(disks: impl Iterator<Item = Option<ProcessDiskIo>>) -> Option<ProcessDiskIo> {
    let known: Vec<ProcessDiskIo> = disks.flatten().collect();
    (!known.is_empty()).then(|| {
        known
            .into_iter()
            .fold(ProcessDiskIo::default(), ProcessDiskIo::plus)
    })
}

/// `threads` is None for the action-time check, which reads neither memory figures nor threads.
fn snap_of(p: &Process, me: u32, threads: Option<&HashMap<u32, u32>>) -> Snap {
    let pid = p.pid().as_u32();
    let mine = is_mine(p, me);
    let footprint = if mine && threads.is_some() {
        footprint_of(pid)
    } else {
        None
    };
    Snap {
        identity: ProcessIdentity {
            pid,
            start_time: p.start_time(),
        },
        name: p.name().to_string_lossy().into_owned(),
        parent: p.parent().map(|pid| pid.as_u32()),
        exe: p.exe().map(Path::to_path_buf),
        cpu: p.cpu_usage(),
        memory: footprint.unwrap_or_else(|| p.memory()),
        resident: footprint.map(|_| p.memory()),
        threads: threads.and_then(|t| t.get(&pid).copied()),
        disk: Some(disk_of(p)),
        mine,
    }
}

/// Physical footprint in bytes, which counts compressed and swapped pages. macOS only; None
/// elsewhere or when libproc will not say, and the row then keeps the resident size.
#[cfg(target_os = "macos")]
pub fn footprint_of(pid: u32) -> Option<u64> {
    libproc::phys_footprint(pid)
}

#[cfg(not(target_os = "macos"))]
pub fn footprint_of(_pid: u32) -> Option<u64> {
    None
}

/// Thread counts for the given processes. macOS asks libproc for each one; Windows reads one
/// toolhelp snapshot, which needs no process handle, so a process that cannot be opened still has
/// a count. Other systems report none.
#[cfg(target_os = "macos")]
fn thread_table(pids: impl Iterator<Item = u32>) -> HashMap<u32, u32> {
    pids.filter_map(|pid| libproc::thread_count(pid).map(|count| (pid, count)))
        .collect()
}

#[cfg(windows)]
fn thread_table(_pids: impl Iterator<Item = u32>) -> HashMap<u32, u32> {
    toolhelp::thread_counts()
}

#[cfg(not(any(target_os = "macos", windows)))]
fn thread_table(_pids: impl Iterator<Item = u32>) -> HashMap<u32, u32> {
    HashMap::new()
}

/// macOS thread counts from `proc_pidinfo(PROC_PIDTASKINFO)`, for the current user's processes.
#[cfg(target_os = "macos")]
mod libproc {
    use std::ffi::{c_int, c_void};

    const PROC_PIDTASKINFO: c_int = 4;

    /// `struct proc_taskinfo` from `<sys/proc_info.h>`. Only `threadnum` is read; the rest keeps the layout.
    #[allow(dead_code)]
    #[repr(C)]
    #[derive(Default)]
    struct TaskInfo {
        virtual_size: u64,
        resident_size: u64,
        total_user: u64,
        total_system: u64,
        threads_user: u64,
        threads_system: u64,
        policy: i32,
        faults: i32,
        pageins: i32,
        cow_faults: i32,
        messages_sent: i32,
        messages_received: i32,
        syscalls_mach: i32,
        syscalls_unix: i32,
        csw: i32,
        threadnum: i32,
        numrunning: i32,
        priority: i32,
    }

    const RUSAGE_INFO_V2: c_int = 2;

    /// `struct rusage_info_v2` from `<libproc.h>`, the first flavor with `ri_phys_footprint`.
    /// Declared here rather than taken from libc, whose pinned version may lack it. Only the
    /// footprint is read; the rest keeps the layout.
    #[allow(dead_code)]
    #[repr(C)]
    #[derive(Default)]
    struct RusageInfoV2 {
        uuid: [u8; 16],
        user_time: u64,
        system_time: u64,
        pkg_idle_wkups: u64,
        interrupt_wkups: u64,
        pageins: u64,
        wired_size: u64,
        resident_size: u64,
        phys_footprint: u64,
        proc_start_abstime: u64,
        proc_exit_abstime: u64,
        child_user_time: u64,
        child_system_time: u64,
        child_pkg_idle_wkups: u64,
        child_interrupt_wkups: u64,
        child_pageins: u64,
        child_elapsed_abstime: u64,
        diskio_bytesread: u64,
        diskio_byteswritten: u64,
    }

    unsafe extern "C" {
        fn proc_pid_rusage(pid: c_int, flavor: c_int, buffer: *mut c_void) -> c_int;
        fn proc_pidinfo(
            pid: c_int,
            flavor: c_int,
            arg: u64,
            buffer: *mut c_void,
            buffersize: c_int,
        ) -> c_int;
    }

    /// The thread count, or None when libproc will not say (the process is gone or not ours).
    pub fn thread_count(pid: u32) -> Option<u32> {
        let mut info = TaskInfo::default();
        let size = std::mem::size_of::<TaskInfo>() as c_int;
        let buffer: *mut c_void = (&mut info as *mut TaskInfo).cast();
        let written = unsafe { proc_pidinfo(pid as c_int, PROC_PIDTASKINFO, 0, buffer, size) };
        (written == size && info.threadnum > 0).then_some(info.threadnum as u32)
    }

    /// The physical footprint in bytes (Activity Monitor's Memory), or None when libproc will not
    /// say (another user's or a system process, or one that has exited). One syscall per process.
    pub fn phys_footprint(pid: u32) -> Option<u64> {
        let mut info = RusageInfoV2::default();
        let buffer: *mut c_void = (&mut info as *mut RusageInfoV2).cast();
        // SAFETY: `buffer` points at a live, writable `RusageInfoV2` laid out as the kernel's
        // `rusage_info_v2`, which is what flavor RUSAGE_INFO_V2 fills; the call writes no further.
        let status = unsafe { proc_pid_rusage(pid as c_int, RUSAGE_INFO_V2, buffer) };
        (status == 0 && info.phys_footprint > 0).then_some(info.phys_footprint)
    }
}

fn is_system_path(path: &Path) -> bool {
    let text = path.to_string_lossy();
    if text.starts_with("/usr/local/") {
        return false;
    }
    SYSTEM_PREFIXES
        .iter()
        .any(|prefix| text.starts_with(prefix))
}

fn is_host(root: Option<&Path>, bundle_id: Option<&str>, name: &str) -> bool {
    bundle_id.is_some_and(crate::host::is_host_bundle_id)
        || root
            .and_then(|p| p.file_name())
            .is_some_and(|n| crate::host::is_host_name(&n.to_string_lossy()))
        || crate::host::is_host_name(name)
}

/// Which app bundle a process belongs to: its own executable's outermost
/// `.app`, else the nearest ancestor (verified: the parent started no later
/// than the child, so a reused PID cannot adopt it) that lives in one.
fn owning_app(snap: &Snap, by_pid: &HashMap<u32, &Snap>) -> Option<PathBuf> {
    if let Some(root) = snap.exe.as_deref().and_then(app_root_of) {
        return Some(root);
    }
    let mut current = snap;
    for _ in 0..16 {
        let parent = by_pid.get(&current.parent?)?;
        if parent.identity.start_time > current.identity.start_time || parent.identity.pid <= 1 {
            return None;
        }
        if let Some(root) = parent.exe.as_deref().and_then(app_root_of) {
            return Some(root);
        }
        current = parent;
    }
    None
}

type InfoCache = Mutex<HashMap<PathBuf, Option<(String, String)>>>;

fn infos() -> &'static InfoCache {
    static CACHE: OnceLock<InfoCache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// `(bundle id, display name)` for an app bundle, cached.
fn cached_info(root: &Path) -> Option<(String, String)> {
    if let Ok(cache) = infos().lock()
        && let Some(found) = cache.get(root)
    {
        return found.clone();
    }
    let found = bundle_info(root).and_then(|i| i.bundle_id.map(|b| (b, i.name)));
    if let Ok(mut cache) = infos().lock() {
        cache.insert(root.to_path_buf(), found.clone());
    }
    found
}

/// Processes of the current user, grouped under their app, largest memory first.
pub fn process_rows() -> Vec<ProcessRow> {
    build_rows(&snapshot())
}

fn member(s: &Snap) -> ProcessMember {
    ProcessMember {
        identity: s.identity.clone(),
        name: s.name.clone(),
        cpu_usage_percent: s.cpu,
        memory_bytes: s.memory,
        resident_bytes: s.resident,
        threads: s.threads,
        started_at: known_start(s.identity.start_time),
        disk_io: s.disk,
    }
}

fn refusal_for(
    root: Option<&Path>,
    bundle_id: Option<&str>,
    name: &str,
    members: &[&Snap],
) -> Option<String> {
    if members.iter().any(|m| m.identity.pid == std::process::id())
        || is_host(root, bundle_id, name)
    {
        return Some(format!(
            "{} does not quit itself.",
            crate::host::display_name()
        ));
    }
    if let Some(b) = bundle_id
        && NEVER_QUIT_BUNDLES.contains(&b)
    {
        return Some("macOS restarts this automatically; quit it from the system.".into());
    }
    None
}

fn build_rows(snaps: &[Snap]) -> Vec<ProcessRow> {
    let by_pid: HashMap<u32, &Snap> = snaps.iter().map(|s| (s.identity.pid, s)).collect();
    let mut apps: HashMap<PathBuf, Vec<&Snap>> = HashMap::new();
    let mut loose: Vec<&Snap> = Vec::new();
    for snap in snaps.iter().filter(|s| s.mine && s.identity.pid > 1) {
        if snap.exe.as_deref().map(is_system_path).unwrap_or(false) {
            continue;
        }
        match owning_app(snap, &by_pid) {
            Some(root) => apps.entry(root).or_default().push(snap),
            None => loose.push(snap),
        }
    }
    let mut rows = Vec::new();
    for (root, members) in apps {
        let info = cached_info(&root);
        let bundle_id = info.as_ref().map(|i| i.0.clone());
        let name = info.map(|i| i.1).unwrap_or_else(|| {
            root.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
        let main_prefix = root.join("Contents/MacOS");
        let Some(lead) = members
            .iter()
            .filter(|m| {
                m.exe
                    .as_deref()
                    .map(|e| e.starts_with(&main_prefix))
                    .unwrap_or(false)
            })
            .min_by_key(|m| m.identity.start_time)
            .or_else(|| members.iter().min_by_key(|m| m.identity.start_time))
            .map(|m| m.identity.clone())
        else {
            continue;
        };
        let started_at = known_start(lead.start_time);
        let refusal = refusal_for(Some(&root), bundle_id.as_deref(), &name, &members);
        rows.push(ProcessRow {
            key: root.to_string_lossy().into_owned(),
            name,
            bundle_id,
            app_path: Some(root.to_string_lossy().into_owned()),
            lead,
            cpu_usage_percent: members.iter().map(|m| m.cpu).sum(),
            memory_bytes: members.iter().map(|m| m.memory).sum(),
            resident_bytes: members
                .iter()
                .any(|m| m.resident.is_some())
                .then(|| members.iter().map(|m| m.resident.unwrap_or(m.memory)).sum()),
            threads: members.iter().map(|m| m.threads).sum::<Option<u32>>(),
            disk_io: sum_disk(members.iter().map(|m| m.disk)),
            started_at,
            executable: Some(root.to_string_lossy().into_owned()),
            members: members.iter().map(|m| member(m)).collect(),
            can_act: refusal.is_none(),
            refusal,
        });
    }
    for snap in loose {
        let refusal = refusal_for(None, None, &snap.name, &[snap]);
        rows.push(ProcessRow {
            key: format!("{}:{}", snap.identity.pid, snap.identity.start_time),
            name: snap.name.clone(),
            bundle_id: None,
            app_path: None,
            lead: snap.identity.clone(),
            cpu_usage_percent: snap.cpu,
            memory_bytes: snap.memory,
            resident_bytes: snap.resident,
            threads: snap.threads,
            disk_io: snap.disk,
            started_at: known_start(snap.identity.start_time),
            executable: snap
                .exe
                .as_deref()
                .map(|path| path.to_string_lossy().into_owned()),
            members: vec![member(snap)],
            can_act: refusal.is_none(),
            refusal,
        });
    }
    rows.sort_by_key(|a| std::cmp::Reverse(a.memory_bytes));
    rows
}

/// Re-read the system and return the row `lead` currently heads, or the
/// reason it cannot be acted on. Identity must match exactly.
fn recheck(key: &str, lead: &ProcessIdentity) -> Result<ProcessRow, String> {
    let snaps = identity_snapshot();
    let current = snaps
        .iter()
        .find(|s| s.identity.pid == lead.pid)
        .ok_or("That process has already exited.")?;
    if current.identity.start_time != lead.start_time {
        return Err("That process was replaced by a different one; nothing was changed.".into());
    }
    let row = build_rows(&snaps)
        .into_iter()
        .find(|r| r.key == key && r.lead.pid == lead.pid && r.lead.start_time == lead.start_time)
        .ok_or("That is no longer a listed app or process; nothing was changed.")?;
    match &row.refusal {
        Some(reason) => Err(reason.clone()),
        None => Ok(row),
    }
}

fn alive(id: &ProcessIdentity) -> bool {
    let mut system = System::new();
    let pid = Pid::from_u32(id.pid);
    system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
    system
        .process(pid)
        .map(|p| p.start_time() == id.start_time)
        .unwrap_or(false)
}

fn wait_gone(id: &ProcessIdentity, limit: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
        if !alive(id) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    !alive(id)
}

/// Sends `signal` to the process `id` names. RightKit's owner captures the pid and re-checks its
/// identity inside the signal call; the start time is read again around the capture, so a pid
/// reused since the check is never signalled.
fn signal(id: &ProcessIdentity, how: AdoptedSignal) -> Result<(), String> {
    if !alive(id) {
        return Ok(());
    }
    let Some(pid) = NonZeroU32::new(id.pid) else {
        return Ok(());
    };
    let Some(target) = AdoptedIdentity::capture(pid).map_err(|e| e.to_string())? else {
        return Ok(());
    };
    if !alive(id) {
        return Ok(());
    }
    target.signal(how).map_err(|e| e.to_string())?;
    Ok(())
}

/// Graceful quit. Apps get a Quit Apple event; plain processes get SIGTERM.
/// Never escalates: a target that stays up is reported as `StillRunning`.
pub fn quit(key: &str, lead: &ProcessIdentity) -> Result<QuitOutcome, String> {
    let row = recheck(key, lead)?;
    match &row.bundle_id {
        Some(bundle_id) if row.app_path.is_some() => quit_bundle(bundle_id)?,
        _ => signal(&row.lead, AdoptedSignal::Graceful)?,
    }
    Ok(if wait_gone(&row.lead, Duration::from_secs(8)) {
        QuitOutcome::Quit
    } else {
        QuitOutcome::StillRunning
    })
}

/// Explicit SIGKILL of the group's current members (each identity re-checked).
pub fn force_quit(key: &str, lead: &ProcessIdentity) -> Result<QuitOutcome, String> {
    let row = recheck(key, lead)?;
    for m in &row.members {
        signal(&m.identity, AdoptedSignal::Force)?;
    }
    Ok(if wait_gone(&row.lead, Duration::from_secs(3)) {
        QuitOutcome::Quit
    } else {
        QuitOutcome::StillRunning
    })
}

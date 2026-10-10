//! Content-free host/process resource snapshot for diagnostics.

use serde_json::{json, Map, Value};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

static SAMPLER: OnceLock<Mutex<Sampler>> = OnceLock::new();
const CACHE_FOR: Duration = Duration::from_secs(10);

/// Best-effort resource values. Missing platform counters stay absent.
pub fn snapshot() -> Value {
    let sampler = SAMPLER.get_or_init(|| Mutex::new(Sampler::default()));
    let mut sampler = sampler.lock().unwrap_or_else(|error| error.into_inner());
    if let Some((captured_at, value)) = sampler.cached.as_ref() {
        if captured_at.elapsed() < CACHE_FOR {
            return value.clone();
        }
    }
    let value = platform::collect(&mut sampler.cpu_baseline).into_json();
    sampler.cached = Some((Instant::now(), value.clone()));
    value
}

#[derive(Default)]
struct Sampler {
    cached: Option<(Instant, Value)>,
    cpu_baseline: Option<platform::CpuBaseline>,
}

#[derive(Default)]
struct PlatformSnapshot {
    system_load_1m: Option<f64>,
    system_cpu_pct: Option<f64>,
    memory_pressure_pct: Option<f64>,
    swap_used_mb: Option<f64>,
    thermal_state: Option<String>,
    app_cpu_pct: Option<f64>,
    app_memory_mb: Option<f64>,
}

impl PlatformSnapshot {
    fn into_json(self) -> Value {
        let mut out = Map::new();
        insert_number(&mut out, "system_load_1m", self.system_load_1m);
        insert_number(&mut out, "system_cpu_pct", self.system_cpu_pct);
        insert_number(&mut out, "memory_pressure_pct", self.memory_pressure_pct);
        insert_number(&mut out, "swap_used_mb", self.swap_used_mb);
        insert_number(&mut out, "app_cpu_pct", self.app_cpu_pct);
        insert_number(&mut out, "app_memory_mb", self.app_memory_mb);
        if let Some(value) = self.thermal_state {
            out.insert("thermal_state".into(), json!(value));
        }
        Value::Object(out)
    }
}

fn insert_number(out: &mut Map<String, Value>, key: &str, value: Option<f64>) {
    if let Some(value) = value.filter(|value| value.is_finite()) {
        out.insert(key.into(), json!(value));
    }
}

fn bytes_to_mb(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

fn percent_used(used: u64, total: u64) -> Option<f64> {
    (total > 0).then(|| (used as f64 * 100.0 / total as f64).clamp(0.0, 100.0))
}

#[cfg(target_os = "windows")]
mod platform {
    use super::{bytes_to_mb, percent_used, PlatformSnapshot};
    use std::mem::{size_of, zeroed};
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetProcessTimes, GetSystemTimes,
    };

    #[derive(Clone, Copy)]
    pub(super) struct CpuBaseline {
        idle: u64,
        kernel: u64,
        user: u64,
        process: u64,
    }

    pub(super) fn collect(baseline: &mut Option<CpuBaseline>) -> PlatformSnapshot {
        let memory = read_memory();
        let current_cpu = read_cpu_times();
        let (system_cpu_pct, app_cpu_pct) = baseline
            .zip(current_cpu)
            .and_then(|(previous, current)| cpu_percentages(previous, current))
            .map(|(system, app)| (Some(system), Some(app)))
            .unwrap_or_default();
        if current_cpu.is_some() {
            *baseline = current_cpu;
        }

        PlatformSnapshot {
            system_cpu_pct,
            memory_pressure_pct: memory.map(|value| value.0),
            swap_used_mb: memory.map(|value| value.1),
            app_cpu_pct,
            app_memory_mb: read_process_memory().map(bytes_to_mb),
            ..PlatformSnapshot::default()
        }
    }

    fn read_memory() -> Option<(f64, f64)> {
        // SAFETY: Windows fills an initialized MEMORYSTATUSEX whose length is supplied.
        let memory = unsafe {
            let mut memory: MEMORYSTATUSEX = zeroed();
            memory.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
            (GlobalMemoryStatusEx(&mut memory) != 0).then_some(memory)?
        };
        let physical_used = memory.ullTotalPhys.saturating_sub(memory.ullAvailPhys);
        let committed = memory
            .ullTotalPageFile
            .saturating_sub(memory.ullAvailPageFile);
        let swap_used = committed.saturating_sub(physical_used);
        Some((
            percent_used(physical_used, memory.ullTotalPhys)?,
            bytes_to_mb(swap_used),
        ))
    }

    fn read_process_memory() -> Option<u64> {
        // SAFETY: pseudo-handle stays valid for process lifetime; buffer size matches EX layout.
        unsafe {
            let mut counters: PROCESS_MEMORY_COUNTERS_EX = zeroed();
            counters.cb = size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
            let ok = K32GetProcessMemoryInfo(
                GetCurrentProcess(),
                (&mut counters as *mut PROCESS_MEMORY_COUNTERS_EX)
                    .cast::<PROCESS_MEMORY_COUNTERS>(),
                counters.cb,
            );
            (ok != 0).then_some(counters.PrivateUsage as u64)
        }
    }

    fn read_cpu_times() -> Option<CpuBaseline> {
        // SAFETY: all pointers reference writable FILETIME values for each API call.
        unsafe {
            let mut idle: FILETIME = zeroed();
            let mut kernel: FILETIME = zeroed();
            let mut user: FILETIME = zeroed();
            if GetSystemTimes(&mut idle, &mut kernel, &mut user) == 0 {
                return None;
            }
            let mut creation: FILETIME = zeroed();
            let mut exit: FILETIME = zeroed();
            let mut process_kernel: FILETIME = zeroed();
            let mut process_user: FILETIME = zeroed();
            if GetProcessTimes(
                GetCurrentProcess(),
                &mut creation,
                &mut exit,
                &mut process_kernel,
                &mut process_user,
            ) == 0
            {
                return None;
            }
            Some(CpuBaseline {
                idle: filetime(idle),
                kernel: filetime(kernel),
                user: filetime(user),
                process: filetime(process_kernel).saturating_add(filetime(process_user)),
            })
        }
    }

    fn filetime(value: FILETIME) -> u64 {
        ((value.dwHighDateTime as u64) << 32) | value.dwLowDateTime as u64
    }

    fn cpu_percentages(previous: CpuBaseline, current: CpuBaseline) -> Option<(f64, f64)> {
        let idle = current.idle.saturating_sub(previous.idle);
        let kernel = current.kernel.saturating_sub(previous.kernel);
        let user = current.user.saturating_sub(previous.user);
        let total = kernel.saturating_add(user);
        if total == 0 {
            return None;
        }
        let system = total.saturating_sub(idle) as f64 * 100.0 / total as f64;
        let app = current.process.saturating_sub(previous.process) as f64 * 100.0 / total as f64;
        Some((system.clamp(0.0, 100.0), app.clamp(0.0, 100.0)))
    }
}

#[cfg(target_os = "macos")]
#[allow(deprecated)]
mod platform {
    use super::{bytes_to_mb, percent_used, PlatformSnapshot};
    use std::ffi::{c_char, c_int};
    use std::mem::{size_of, zeroed};
    use std::ptr::{addr_of, null_mut};

    #[derive(Clone, Copy)]
    pub(super) struct CpuBaseline {
        system: [u32; libc::CPU_STATE_MAX as usize],
        process: u64,
        wall: u64,
    }

    pub(super) fn collect(baseline: &mut Option<CpuBaseline>) -> PlatformSnapshot {
        let current_cpu = read_cpu_times();
        let (system_cpu_pct, app_cpu_pct) = baseline
            .zip(current_cpu)
            .and_then(|(previous, current)| cpu_percentages(previous, current))
            .map(|(system, app)| (Some(system), Some(app)))
            .unwrap_or_default();
        if current_cpu.is_some() {
            *baseline = current_cpu;
        }

        let process = read_process_info();
        let physical_memory = read_physical_memory().unwrap_or(0);
        PlatformSnapshot {
            system_load_1m: read_load_average(),
            system_cpu_pct,
            memory_pressure_pct: read_used_memory()
                .and_then(|used| percent_used(used, physical_memory)),
            swap_used_mb: read_swap_used().map(bytes_to_mb),
            thermal_state: Some(thermal_state().into()),
            app_cpu_pct,
            app_memory_mb: process.map(|value| bytes_to_mb(value.pti_resident_size)),
        }
    }

    fn read_cpu_times() -> Option<CpuBaseline> {
        let process = read_process_info()?;
        // SAFETY: host_statistics writes exactly HOST_CPU_LOAD_INFO_COUNT integers.
        let system = unsafe {
            let mut info: libc::host_cpu_load_info_data_t = zeroed();
            let mut count = libc::HOST_CPU_LOAD_INFO_COUNT;
            let status = libc::host_statistics(
                libc::mach_host_self(),
                libc::HOST_CPU_LOAD_INFO,
                (&mut info as *mut libc::host_cpu_load_info_data_t).cast::<libc::integer_t>(),
                &mut count,
            );
            (status == libc::KERN_SUCCESS).then_some(info.cpu_ticks)?
        };
        Some(CpuBaseline {
            system,
            process: process
                .pti_total_user
                .saturating_add(process.pti_total_system),
            // Same Mach timebase as proc_taskinfo CPU counters.
            wall: unsafe { libc::mach_absolute_time() },
        })
    }

    fn cpu_percentages(previous: CpuBaseline, current: CpuBaseline) -> Option<(f64, f64)> {
        let delta =
            |index: usize| current.system[index].wrapping_sub(previous.system[index]) as u64;
        let busy = delta(libc::CPU_STATE_USER as usize)
            .saturating_add(delta(libc::CPU_STATE_SYSTEM as usize))
            .saturating_add(delta(libc::CPU_STATE_NICE as usize));
        let total = busy.saturating_add(delta(libc::CPU_STATE_IDLE as usize));
        let wall = current.wall.saturating_sub(previous.wall);
        if total == 0 || wall == 0 {
            return None;
        }
        let system = busy as f64 * 100.0 / total as f64;
        let app = current.process.saturating_sub(previous.process) as f64 * 100.0 / wall as f64;
        Some((system.clamp(0.0, 100.0), app.max(0.0)))
    }

    fn read_process_info() -> Option<libc::proc_taskinfo> {
        // SAFETY: proc_pidinfo receives current PID plus correctly sized writable buffer.
        unsafe {
            let mut info: libc::proc_taskinfo = zeroed();
            let size = size_of::<libc::proc_taskinfo>() as i32;
            let written = libc::proc_pidinfo(
                libc::getpid(),
                libc::PROC_PIDTASKINFO,
                0,
                (&mut info as *mut libc::proc_taskinfo).cast(),
                size,
            );
            (written == size).then_some(info)
        }
    }

    fn read_used_memory() -> Option<u64> {
        // SAFETY: host_statistics64 writes a vm_statistics64_data_t buffer with matching count.
        unsafe {
            let mut stats: libc::vm_statistics64_data_t = zeroed();
            let mut count = libc::HOST_VM_INFO64_COUNT;
            let status = libc::host_statistics64(
                libc::mach_host_self(),
                libc::HOST_VM_INFO64,
                (&mut stats as *mut libc::vm_statistics64_data_t).cast::<libc::integer_t>(),
                &mut count,
            );
            if status != libc::KERN_SUCCESS {
                return None;
            }
            let active = addr_of!(stats.active_count).read_unaligned() as u64;
            let wired = addr_of!(stats.wire_count).read_unaligned() as u64;
            let compressed = addr_of!(stats.compressor_page_count).read_unaligned() as u64;
            let page_size = libc::sysconf(libc::_SC_PAGESIZE);
            (page_size > 0).then(|| {
                active
                    .saturating_add(wired)
                    .saturating_add(compressed)
                    .saturating_mul(page_size as u64)
            })
        }
    }

    fn read_swap_used() -> Option<u64> {
        // SAFETY: sysctlbyname writes xsw_usage into correctly sized output buffer.
        unsafe {
            let mut usage: libc::xsw_usage = zeroed();
            let mut size = size_of::<libc::xsw_usage>();
            let status = libc::sysctlbyname(
                c"vm.swapusage".as_ptr(),
                (&mut usage as *mut libc::xsw_usage).cast(),
                &mut size,
                null_mut(),
                0,
            );
            (status == 0).then_some(usage.xsu_used)
        }
    }

    fn read_load_average() -> Option<f64> {
        // SAFETY: getloadavg writes one f64 into supplied one-element buffer.
        unsafe {
            let mut load = [0.0_f64; 1];
            (libc::getloadavg(load.as_mut_ptr(), 1) == 1).then_some(load[0])
        }
    }

    fn read_physical_memory() -> Option<u64> {
        // SAFETY: sysctlbyname writes one u64 into a correctly sized output buffer.
        unsafe {
            let mut value = 0_u64;
            let mut size = size_of::<u64>();
            let status = libc::sysctlbyname(
                c"hw.memsize".as_ptr(),
                (&mut value as *mut u64).cast(),
                &mut size,
                null_mut(),
                0,
            );
            (status == 0 && size == size_of::<u64>()).then_some(value)
        }
    }

    // libnotify lives in libSystem (always linked). The kernel publishes the thermal
    // pressure level under this name; it is the source behind
    // `NSProcessInfo.thermalState`, and there is no sysctl key for it.
    extern "C" {
        fn notify_register_check(name: *const c_char, token: *mut c_int) -> u32;
        fn notify_get_state(token: c_int, state: *mut u64) -> u32;
        fn notify_cancel(token: c_int) -> u32;
    }

    fn thermal_state() -> &'static str {
        let mut token: c_int = 0;
        let mut level = 0_u64;
        // SAFETY: valid NUL-terminated name and out-pointers; the token is cancelled below.
        let ok = unsafe {
            if notify_register_check(
                c"com.apple.system.thermalpressurelevel".as_ptr(),
                &mut token,
            ) != 0
            {
                return "unknown";
            }
            let status = notify_get_state(token, &mut level);
            notify_cancel(token);
            status == 0
        };
        match (ok, level) {
            (false, _) => "unknown",
            (true, 0) => "nominal",
            (true, 1) => "fair",
            (true, 2) => "serious",
            (true, 3..=4) => "critical",
            _ => "unknown",
        }
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
mod platform {
    use super::PlatformSnapshot;

    pub(super) struct CpuBaseline;

    pub(super) fn collect(_baseline: &mut Option<CpuBaseline>) -> PlatformSnapshot {
        let load = std::fs::read_to_string("/proc/loadavg")
            .ok()
            .and_then(|text| text.split_whitespace().next()?.parse().ok());
        PlatformSnapshot {
            system_load_1m: load,
            ..PlatformSnapshot::default()
        }
    }
}

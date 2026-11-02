//! Production-cadence KWS load generator for concurrent ASR benchmarks, and the
//! CPU duty / memory soak for Rust KWS qualification.
//!
//! Feeds a looped WAV through `Kws::transcribe_cascade_result_with_wake_start`
//! at a fixed wall-clock cadence under the worker's cursor contract (rolling 3 s
//! snapshot at absolute sample positions), so each call decodes only the new audio.
//! (`transcribe_result` on successive chunks is a duplicate snapshot after the
//! first call and decodes nothing.) A fired command resets the cascade and the
//! feed continues as a new session from that sample, as a new recording would.
//!
//! Usage: kws_paced_driver <model-dir> <wav> <duration-seconds> [cadence-ms]
//!          [--ort ONNXRUNTIME_LIB] [--allow-known-bad-ort]
//!          [--out REPORT.json]
//! Runbook:
//! docs/RUNBOOK_RUST_KWS_QUALIFICATION.md.

use rightkit_dictation_engine::kws::Kws;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const SAMPLE_RATE: usize = 16_000;
/// The worker's rolling probe snapshot: the last three seconds of PCM.
const SNAPSHOT_SAMPLES: usize = SAMPLE_RATE * 3;
const USAGE: &str = "usage: kws_paced_driver <model-dir> <wav> <duration-seconds> [cadence-ms] \
[--ort ONNXRUNTIME_LIB] [--allow-known-bad-ort] [--out REPORT.json]";

#[derive(Serialize)]
struct Report {
    schema: &'static str,
    wav: PathBuf,
    cadence_ms: u64,
    duration_s: f64,
    load_ms: f64,
    calls: usize,
    detections: usize,
    audio_fed_s: f64,
    compute_ms: f64,
    duty_pct: f64,
    call_p50_ms: f64,
    call_p95_ms: f64,
    call_max_ms: f64,
    cpu_ms: Option<f64>,
    cpu_pct_of_wall: Option<f64>,
    memory_after_load: proc_stats::Memory,
    memory_end: proc_stats::Memory,
}

fn read_wav(path: &Path) -> Result<Vec<f32>, String> {
    let wav = rightkit_audio::wav::read_wav_mono_f32(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if wav.sample_rate != SAMPLE_RATE as u32 {
        return Err(format!(
            "{}: expected {} Hz WAV, got {} Hz",
            path.display(),
            SAMPLE_RATE as u32,
            wav.sample_rate
        ));
    }
    Ok(wav.samples)
}

/// The developer override, else the staged runtime, else the committed source.
fn default_ort_runtime() -> Option<PathBuf> {
    let name = if cfg!(target_os = "windows") {
        "onnxruntime.dll"
    } else if cfg!(target_os = "macos") {
        "libonnxruntime.dylib"
    } else {
        "libonnxruntime.so"
    };
    let src_tauri = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../src-tauri");
    std::env::var_os("ORT_DYLIB_PATH")
        .map(PathBuf::from)
        .into_iter()
        .chain([
            src_tauri.join("resources/runtime").join(name),
            src_tauri.join("native-sources").join(name),
        ])
        .find(|path| path.is_file())
}

fn bind_ort(explicit: Option<&Path>, allow_known_bad: bool) -> Result<(), String> {
    let Some(path) = explicit.map(Path::to_path_buf).or_else(default_ort_runtime) else {
        return Ok(());
    };
    let path = std::path::absolute(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    rightkit_onnx::configure_runtime_with_options(
        [rightkit_onnx::explicit_file_candidate(&path)],
        &rightkit_onnx::RuntimeBindingOptions {
            force_replace: true,
            allow_known_bad_runtime: allow_known_bad,
            ..Default::default()
        },
    )
    .map(|_| ())
    .map_err(|e| format!("bind ONNX Runtime {}: {e}", path.display()))
}

fn percentile(sorted: &[f64], quantile: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() as f64 * quantile).ceil() as usize).clamp(1, sorted.len()) - 1]
}

fn main() -> Result<(), String> {
    let mut positional = Vec::new();
    let mut ort = None;
    let mut allow_known_bad_ort = false;
    let mut out = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |flag: &str| args.next().ok_or_else(|| format!("{flag} needs a value"));
        match arg.as_str() {
            "--ort" => ort = Some(PathBuf::from(value("--ort")?)),
            "--allow-known-bad-ort" => allow_known_bad_ort = true,
            "--out" => out = Some(PathBuf::from(value("--out")?)),
            flag if flag.starts_with("--") => return Err(format!("unknown flag {flag}\n{USAGE}")),
            _ => positional.push(arg),
        }
    }
    if positional.len() < 3 || positional.len() > 4 {
        return Err(USAGE.into());
    }
    let duration = Duration::from_secs(
        positional[2]
            .parse::<u64>()
            .map_err(|error| error.to_string())?,
    );
    let cadence = Duration::from_millis(
        positional
            .get(3)
            .map(String::as_str)
            .unwrap_or("100")
            .parse::<u64>()
            .map_err(|error| error.to_string())?,
    );
    let chunk_samples = SAMPLE_RATE * cadence.as_millis() as usize / 1_000;
    if chunk_samples == 0 {
        return Err("cadence must produce at least one sample".into());
    }

    let wav_path = PathBuf::from(&positional[1]);
    let audio = read_wav(&wav_path)?;
    if audio.is_empty() {
        return Err("input WAV is empty".into());
    }
    bind_ort(ort.as_deref(), allow_known_bad_ort)?;
    let load_started = Instant::now();
    let mut kws = Kws::load(Path::new(&positional[0]))?;
    let load_ms = load_started.elapsed().as_secs_f64() * 1_000.0;
    let memory_after_load = proc_stats::memory();
    let cpu_started = proc_stats::cpu();
    let started = Instant::now();
    let mut next_tick = started;
    let mut total = 0usize;
    let mut armed = false;
    // A new session never re-reads audio from before its own start.
    let mut session_start = 0usize;
    let mut calls = 0usize;
    let mut detections = 0usize;
    let mut compute = Duration::ZERO;
    let mut call_ms = Vec::new();
    let mut snapshot = Vec::with_capacity(SNAPSHOT_SAMPLES);

    while started.elapsed() < duration {
        total += chunk_samples;
        let start = total.saturating_sub(SNAPSHOT_SAMPLES).max(session_start);
        snapshot.clear();
        snapshot.extend((start..total).map(|sample| audio[sample % audio.len()]));
        let call_started = Instant::now();
        let (result, _) =
            kws.transcribe_cascade_result_with_wake_start(&snapshot, start, total, armed)?;
        match result.text.as_str() {
            "zephyr" => armed = true,
            "" => {}
            _ => {
                detections += 1;
                armed = false;
                kws.reset_stream()?;
                session_start = total;
            }
        }
        let elapsed = call_started.elapsed();
        compute += elapsed;
        call_ms.push(elapsed.as_secs_f64() * 1_000.0);
        calls += 1;
        next_tick += cadence;
        if let Some(remaining) = next_tick.checked_duration_since(Instant::now()) {
            std::thread::sleep(remaining);
        }
    }

    let wall = started.elapsed().as_secs_f64();
    let cpu_ms = cpu_started
        .zip(proc_stats::cpu())
        .map(|(before, after)| after.total_ms() - before.total_ms());
    call_ms.sort_by(f64::total_cmp);
    let report = Report {
        schema: "heardright.kws_paced_driver.v1",
        wav: wav_path,
        cadence_ms: cadence.as_millis() as u64,
        duration_s: wall,
        load_ms,
        calls,
        detections,
        audio_fed_s: total as f64 / SAMPLE_RATE as f64,
        compute_ms: compute.as_secs_f64() * 1_000.0,
        duty_pct: compute.as_secs_f64() / wall * 100.0,
        call_p50_ms: percentile(&call_ms, 0.50),
        call_p95_ms: percentile(&call_ms, 0.95),
        call_max_ms: call_ms.last().copied().unwrap_or(0.0),
        cpu_ms,
        cpu_pct_of_wall: cpu_ms.map(|cpu| cpu / (wall * 1_000.0) * 100.0),
        memory_after_load,
        memory_end: proc_stats::memory(),
    };
    let mb = |bytes: Option<u64>| {
        bytes.map_or_else(
            || "n/a".into(),
            |bytes| format!("{:.1}", bytes as f64 / 1_048_576.0),
        )
    };
    println!(
        "calls={} detections={} compute_ms={:.3} duty_pct={:.3} p50_ms={:.3} p95_ms={:.3} \
max_ms={:.3} cpu_pct={} resident_mb_load={} resident_mb_end={} footprint_mb_end={}",
        report.calls,
        report.detections,
        report.compute_ms,
        report.duty_pct,
        report.call_p50_ms,
        report.call_p95_ms,
        report.call_max_ms,
        report
            .cpu_pct_of_wall
            .map_or_else(|| "n/a".into(), |pct| format!("{pct:.3}")),
        mb(report.memory_after_load.resident_bytes),
        mb(report.memory_end.resident_bytes),
        mb(report.memory_end.footprint_bytes),
    );
    if let Some(out) = out {
        let mut encoded = serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?;
        encoded.push(b'\n');
        std::fs::write(&out, encoded).map_err(|e| format!("{}: {e}", out.display()))?;
        println!("report={}", out.display());
    }
    Ok(())
}

/// Process CPU and memory counters (all threads). Same counters as the KWS runtime soak tests.
mod proc_stats {
    use serde::Serialize;

    #[derive(Clone, Copy, Debug, Default, Serialize)]
    pub struct Cpu {
        pub user_ms: f64,
        pub sys_ms: f64,
    }

    impl Cpu {
        pub fn total_ms(self) -> f64 {
            self.user_ms + self.sys_ms
        }
    }

    /// `resident`: RSS / working set. `footprint`: macOS `phys_footprint`, Windows
    /// private commit (`PagefileUsage`); none on Linux.
    #[derive(Clone, Copy, Debug, Default, Serialize)]
    pub struct Memory {
        pub resident_bytes: Option<u64>,
        pub peak_resident_bytes: Option<u64>,
        pub footprint_bytes: Option<u64>,
        pub peak_footprint_bytes: Option<u64>,
    }

    #[cfg(unix)]
    fn rusage() -> Option<libc::rusage> {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        // SAFETY: getrusage fills the struct it is handed; RUSAGE_SELF is always valid.
        (unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } == 0)
            // SAFETY: zero-initialised and filled by a successful getrusage.
            .then(|| unsafe { usage.assume_init() })
    }

    #[cfg(unix)]
    pub fn cpu() -> Option<Cpu> {
        let usage = rusage()?;
        let ms = |time: libc::timeval| time.tv_sec as f64 * 1_000.0 + time.tv_usec as f64 / 1_000.0;
        Some(Cpu {
            user_ms: ms(usage.ru_utime),
            sys_ms: ms(usage.ru_stime),
        })
    }

    #[cfg(target_os = "linux")]
    pub fn memory() -> Memory {
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        let kib = |key: &str| {
            status
                .lines()
                .find_map(|line| line.strip_prefix(key))
                .and_then(|value| {
                    value
                        .trim()
                        .trim_end_matches("kB")
                        .trim()
                        .parse::<u64>()
                        .ok()
                })
                .map(|kib| kib * 1_024)
        };
        Memory {
            resident_bytes: kib("VmRSS:"),
            peak_resident_bytes: kib("VmHWM:"),
            footprint_bytes: None,
            peak_footprint_bytes: None,
        }
    }

    #[cfg(target_os = "macos")]
    pub fn memory() -> Memory {
        let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::zeroed();
        // SAFETY: proc_pid_rusage writes one rusage_info_v4 for RUSAGE_INFO_V4.
        let info = (unsafe {
            libc::proc_pid_rusage(
                libc::getpid(),
                libc::RUSAGE_INFO_V4,
                info.as_mut_ptr().cast::<libc::rusage_info_t>(),
            )
        } == 0)
            // SAFETY: zero-initialised and filled by a successful proc_pid_rusage.
            .then(|| unsafe { info.assume_init() });
        Memory {
            resident_bytes: info.map(|info| info.ri_resident_size),
            // macOS reports ru_maxrss in bytes.
            peak_resident_bytes: rusage().map(|usage| usage.ru_maxrss as u64),
            footprint_bytes: info.map(|info| info.ri_phys_footprint),
            peak_footprint_bytes: info.map(|info| info.ri_lifetime_max_phys_footprint),
        }
    }

    #[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
    pub fn memory() -> Memory {
        Memory::default()
    }

    #[cfg(windows)]
    mod win {
        use std::ffi::c_void;

        #[repr(C)]
        #[derive(Clone, Copy, Default)]
        pub struct FileTime {
            pub low: u32,
            pub high: u32,
        }

        #[repr(C)]
        #[derive(Default)]
        pub struct ProcessMemoryCounters {
            pub cb: u32,
            pub page_fault_count: u32,
            pub peak_working_set_size: usize,
            pub working_set_size: usize,
            pub quota_peak_paged_pool_usage: usize,
            pub quota_paged_pool_usage: usize,
            pub quota_peak_non_paged_pool_usage: usize,
            pub quota_non_paged_pool_usage: usize,
            pub pagefile_usage: usize,
            pub peak_pagefile_usage: usize,
        }

        #[link(name = "kernel32")]
        unsafe extern "system" {
            pub fn GetCurrentProcess() -> *mut c_void;
            pub fn GetProcessTimes(
                process: *mut c_void,
                creation: *mut FileTime,
                exit: *mut FileTime,
                kernel: *mut FileTime,
                user: *mut FileTime,
            ) -> i32;
            pub fn K32GetProcessMemoryInfo(
                process: *mut c_void,
                counters: *mut ProcessMemoryCounters,
                size: u32,
            ) -> i32;
        }
    }

    #[cfg(windows)]
    pub fn cpu() -> Option<Cpu> {
        let mut creation = win::FileTime::default();
        let mut exit = win::FileTime::default();
        let mut kernel = win::FileTime::default();
        let mut user = win::FileTime::default();
        // SAFETY: the pseudo-handle is always valid; the four out-pointers are live locals.
        let ok = unsafe {
            win::GetProcessTimes(
                win::GetCurrentProcess(),
                &mut creation,
                &mut exit,
                &mut kernel,
                &mut user,
            )
        } != 0;
        let ms = |time: win::FileTime| {
            ((u64::from(time.high) << 32) | u64::from(time.low)) as f64 / 10_000.0
        };
        ok.then(|| Cpu {
            user_ms: ms(user),
            sys_ms: ms(kernel),
        })
    }

    #[cfg(windows)]
    pub fn memory() -> Memory {
        let mut counters = win::ProcessMemoryCounters {
            cb: std::mem::size_of::<win::ProcessMemoryCounters>() as u32,
            ..Default::default()
        };
        // SAFETY: cb names the struct size; the pseudo-handle is always valid.
        let ok = unsafe {
            win::K32GetProcessMemoryInfo(win::GetCurrentProcess(), &mut counters, counters.cb)
        } != 0;
        if !ok {
            return Memory::default();
        }
        Memory {
            resident_bytes: Some(counters.working_set_size as u64),
            peak_resident_bytes: Some(counters.peak_working_set_size as u64),
            footprint_bytes: Some(counters.pagefile_usage as u64),
            peak_footprint_bytes: Some(counters.peak_pagefile_usage as u64),
        }
    }
}

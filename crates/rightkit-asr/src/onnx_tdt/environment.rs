//! Windows ORT initialization before any ASR, KWS or VAD worker starts.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    OnceLock,
};

use rightkit_onnx::{
    configured_runtime, init_environment_with_options, CandidateSource, EnvironmentInitOptions,
    GlobalPool, RuntimeSelection,
};

static ENVIRONMENT: OnceLock<bool> = OnceLock::new();
static PRIVATE_POOLS: AtomicBool = AtomicBool::new(false);

fn cpu_threads(physical: usize) -> usize {
    if physical <= 4 {
        physical.saturating_sub(1).max(1)
    } else {
        4
    }
}

fn physical_cores() -> Option<usize> {
    use windows_sys::Win32::System::SystemInformation::{
        GetLogicalProcessorInformationEx, RelationProcessorCore,
    };
    let mut bytes = 0;
    unsafe {
        GetLogicalProcessorInformationEx(RelationProcessorCore, std::ptr::null_mut(), &mut bytes);
    }
    if bytes < 8 {
        return None;
    }
    // u64 storage supplies required alignment; record sizes are variable.
    let mut storage = vec![0_u64; (bytes as usize).div_ceil(8)];
    if unsafe {
        GetLogicalProcessorInformationEx(
            RelationProcessorCore,
            storage.as_mut_ptr().cast(),
            &mut bytes,
        )
    } == 0
    {
        return None;
    }
    let data = unsafe { std::slice::from_raw_parts(storage.as_ptr().cast::<u8>(), bytes as usize) };
    count_core_records(data)
}

fn count_core_records(data: &[u8]) -> Option<usize> {
    let mut offset = 0;
    let mut cores = 0;
    while offset < data.len() {
        let header = data.get(offset..offset + 8)?;
        let relation = u32::from_ne_bytes(header[..4].try_into().ok()?);
        let size = u32::from_ne_bytes(header[4..].try_into().ok()?) as usize;
        if relation != 0 || size < 8 || size > data.len() - offset {
            return None;
        }
        cores += 1;
        offset += size;
    }
    (cores > 0).then_some(cores)
}

/// Physical core count, queried once. `None` when the topology query fails.
pub(crate) fn physical_core_count() -> Option<usize> {
    static CORES: OnceLock<Option<usize>> = OnceLock::new();
    *CORES.get_or_init(physical_cores)
}

pub(crate) fn windows_cpu_threads() -> usize {
    static THREADS: OnceLock<usize> = OnceLock::new();
    *THREADS.get_or_init(|| {
        let physical = physical_core_count().unwrap_or(1);
        let available = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1);
        let threads = cpu_threads(physical).min(available);
        tracing::info!(
            physical_cores = physical,
            available_cpus = available,
            intra_threads = threads,
            "Windows CPU thread budget"
        );
        threads
    })
}

pub(crate) fn use_private_pools(reason: &str) {
    if !PRIVATE_POOLS.swap(true, Ordering::AcqRel) {
        tracing::warn!(
            reason,
            spinning = false,
            "Windows ORT falling back to per-session CPU pools"
        );
    }
}

/// Engine entry calls this before worker creation, including Whisper startup.
/// Materialize the C environment now: commit alone only stores Rust options.
pub fn init_windows_ort() -> crate::onnx_tdt::Result<()> {
    windows_shared_pool_active().map(|_| ())
}

pub(crate) fn windows_shared_pool_active() -> crate::onnx_tdt::Result<bool> {
    let shared = *ENVIRONMENT.get_or_init(|| {
        let result = (|| -> std::result::Result<bool, String> {
            // Runtime is bound by the app entry (rightkit_onnx::configure_runtime).
            let path = configured_runtime()
                .ok_or("ORT runtime is not configured; bind it before engine startup")?;
            let selection = RuntimeSelection {
                path,
                source: CandidateSource::Explicit,
                diagnostics: Vec::new(),
            };
            // Deferred: commits options only (telemetry off, shared pool 1 inter / windows_cpu_threads intra,
            // no spin). The C environment is materialized below through rightkit_onnx::ort_environment().
            init_environment_with_options(
                &selection,
                &EnvironmentInitOptions {
                    global_pool: Some(GlobalPool {
                        intra_threads: windows_cpu_threads(),
                        inter_threads: 1,
                        spin: false,
                    }),
                    defer_initialization: true,
                    ..EnvironmentInitOptions::default()
                },
            )?;

            // Materialize the one process environment through RightKit (ort rc.14 owns a single
            // Environment; building one directly is refused). On failure sessions use private pools.
            rightkit_onnx::ort_environment()
                .map_err(|error| format!("shared environment creation: {error}"))?;
            tracing::info!(
                inter_threads = 1,
                spinning = false,
                "Windows ORT environment initialized; shared CPU pool requested"
            );
            Ok(true)
        })();
        match result {
            Ok(shared) => shared,
            Err(error) => {
                use_private_pools(&error);
                false
            }
        }
    });
    Ok(shared && !PRIVATE_POOLS.load(Ordering::Acquire))
}

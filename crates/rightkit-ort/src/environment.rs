//! Process-wide ORT environment initialisation (merged from HeardRight
//! `heardright-onnx-asr/environment.rs`, product experiment switches removed).
//!
//! Call [`init_environment`] once, before any session is built or any worker
//! thread starts. It loads the selected runtime explicitly, disables telemetry,
//! optionally requests one shared global CPU thread pool, and materialises the
//! C environment immediately (committing only stores Rust-side options).
//! If a shared pool cannot be had, sessions fall back to private pools and the
//! report says so; nothing is silently reconfigured.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    OnceLock,
};

use crate::RuntimeSelection;

static REPORT: OnceLock<Result<EnvironmentReport, String>> = OnceLock::new();
static PRIVATE_POOLS: AtomicBool = AtomicBool::new(false);

/// Shared CPU pool request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlobalPool {
    pub intra_threads: usize,
    pub inter_threads: usize,
    /// Busy-wait in idle workers. Off by default: HeardRight measured spinning
    /// as pure CPU cost for bursty dictation workloads.
    pub spin: bool,
}

impl GlobalPool {
    /// Budget derived from physical cores via [`cpu_thread_budget`], one inter
    /// thread, no spinning.
    pub fn from_budget() -> Self {
        Self {
            intra_threads: cpu_thread_budget(physical_cores().unwrap_or(1))
                .min(std::thread::available_parallelism().map_or(1, usize::from)),
            inter_threads: 1,
            spin: false,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct EnvironmentOptions {
    /// `None` keeps per-session private pools.
    pub global_pool: Option<GlobalPool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentReport {
    pub runtime_path: std::path::PathBuf,
    /// ORT build info string reported by the loaded library.
    pub runtime_info: String,
    pub shared_pool_requested: bool,
    pub shared_pool_active: bool,
}

/// HeardRight's measured budget: leave headroom on small machines, cap at four.
pub fn cpu_thread_budget(physical: usize) -> usize {
    if physical <= 4 {
        physical.saturating_sub(1).max(1)
    } else {
        4
    }
}

#[cfg(target_os = "windows")]
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
        GetLogicalProcessorInformationEx(RelationProcessorCore, storage.as_mut_ptr().cast(), &mut bytes)
    } == 0
    {
        return None;
    }
    let data = unsafe { std::slice::from_raw_parts(storage.as_ptr().cast::<u8>(), bytes as usize) };
    count_core_records(data)
}

#[cfg(not(target_os = "windows"))]
fn physical_cores() -> Option<usize> {
    std::thread::available_parallelism().ok().map(usize::from)
}

/// Count `RelationProcessorCore` records in a `GetLogicalProcessorInformationEx`
/// buffer; rejects truncated or foreign records. Public so Windows hosts and
/// tests can validate topology buffers.
pub fn count_core_records(data: &[u8]) -> Option<usize> {
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

pub(crate) fn use_private_pools(reason: &str) {
    PRIVATE_POOLS.store(true, Ordering::Release);
    let _ = reason;
}

/// True once [`init_environment`] succeeded with a shared pool and no session
/// has since fallen back to private pools.
pub fn shared_pool_active() -> bool {
    matches!(REPORT.get(), Some(Ok(r)) if r.shared_pool_active) && !PRIVATE_POOLS.load(Ordering::Acquire)
}

/// Initialise ORT exactly once for the process. Later calls return the first
/// outcome unchanged (options of later calls are ignored).
pub fn init_environment(
    selection: &RuntimeSelection,
    options: &EnvironmentOptions,
) -> Result<EnvironmentReport, String> {
    REPORT
        .get_or_init(|| init_inner(selection, options))
        .clone()
}

fn init_inner(
    selection: &RuntimeSelection,
    options: &EnvironmentOptions,
) -> Result<EnvironmentReport, String> {
    let mut builder = ort::init_from(&selection.path)
        .map_err(|e| format!("load {}: {e}", selection.path.display()))?
        .with_telemetry(false);
    let mut shared_requested = false;
    if let Some(pool) = options.global_pool {
        shared_requested = true;
        let pool_options = ort::environment::GlobalThreadPoolOptions::default()
            .with_intra_threads(pool.intra_threads)
            .map_err(|e| e.to_string())?
            .with_inter_threads(pool.inter_threads)
            .map_err(|e| e.to_string())?
            .with_spin_control(pool.spin)
            .map_err(|e| e.to_string())?;
        builder = builder.with_global_thread_pool(pool_options);
    }
    if !builder.commit() {
        return Err("ORT was configured before rightkit-ort::init_environment".into());
    }
    // Materialise the C environment now so failures surface at startup.
    ort::environment::Environment::current().map_err(|e| format!("environment creation: {e}"))?;
    Ok(EnvironmentReport {
        runtime_path: selection.path.clone(),
        runtime_info: ort::info().to_owned(),
        shared_pool_requested: shared_requested,
        shared_pool_active: shared_requested,
    })
}

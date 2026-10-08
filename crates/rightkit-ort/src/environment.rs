//! Process-wide ORT environment initialisation (merged from HeardRight
//! `heardright-onnx-asr/environment.rs`, product experiment switches removed).
//!
//! Call [`init_environment`] once, before any session is built or any worker
//! thread starts. It loads the selected runtime explicitly, disables telemetry,
//! optionally requests one shared global CPU thread pool, and materialises the
//! C environment immediately by default (committing only stores Rust-side
//! options). Deferred mode still checks/loads the dylib, but leaves C environment
//! creation to the first ORT use, matching `ort::init_from(...).commit()`.
//! If a shared pool cannot be had, sessions fall back to private pools and the
//! report says so; nothing is silently reconfigured.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    OnceLock,
};

use crate::RuntimeSelection;

static REPORT: OnceLock<Result<EnvironmentInitReport, String>> = OnceLock::new();
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

/// Original eager-initialization options, retained for existing struct literals.
#[derive(Debug, Clone, Default)]
pub struct EnvironmentOptions {
    /// `None` keeps per-session private pools.
    pub global_pool: Option<GlobalPool>,
}

/// Extended initialization policy; defaults match [`EnvironmentOptions`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentInitOptions {
    /// `None` keeps per-session private pools.
    pub global_pool: Option<GlobalPool>,
    /// Commit options now, create the C environment on first use. The dylib
    /// is still loaded now, so loader failures remain reportable startup errors.
    pub defer_initialization: bool,
    /// `Some(false)` disables telemetry (default); `None` leaves ort's builder
    /// default unchanged; `Some(true)` explicitly enables telemetry.
    pub telemetry: Option<bool>,
    /// Accept an environment configured outside this crate. Repeated successful
    /// calls report AlreadyInitialized; existing options are never replaced.
    pub tolerate_already_initialized: bool,
    /// Return the loader's Display message exactly, without a path prefix.
    pub verbatim_loader_errors: bool,
}

impl Default for EnvironmentInitOptions {
    fn default() -> Self {
        Self {
            global_pool: None,
            defer_initialization: false,
            telemetry: Some(false),
            tolerate_already_initialized: false,
            verbatim_loader_errors: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvironmentStatus {
    Initialized,
    /// Options committed; C environment creation deferred to first use.
    Deferred,
    /// Earlier configuration retained, including configuration outside this crate.
    AlreadyInitialized,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentReport {
    pub runtime_path: std::path::PathBuf,
    /// ORT build info string reported by the loaded library.
    pub runtime_info: String,
    pub shared_pool_requested: bool,
    pub shared_pool_active: bool,
}

/// Extended report for [`init_environment_with_options`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentInitReport {
    /// Selected path; when external configuration already exists, ort retains
    /// its loaded library & this path is not proof of that library's identity.
    pub runtime_path: std::path::PathBuf,
    /// ORT build info; empty when C environment initialization was deferred.
    pub runtime_info: String,
    pub shared_pool_requested: bool,
    pub shared_pool_active: bool,
    pub status: EnvironmentStatus,
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

/// True when this crate committed a shared pool configuration. In deferred
/// mode, ORT creates that pool on first use before session options consult it.
/// Externally configured pools are unknown & conservatively return false.
pub fn shared_pool_active() -> bool {
    matches!(REPORT.get(), Some(Ok(r)) if r.shared_pool_active
        || (r.status == EnvironmentStatus::Deferred && r.shared_pool_requested))
        && !PRIVATE_POOLS.load(Ordering::Acquire)
}

/// Initialise ORT exactly once for the process. Later calls return the first
/// outcome unchanged (options of later calls are ignored).
pub fn init_environment(
    selection: &RuntimeSelection,
    options: &EnvironmentOptions,
) -> Result<EnvironmentReport, String> {
    init_environment_with_options(
        selection,
        &EnvironmentInitOptions {
            global_pool: options.global_pool,
            ..EnvironmentInitOptions::default()
        },
    )
    .map(|report| EnvironmentReport {
        runtime_path: report.runtime_path,
        runtime_info: report.runtime_info,
        shared_pool_requested: report.shared_pool_requested,
        shared_pool_active: report.shared_pool_active,
    })
}

/// Initialise once under extended policy. Later calls reuse the first outcome;
/// tolerance reports [`EnvironmentStatus::AlreadyInitialized`] on repeated
/// success & accepts a configuration committed outside this crate. Neither
/// existing options nor a loaded runtime can be replaced through this API.
pub fn init_environment_with_options(
    selection: &RuntimeSelection,
    options: &EnvironmentInitOptions,
) -> Result<EnvironmentInitReport, String> {
    init_once_with(&REPORT, options, || {
        init_inner_with(selection, options, &mut OrtBackend)
    })
}

// A caller-local OnceLock & backend keep tests independent of process ORT state.
fn init_once_with(
    report: &OnceLock<Result<EnvironmentInitReport, String>>,
    options: &EnvironmentInitOptions,
    init: impl FnOnce() -> Result<EnvironmentInitReport, String>,
) -> Result<EnvironmentInitReport, String> {
    let mut initialized_here = false;
    let result = report.get_or_init(|| {
        initialized_here = true;
        init()
    });
    let mut result = result.clone();
    if !initialized_here && options.tolerate_already_initialized {
        if let Ok(report) = &mut result {
            report.status = EnvironmentStatus::AlreadyInitialized;
        }
    }
    result
}

trait EnvironmentBackend {
    type Builder;

    fn load(&mut self, path: &std::path::Path) -> Result<Self::Builder, String>;
    fn configure(
        &mut self,
        builder: Self::Builder,
        options: &EnvironmentInitOptions,
    ) -> Result<bool, String>;
    fn initialize(&mut self) -> Result<String, String>;
}

fn init_inner_with(
    selection: &RuntimeSelection,
    options: &EnvironmentInitOptions,
    backend: &mut impl EnvironmentBackend,
) -> Result<EnvironmentInitReport, String> {
    let builder = backend.load(&selection.path).map_err(|error| {
        if options.verbatim_loader_errors {
            error
        } else {
            format!("load {}: {error}", selection.path.display())
        }
    })?;
    let committed = backend.configure(builder, options)?;
    if !committed && !options.tolerate_already_initialized {
        return Err("ORT was configured before rightkit-ort::init_environment".into());
    }
    let status = if !committed {
        EnvironmentStatus::AlreadyInitialized
    } else if options.defer_initialization {
        EnvironmentStatus::Deferred
    } else {
        EnvironmentStatus::Initialized
    };
    let runtime_info = if options.defer_initialization {
        String::new()
    } else {
        backend
            .initialize()
            .map_err(|error| format!("environment creation: {error}"))?
    };
    let shared_requested = options.global_pool.is_some();
    Ok(EnvironmentInitReport {
        runtime_path: selection.path.clone(),
        runtime_info,
        shared_pool_requested: shared_requested,
        shared_pool_active: committed && shared_requested && !options.defer_initialization,
        status,
    })
}

struct OrtBackend;

impl EnvironmentBackend for OrtBackend {
    type Builder = ort::environment::EnvironmentBuilder;

    fn load(&mut self, path: &std::path::Path) -> Result<Self::Builder, String> {
        ort::init_from(path).map_err(|error| error.to_string())
    }

    fn configure(
        &mut self,
        mut builder: Self::Builder,
        options: &EnvironmentInitOptions,
    ) -> Result<bool, String> {
        if let Some(telemetry) = options.telemetry {
            builder = builder.with_telemetry(telemetry);
        }
        if let Some(pool) = options.global_pool {
            let pool_options = ort::environment::GlobalThreadPoolOptions::default()
                .with_intra_threads(pool.intra_threads)
                .map_err(|error| error.to_string())?
                .with_inter_threads(pool.inter_threads)
                .map_err(|error| error.to_string())?
                .with_spin_control(pool.spin)
                .map_err(|error| error.to_string())?;
            builder = builder.with_global_thread_pool(pool_options);
        }
        Ok(builder.commit())
    }

    fn initialize(&mut self) -> Result<String, String> {
        ort::environment::Environment::current().map_err(|error| error.to_string())?;
        Ok(ort::info().to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CandidateSource;

    struct FakeBackend {
        committed: bool,
        load_error: Option<String>,
        initialize_error: Option<String>,
        configured: Option<EnvironmentInitOptions>,
        initializations: usize,
    }

    impl Default for FakeBackend {
        fn default() -> Self {
            Self {
                committed: true,
                load_error: None,
                initialize_error: None,
                configured: None,
                initializations: 0,
            }
        }
    }

    impl EnvironmentBackend for FakeBackend {
        type Builder = ();

        fn load(&mut self, _: &std::path::Path) -> Result<(), String> {
            match &self.load_error {
                Some(error) => Err(error.clone()),
                None => Ok(()),
            }
        }

        fn configure(&mut self, _: (), options: &EnvironmentInitOptions) -> Result<bool, String> {
            self.configured = Some(options.clone());
            Ok(self.committed)
        }

        fn initialize(&mut self) -> Result<String, String> {
            self.initializations += 1;
            match &self.initialize_error {
                Some(error) => Err(error.clone()),
                None => Ok("ORT Build Info: seam".into()),
            }
        }
    }

    fn selection() -> RuntimeSelection {
        RuntimeSelection {
            path: std::env::temp_dir().join(crate::runtime_filename()),
            source: CandidateSource::Explicit,
            diagnostics: Vec::new(),
        }
    }

    #[test]
    fn defaults_remain_eager_without_telemetry_or_shared_pool() {
        // Original public struct literals remain source-compatible.
        let legacy = EnvironmentOptions { global_pool: None };
        assert_eq!(
            legacy.global_pool,
            EnvironmentOptions::default().global_pool
        );
        let options = EnvironmentInitOptions::default();
        assert_eq!(options.global_pool, None);
        assert!(!options.defer_initialization);
        assert_eq!(options.telemetry, Some(false));
        assert!(!options.tolerate_already_initialized);
        assert!(!options.verbatim_loader_errors);
        let mut backend = FakeBackend::default();
        let report = init_inner_with(&selection(), &options, &mut backend).unwrap();
        assert_eq!(report.status, EnvironmentStatus::Initialized);
        assert_eq!(backend.initializations, 1);
        assert_eq!(backend.configured, Some(options));
        assert!(!report.shared_pool_active);
        assert!(!report.shared_pool_requested);
    }

    #[test]
    fn deferred_mode_commits_options_with_default_telemetry_without_creating_environment() {
        let options = EnvironmentInitOptions {
            defer_initialization: true,
            telemetry: None,
            global_pool: Some(GlobalPool {
                intra_threads: 2,
                inter_threads: 1,
                spin: false,
            }),
            ..Default::default()
        };
        let mut backend = FakeBackend::default();
        let report = init_inner_with(&selection(), &options, &mut backend).unwrap();
        assert_eq!(report.status, EnvironmentStatus::Deferred);
        assert_eq!(backend.configured, Some(options));
        assert_eq!(backend.initializations, 0);
        assert!(report.runtime_info.is_empty());
        assert!(report.shared_pool_requested);
        assert!(!report.shared_pool_active);
    }

    #[test]
    fn tolerant_second_init_reports_already_initialized_without_reconfiguration() {
        let cache = OnceLock::new();
        let mut backend = FakeBackend::default();
        let options = EnvironmentInitOptions {
            tolerate_already_initialized: true,
            ..Default::default()
        };
        let first = init_once_with(&cache, &options, || {
            init_inner_with(&selection(), &options, &mut backend)
        })
        .unwrap();
        let second =
            init_once_with(&cache, &options, || panic!("must not initialize twice")).unwrap();
        assert_eq!(first.status, EnvironmentStatus::Initialized);
        assert_eq!(second.status, EnvironmentStatus::AlreadyInitialized);
        assert_eq!(second.runtime_path, first.runtime_path);
        assert_eq!(second.runtime_info, first.runtime_info);
        assert_eq!(backend.initializations, 1);
        let default_repeat = init_once_with(&cache, &EnvironmentInitOptions::default(), || {
            panic!("must reuse cache")
        })
        .unwrap();
        assert_eq!(default_repeat, first);
    }

    #[test]
    fn external_configuration_is_tolerated_only_when_requested() {
        let mut backend = FakeBackend {
            committed: false,
            ..Default::default()
        };
        assert_eq!(
            init_inner_with(
                &selection(),
                &EnvironmentInitOptions::default(),
                &mut backend
            )
            .unwrap_err(),
            "ORT was configured before rightkit-ort::init_environment"
        );
        assert_eq!(backend.initializations, 0);
        let options = EnvironmentInitOptions {
            tolerate_already_initialized: true,
            global_pool: Some(GlobalPool {
                intra_threads: 2,
                inter_threads: 1,
                spin: false,
            }),
            ..Default::default()
        };
        let report = init_inner_with(&selection(), &options, &mut backend).unwrap();
        assert_eq!(report.status, EnvironmentStatus::AlreadyInitialized);
        assert!(report.shared_pool_requested);
        assert!(!report.shared_pool_active); // Never attribute ignored pool options to ORT.
        assert_eq!(backend.initializations, 1);
        let deferred = EnvironmentInitOptions {
            defer_initialization: true,
            ..options
        };
        let _ = init_inner_with(&selection(), &deferred, &mut backend).unwrap();
        assert_eq!(backend.initializations, 1);
    }

    #[test]
    fn loader_errors_are_verbatim_only_when_requested_and_cached_errors_stay_errors() {
        let message = "loader original: missing dependency\n  native detail";
        let mut backend = FakeBackend {
            load_error: Some(message.into()),
            ..Default::default()
        };
        let selected = selection();
        let contextual =
            init_inner_with(&selected, &EnvironmentInitOptions::default(), &mut backend)
                .unwrap_err();
        assert_eq!(
            contextual,
            format!("load {}: {message}", selected.path.display())
        );
        let options = EnvironmentInitOptions {
            verbatim_loader_errors: true,
            tolerate_already_initialized: true,
            ..Default::default()
        };
        let cache = OnceLock::new();
        let error = init_once_with(&cache, &options, || {
            init_inner_with(&selected, &options, &mut backend)
        })
        .unwrap_err();
        assert_eq!(error, message);
        assert_eq!(backend.initializations, 0);
        assert!(backend.configured.is_none());
        assert_eq!(
            init_once_with(&cache, &options, || panic!("must preserve failure")).unwrap_err(),
            message
        );
    }

    #[test]
    fn eager_environment_creation_failures_remain_errors() {
        let mut backend = FakeBackend {
            initialize_error: Some("create failed".into()),
            ..Default::default()
        };
        let options = EnvironmentInitOptions {
            verbatim_loader_errors: true,
            ..Default::default()
        };
        assert_eq!(
            init_inner_with(&selection(), &options, &mut backend).unwrap_err(),
            "environment creation: create failed"
        );
    }
}

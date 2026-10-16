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
//!
//! Without a [`RuntimeSelection`] (system-loader fallback), call
//! [`init_environment_default_loader`]. It loads the runtime named by an absolute
//! `ORT_DYLIB_PATH`, or the platform file name through the system loader, and applies
//! the same options (telemetry off by default).
//!
//! Ordering rules (the first initialiser wins; a loaded runtime and committed
//! options are never replaced):
//!
//! * [`init_environment_default_loader`] after any earlier initialisation returns
//!   that outcome unchanged (no second initialisation), except that a request to
//!   disable telemetry fails when the earlier initialisation left it enabled or
//!   unspecified.
//! * [`init_environment_with_options`] after a default-loader initialisation returns
//!   that outcome only when the earlier call loaded the same runtime file; otherwise
//!   it fails, because the runtime cannot be replaced.
//! * Building a session through [`crate::SessionOptions`] with nothing initialised
//!   commits the default options (telemetry off, private pools) first, without loading
//!   anything, so the first session never runs with ORT's default telemetry. An
//!   [`init_environment_with_options`] call after that fails: call it before building a
//!   session. Sessions built straight through the re-exported `ort` crate bypass this.

use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        OnceLock,
    },
};

use crate::RuntimeSelection;

/// How the process environment was first initialised by this crate.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Origin {
    /// An explicit [`RuntimeSelection`] was loaded.
    Selected(PathBuf),
    /// [`init_environment_default_loader`] loaded this path (absolute `ORT_DYLIB_PATH`, or the
    /// bare platform file name for the system loader).
    DefaultLoader(PathBuf),
    /// A session build committed the default options; nothing was loaded.
    Implicit,
}

/// What this crate committed (absent when the first outcome was an error or an environment
/// configured outside this crate).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Applied {
    origin: Origin,
    telemetry: Option<bool>,
}

struct State {
    report: OnceLock<Result<EnvironmentInitReport, String>>,
    applied: OnceLock<Applied>,
}

impl State {
    const fn new() -> Self {
        Self {
            report: OnceLock::new(),
            applied: OnceLock::new(),
        }
    }
}

static STATE: State = State::new();
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
    matches!(STATE.report.get(), Some(Ok(r)) if r.shared_pool_active
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
///
/// Fails when [`init_environment_default_loader`] (or a session build) initialised the
/// environment first with a different runtime; see the module's ordering rules.
pub fn init_environment_with_options(
    selection: &RuntimeSelection,
    options: &EnvironmentInitOptions,
) -> Result<EnvironmentInitReport, String> {
    init_selected(&STATE, selection, options, &mut OrtBackend)
}

/// Initialise the environment without a [`RuntimeSelection`], for the system-loader
/// fallback. The runtime is the file named by a non-empty, absolute `ORT_DYLIB_PATH`
/// (a relative value, or one under a Windows `System32`/`SysWOW64`, is refused), else the
/// platform file name left to the system loader. Windows refuses the bare-name fallback:
/// it would pick the OS Windows-ML copy, which hangs at session init.
///
/// Options default to telemetry off, private pools, eager creation
/// ([`EnvironmentInitOptions::default`]). The load is fallible and goes through ort's
/// library check (exports `OrtGetApiBase`, minor version at least the pinned ort's), so a
/// missing or wrong library is an `Err`, never a panic.
/// `runtime_path` in the report is the path handed to the loader; a bare name means the
/// system loader chose the file.
///
/// Idempotent and consistent with [`init_environment_with_options`]: the first
/// initialisation wins, a repeat returns its outcome, and a request to disable telemetry
/// fails if the first initialisation left it enabled or unspecified. Call before any
/// session is built and before worker threads start.
pub fn init_environment_default_loader(
    options: &EnvironmentInitOptions,
) -> Result<EnvironmentInitReport, String> {
    init_default_loader(&STATE, options, &mut OrtBackend, default_loader_path)
}

/// Commit the default options (telemetry off) if nothing has initialised the environment,
/// without loading a runtime; the first session loads it lazily exactly as before. Used by
/// every session build in this crate. An earlier failed initialisation is returned as an
/// error rather than letting the session create an environment with ORT defaults.
pub(crate) fn ensure_environment() -> Result<(), String> {
    ensure_with(&STATE, &mut OrtBackend, || {
        default_loader_path().unwrap_or_else(|_| PathBuf::from(crate::runtime_filename()))
    })
}

fn default_loader_path() -> Result<PathBuf, String> {
    loader_path_from(
        std::env::var_os("ORT_DYLIB_PATH")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from),
        cfg!(target_os = "windows"),
    )
}

fn loader_path_from(env_path: Option<PathBuf>, windows: bool) -> Result<PathBuf, String> {
    if let Some(path) = env_path {
        if !path.is_absolute() {
            return Err(format!(
                "ORT_DYLIB_PATH must be absolute: {}",
                path.display()
            ));
        }
        if crate::is_system_runtime(&path) {
            return Err(format!(
                "ORT_DYLIB_PATH {} is the OS-provided runtime; bundle and supply an app runtime",
                path.display()
            ));
        }
        return Ok(path);
    }
    if windows {
        return Err(
            "no runtime selected and ORT_DYLIB_PATH is unset: the Windows system loader would pick \
             the OS onnxruntime.dll, which hangs at session init; supply an app runtime"
                .into(),
        );
    }
    Ok(PathBuf::from(crate::runtime_filename()))
}

fn init_selected(
    state: &State,
    selection: &RuntimeSelection,
    options: &EnvironmentInitOptions,
    backend: &mut impl EnvironmentBackend,
) -> Result<EnvironmentInitReport, String> {
    if let Some(applied) = state.applied.get() {
        match &applied.origin {
            Origin::Selected(_) => {}
            Origin::DefaultLoader(loaded) => {
                if !same_file(loaded, &selection.path) {
                    return Err(format!(
                        "ORT environment was already initialised through the default loader ({}); \
                         the runtime cannot be replaced by {}",
                        loaded.display(),
                        selection.path.display()
                    ));
                }
            }
            Origin::Implicit => {
                return Err(format!(
                    "ORT environment was already initialised by a session build before \
                     init_environment; the runtime {} was not applied. Initialise before building sessions",
                    selection.path.display()
                ));
            }
        }
    }
    init_once_with(&state.report, options, || {
        let report = init_inner_with(
            Some(&selection.path),
            selection.path.clone(),
            options,
            backend,
        )?;
        record(
            state,
            Origin::Selected(selection.path.clone()),
            options.telemetry,
            &report,
        );
        Ok(report)
    })
}

fn init_default_loader(
    state: &State,
    options: &EnvironmentInitOptions,
    backend: &mut impl EnvironmentBackend,
    path: impl FnOnce() -> Result<PathBuf, String>,
) -> Result<EnvironmentInitReport, String> {
    if state.report.get().is_some() {
        if let (Some(Ok(_)), Some(applied)) = (state.report.get(), state.applied.get()) {
            if options.telemetry == Some(false) && applied.telemetry != Some(false) {
                return Err(format!(
                    "ORT environment was already initialised with telemetry {}; \
                     it cannot be disabled afterwards",
                    match applied.telemetry {
                        Some(true) => "enabled",
                        _ => "unspecified (ORT default: enabled)",
                    }
                ));
            }
        }
        return init_once_with(&state.report, options, || {
            unreachable!("report already set")
        });
    }
    init_once_with(&state.report, options, || {
        let path = path()?;
        let report = init_inner_with(Some(&path), path.clone(), options, backend)?;
        record(
            state,
            Origin::DefaultLoader(path),
            options.telemetry,
            &report,
        );
        Ok(report)
    })
}

fn ensure_with(
    state: &State,
    backend: &mut impl EnvironmentBackend,
    path: impl FnOnce() -> PathBuf,
) -> Result<(), String> {
    if let Some(result) = state.report.get() {
        return result.as_ref().map(|_| ()).map_err(Clone::clone);
    }
    let options = EnvironmentInitOptions {
        defer_initialization: true,
        tolerate_already_initialized: true,
        ..EnvironmentInitOptions::default()
    };
    // Failures here are not cached: nothing was loaded, so a later explicit call can still run.
    let report = init_inner_with(None, path(), &options, backend)?;
    record(state, Origin::Implicit, options.telemetry, &report);
    let _ = state.report.set(Ok(report));
    Ok(())
}

/// Remember what this crate committed (not an environment configured elsewhere).
fn record(state: &State, origin: Origin, telemetry: Option<bool>, report: &EnvironmentInitReport) {
    if report.status != EnvironmentStatus::AlreadyInitialized {
        let _ = state.applied.set(Applied { origin, telemetry });
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
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

    /// `Some(path)` loads that runtime (fallible); `None` only creates the builder, so nothing
    /// is loaded until ORT is first used.
    fn load(&mut self, path: Option<&Path>) -> Result<Self::Builder, String>;
    fn configure(
        &mut self,
        builder: Self::Builder,
        options: &EnvironmentInitOptions,
    ) -> Result<bool, String>;
    fn initialize(&mut self) -> Result<String, String>;
}

fn init_inner_with(
    load: Option<&Path>,
    runtime_path: PathBuf,
    options: &EnvironmentInitOptions,
    backend: &mut impl EnvironmentBackend,
) -> Result<EnvironmentInitReport, String> {
    let builder = backend.load(load).map_err(|error| {
        if options.verbatim_loader_errors {
            error
        } else {
            format!("load {}: {error}", runtime_path.display())
        }
    })?;
    let committed = backend.configure(builder, options)?;
    if !committed && !options.tolerate_already_initialized {
        return Err("ORT was configured before rightkit-onnx::init_environment".into());
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
        runtime_path,
        runtime_info,
        shared_pool_requested: shared_requested,
        shared_pool_active: committed && shared_requested && !options.defer_initialization,
        status,
    })
}

struct OrtBackend;

impl EnvironmentBackend for OrtBackend {
    type Builder = ort::environment::EnvironmentBuilder;

    fn load(&mut self, path: Option<&Path>) -> Result<Self::Builder, String> {
        match path {
            Some(path) => ort::init_from(path).map_err(|error| error.to_string()),
            None => Ok(ort::init()),
        }
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
        loads: Vec<Option<PathBuf>>,
    }

    impl Default for FakeBackend {
        fn default() -> Self {
            Self {
                committed: true,
                load_error: None,
                initialize_error: None,
                configured: None,
                initializations: 0,
                loads: Vec::new(),
            }
        }
    }

    impl EnvironmentBackend for FakeBackend {
        type Builder = ();

        fn load(&mut self, path: Option<&Path>) -> Result<(), String> {
            self.loads.push(path.map(Path::to_path_buf));
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

    fn run(
        selection: &RuntimeSelection,
        options: &EnvironmentInitOptions,
        backend: &mut impl EnvironmentBackend,
    ) -> Result<EnvironmentInitReport, String> {
        init_inner_with(
            Some(&selection.path),
            selection.path.clone(),
            options,
            backend,
        )
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
        let report = run(&selection(), &options, &mut backend).unwrap();
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
        let report = run(&selection(), &options, &mut backend).unwrap();
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
            run(&selection(), &options, &mut backend)
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
            run(
                &selection(),
                &EnvironmentInitOptions::default(),
                &mut backend
            )
            .unwrap_err(),
            "ORT was configured before rightkit-onnx::init_environment"
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
        let report = run(&selection(), &options, &mut backend).unwrap();
        assert_eq!(report.status, EnvironmentStatus::AlreadyInitialized);
        assert!(report.shared_pool_requested);
        assert!(!report.shared_pool_active); // Never attribute ignored pool options to ORT.
        assert_eq!(backend.initializations, 1);
        let deferred = EnvironmentInitOptions {
            defer_initialization: true,
            ..options
        };
        let _ = run(&selection(), &deferred, &mut backend).unwrap();
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
            run(&selected, &EnvironmentInitOptions::default(), &mut backend).unwrap_err();
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
        let error = init_once_with(&cache, &options, || run(&selected, &options, &mut backend))
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
            run(&selection(), &options, &mut backend).unwrap_err(),
            "environment creation: create failed"
        );
    }

    fn state() -> State {
        State::new()
    }

    fn bare() -> PathBuf {
        PathBuf::from(crate::runtime_filename())
    }

    #[test]
    fn default_loader_needs_no_selection_and_defaults_to_telemetry_off() {
        let state = state();
        let mut backend = FakeBackend::default();
        let path = std::env::temp_dir().join("libonnxruntime.so.1.24.0");
        let report = init_default_loader(
            &state,
            &EnvironmentInitOptions::default(),
            &mut backend,
            || Ok(path.clone()),
        )
        .unwrap();
        assert_eq!(report.status, EnvironmentStatus::Initialized);
        assert_eq!(report.runtime_path, path);
        assert_eq!(backend.loads, vec![Some(path)]);
        assert_eq!(backend.initializations, 1);
        assert_eq!(backend.configured.unwrap().telemetry, Some(false));
    }

    #[test]
    fn default_loader_failures_are_errors_and_commit_nothing() {
        let state = state();
        let mut backend = FakeBackend::default();
        let error = init_default_loader(
            &state,
            &EnvironmentInitOptions::default(),
            &mut backend,
            || Err("ORT_DYLIB_PATH must be absolute: x".into()),
        )
        .unwrap_err();
        assert!(error.contains("absolute"));
        assert!(backend.loads.is_empty() && backend.configured.is_none());
        let mut failing = FakeBackend {
            load_error: Some("no such library".into()),
            ..Default::default()
        };
        let error = init_default_loader(
            &State::new(),
            &EnvironmentInitOptions::default(),
            &mut failing,
            || Ok(bare()),
        )
        .unwrap_err();
        assert!(error.starts_with("load "), "{error}");
        assert!(failing.configured.is_none());
    }

    #[test]
    fn default_loader_is_idempotent_and_never_initialises_twice() {
        let state = state();
        let mut backend = FakeBackend::default();
        let first = init_default_loader(
            &state,
            &EnvironmentInitOptions::default(),
            &mut backend,
            || Ok(bare()),
        )
        .unwrap();
        let second = init_default_loader(
            &state,
            &EnvironmentInitOptions {
                tolerate_already_initialized: true,
                ..Default::default()
            },
            &mut backend,
            || panic!("must not resolve a path again"),
        )
        .unwrap();
        assert_eq!(second.status, EnvironmentStatus::AlreadyInitialized);
        assert_eq!(second.runtime_path, first.runtime_path);
        assert_eq!(backend.loads.len(), 1);
        assert_eq!(backend.initializations, 1);
    }

    #[test]
    fn disabling_telemetry_fails_after_an_initialisation_that_left_it_on() {
        for (first, expect_ok) in [(Some(false), true), (None, false), (Some(true), false)] {
            let state = state();
            let mut backend = FakeBackend::default();
            init_default_loader(
                &state,
                &EnvironmentInitOptions {
                    telemetry: first,
                    ..Default::default()
                },
                &mut backend,
                || Ok(bare()),
            )
            .unwrap();
            let again = init_default_loader(
                &state,
                &EnvironmentInitOptions::default(),
                &mut backend,
                || panic!("no second resolve"),
            );
            assert_eq!(again.is_ok(), expect_ok, "first telemetry {first:?}");
            assert_eq!(backend.initializations, 1);
        }
    }

    #[test]
    fn selection_after_default_loader_must_name_the_loaded_file() {
        let dir = std::env::temp_dir().join(format!("rightkit-ort-env-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let loaded = dir.join("libonnxruntime.so.1.24.0");
        let other = dir.join("libother.so");
        std::fs::write(&loaded, b"x").unwrap();
        std::fs::write(&other, b"x").unwrap();
        let state = state();
        let mut backend = FakeBackend::default();
        init_default_loader(
            &state,
            &EnvironmentInitOptions::default(),
            &mut backend,
            || Ok(loaded.clone()),
        )
        .unwrap();
        let pick = |path: &Path| RuntimeSelection {
            path: path.to_path_buf(),
            source: CandidateSource::ExplicitFile,
            diagnostics: Vec::new(),
        };
        let error = init_selected(
            &state,
            &pick(&other),
            &EnvironmentInitOptions::default(),
            &mut backend,
        )
        .unwrap_err();
        assert!(error.contains("cannot be replaced"), "{error}");
        let same = init_selected(
            &state,
            &pick(&loaded),
            &EnvironmentInitOptions::default(),
            &mut backend,
        )
        .unwrap();
        assert_eq!(same.runtime_path, loaded);
        assert_eq!((backend.loads.len(), backend.initializations), (1, 1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn default_loader_after_a_selection_returns_the_selection_outcome() {
        let state = state();
        let mut backend = FakeBackend::default();
        let first = init_selected(
            &state,
            &selection(),
            &EnvironmentInitOptions::default(),
            &mut backend,
        )
        .unwrap();
        let again = init_default_loader(
            &state,
            &EnvironmentInitOptions::default(),
            &mut backend,
            || panic!("no second resolve"),
        )
        .unwrap();
        assert_eq!(again, first);
        assert_eq!(backend.loads.len(), 1);
    }

    #[test]
    fn session_builds_commit_telemetry_off_without_loading_and_do_not_repeat() {
        let state = state();
        let mut backend = FakeBackend::default();
        ensure_with(&state, &mut backend, bare).unwrap();
        ensure_with(&state, &mut backend, || panic!("second ensure is a no-op")).unwrap();
        let configured = backend.configured.clone().unwrap();
        assert_eq!(configured.telemetry, Some(false));
        assert!(configured.defer_initialization);
        assert_eq!(backend.loads, vec![None]);
        assert_eq!(backend.initializations, 0);
        let report = state.report.get().unwrap().as_ref().unwrap();
        assert_eq!(report.status, EnvironmentStatus::Deferred);
        // An explicit selection after a session build is refused, not silently ignored.
        let error = init_selected(
            &state,
            &selection(),
            &EnvironmentInitOptions::default(),
            &mut backend,
        )
        .unwrap_err();
        assert!(error.contains("before building sessions") || error.contains("Initialise before"));
        assert_eq!(backend.loads.len(), 1);
    }

    #[test]
    fn session_builds_after_an_explicit_init_change_nothing_and_after_a_failure_report_it() {
        let state = state();
        let mut backend = FakeBackend::default();
        init_selected(
            &state,
            &selection(),
            &EnvironmentInitOptions::default(),
            &mut backend,
        )
        .unwrap();
        ensure_with(&state, &mut backend, || panic!("already initialised")).unwrap();
        assert_eq!(backend.loads.len(), 1);

        let failed = State::new();
        let mut broken = FakeBackend {
            load_error: Some("boom".into()),
            ..Default::default()
        };
        let _ = init_selected(
            &failed,
            &selection(),
            &EnvironmentInitOptions::default(),
            &mut broken,
        )
        .unwrap_err();
        // A session must not create an environment with ORT defaults after a failed init.
        assert!(ensure_with(&failed, &mut broken, bare).is_err());
    }

    #[test]
    fn implicit_commit_failure_is_not_cached() {
        let state = state();
        let mut backend = FakeBackend {
            committed: false,
            ..Default::default()
        };
        // Environment configured outside this crate: tolerated, nothing recorded as ours.
        ensure_with(&state, &mut backend, bare).unwrap();
        assert!(state.applied.get().is_none());
        let mut failing = FakeBackend {
            load_error: Some("x".into()),
            ..Default::default()
        };
        let fresh = State::new();
        // load(None) never fails on the real backend; a failing seam must not poison the cache.
        assert!(ensure_with(&fresh, &mut failing, bare).is_err());
        assert!(fresh.report.get().is_none());
    }

    #[test]
    fn default_loader_path_rules() {
        let abs = std::env::temp_dir().join("libonnxruntime.so.1.24.0");
        assert_eq!(loader_path_from(Some(abs.clone()), false).unwrap(), abs);
        assert_eq!(loader_path_from(Some(abs.clone()), true).unwrap(), abs);
        assert!(loader_path_from(Some(PathBuf::from("rel/lib.so")), false)
            .unwrap_err()
            .contains("absolute"));
        assert_eq!(loader_path_from(None, false).unwrap(), bare());
        assert!(loader_path_from(None, true)
            .unwrap_err()
            .contains("hangs at session init"));
        let sys = std::env::temp_dir()
            .join("Windows")
            .join("System32")
            .join("onnxruntime.dll");
        assert!(loader_path_from(Some(sys), false)
            .unwrap_err()
            .contains("OS-provided"));
        let wow = std::env::temp_dir()
            .join("SysWOW64")
            .join("onnxruntime.dll");
        assert!(loader_path_from(Some(wow), true)
            .unwrap_err()
            .contains("OS-provided"));
    }
}

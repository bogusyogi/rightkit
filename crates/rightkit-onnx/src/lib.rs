//! Product-neutral ONNX Runtime dynamic-library resolution, environment and
//! session setup for Right Suite apps.
//!
//! Merged from ScrapeRight `ort_common` (installed-bundle layout, system-DLL
//! hazard) and HeardRight `heardright-onnx-asr` (environment, execution
//! providers, session builder). Product environment variables and data roots
//! stay in app adapters.

/// The pinned `ort` crate, re-exported so apps that run their own inference reach
/// `rightkit_onnx::ort::{value::Tensor, session::Session, ...}` through this crate.
/// They never declare a direct `ort` dependency, which keeps the release ownership
/// scanner from seeing one and keeps the whole suite on a single `ort` pin.
///
/// ```no_run
/// use rightkit_onnx::ort::{session::Session, value::Tensor};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// // Configure the runtime and environment first (see `configure_runtime`).
/// let mut session = Session::builder(rightkit_onnx::ort_environment()?)?
///     .commit_from_file("model.onnx")?;
/// let x = Tensor::from_array(([1usize, 4], vec![0.0f32; 4]))?;
/// let _outputs = session.run(rightkit_onnx::ort::inputs!["x" => x])?;
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "session")]
pub use ort;

/// `half` (f16/bf16) as used by ort's `half` feature: `rightkit_onnx::half::f16`.
#[cfg(feature = "half")]
pub use half;

/// `ndarray` as used by ort's `ndarray` feature: `rightkit_onnx::ndarray::Array2`.
#[cfg(feature = "ndarray")]
pub use ndarray;

#[cfg(feature = "session")]
pub mod environment;
#[cfg(feature = "session")]
pub mod probe;
/// Build-time download, verification and extraction of the catalog runtime (`provision` feature).
#[cfg(feature = "provision")]
pub mod provision;
pub mod runtime_bundle;
#[cfg(feature = "session")]
pub mod session;

#[cfg(feature = "session")]
pub use environment::{
    cpu_thread_budget, init_environment, init_environment_default_loader,
    init_environment_with_options, ort_environment, shared_pool_active, EnvironmentInitOptions,
    EnvironmentInitReport, EnvironmentOptions, EnvironmentReport, EnvironmentStatus, GlobalPool,
};
#[cfg(feature = "session")]
pub use probe::{probe_providers, ProviderDiagnostic, ProviderKind};
pub use runtime_bundle::{
    verify_library_in_manifest, verify_runtime_manifest, OnnxRuntimeIdentity, VerifiedFile,
    VerifiedRuntime,
};
#[cfg(feature = "session")]
pub use session::{BuiltSession, ExecutionProvider, SessionError, SessionOptions};

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

static CONFIGURED_RUNTIME: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Clear inherited `ORT_DYLIB_PATH` unless the `dev-overrides` feature is on (then a no-op).
/// This keeps shell-exported development paths from steering shipped and installed dev apps,
/// consistent with the policy that inherited `ORT_DYLIB_PATH` fails closed.
///
/// Call at process startup, before any thread spawns, runtime binding or ORT
/// use. Removing a process environment variable requires exclusive access to
/// the environment; this function does not unload an already-loaded runtime.
pub fn clear_inherited_runtime_for_release() {
    clear_inherited_runtime_with(|| std::env::remove_var("ORT_DYLIB_PATH"));
}

// Injected removal keeps release/debug tests free of process env mutation.
fn clear_inherited_runtime_with(remove_path: impl FnOnce()) {
    if cfg!(not(feature = "dev-overrides")) {
        remove_path();
    }
}

/// Read a caller-named development override as an explicit runtime candidate.
/// Unset or empty values return `None`; builds without the `dev-overrides` feature
/// always return `None` without reading the variable. Path validation remains in runtime resolution.
pub fn developer_override_candidate(env_name: &str) -> Option<RuntimeCandidate> {
    if cfg!(feature = "dev-overrides") {
        nonempty_env_path(env_name)
            .map(|path| RuntimeCandidate::new(path, CandidateSource::Explicit))
    } else {
        None
    }
}

/// Candidate for an override that names the runtime file itself, with any file name
/// (`/opt/ort/libonnxruntime.so.1.24.0`). Resolution still requires an absolute path, rejects the
/// Windows `System32`/`SysWOW64` copy (also after symlink resolution), and requires the target to
/// be a regular file (a symlink to one is followed; a directory is `NotFound`). Only the
/// file-name rule is relaxed, and only for this source: candidates found by discovery, the installed
/// bundle layout or `legacy_runtime_candidate` keep the strict platform basename.
///
/// Relative paths stay refused: a relative library path is resolved against the working
/// directory or the loader's search path, which lets a planted library win.
///
/// Resolving and binding do not load the file. The load happens in [`init_environment`] /
/// [`init_environment_with_options`], where ort checks that the file exports `OrtGetApiBase` and
/// reports an ONNX Runtime minor version at least the pinned ort's; anything else is an `Err`.
pub fn explicit_file_candidate(path: impl Into<PathBuf>) -> RuntimeCandidate {
    RuntimeCandidate::new(path, CandidateSource::ExplicitFile)
}

/// Return the last path successfully bound by this crate for this process,
/// falling back to a non-empty `ORT_DYLIB_PATH` when no binding is recorded.
/// Read-only: no resolution, file check, environment mutation or ORT loading.
/// Missing-mode bindings are included; this is not proof of a loaded library.
pub fn configured_runtime() -> Option<PathBuf> {
    let recorded = CONFIGURED_RUNTIME
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    recorded.or_else(|| nonempty_env_path("ORT_DYLIB_PATH"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateSource {
    /// Caller-named path whose file name must be the exact platform name
    /// ([`runtime_filename`]).
    Explicit,
    /// Caller-named absolute path to a regular file with any name (a versioned soname such as
    /// `libonnxruntime.so.1.24.0`, or a renamed copy). See [`explicit_file_candidate`].
    ExplicitFile,
    Bundled,
    AppData,
    /// Caller-opted-in `<workspace>/tools/bin/<runtime filename>`.
    LegacyDefault,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeCandidate {
    pub path: PathBuf,
    pub source: CandidateSource,
}

impl RuntimeCandidate {
    pub fn new(path: impl Into<PathBuf>, source: CandidateSource) -> Self {
        Self {
            path: path.into(),
            source,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateDiagnostic {
    pub path: PathBuf,
    pub source: CandidateSource,
    pub absolute: bool,
    pub expected_filename: bool,
    pub exists: bool,
    pub is_file: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSelection {
    pub path: PathBuf,
    pub source: CandidateSource,
    pub diagnostics: Vec<CandidateDiagnostic>,
}

/// Binding policy; defaults retain strict file validation & conflict refusal.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RuntimeBindingOptions {
    /// Overwrite ORT_DYLIB_PATH, including an invalid existing value. This does
    /// not replace a library already loaded by ort. Use before any ORT use.
    pub force_replace: bool,
    /// If no regular file resolves, bind the first missing candidate. Existing
    /// directories, invalid filenames & unsafe paths remain errors.
    pub allow_missing: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeBindingStatus {
    Ready,
    /// Path recorded & bound, but absent. Callers own degraded-mode messaging.
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeBinding {
    pub selection: RuntimeSelection,
    pub status: RuntimeBindingStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeError {
    NoCandidates,
    UnsafePath {
        path: PathBuf,
    },
    UnexpectedFilename {
        path: PathBuf,
        expected: &'static str,
    },
    NotFound {
        diagnostics: Vec<CandidateDiagnostic>,
    },
    Canonicalize {
        path: PathBuf,
        message: String,
    },
    AlreadyConfigured {
        configured: PathBuf,
        selected: PathBuf,
    },
    /// The Windows-ML copy in `System32` hangs at session init; apps must
    /// supply their bundled runtime instead.
    SystemRuntimeRejected {
        path: PathBuf,
    },
    /// `manifest.json` is missing, unreadable, malformed, or fails the schema.
    BundleManifest {
        path: PathBuf,
        message: String,
    },
    /// A manifest path is empty, absolute, non-normalised, contains `..`, `\`
    /// or a NUL byte, names `manifest.json`, or resolves outside the bundle.
    BundlePath {
        path: String,
        reason: &'static str,
    },
    /// A file listed in the manifest is absent.
    BundleFileMissing {
        path: PathBuf,
    },
    /// A listed file's byte count differs from the manifest.
    BundleFileSize {
        path: PathBuf,
        expected: u64,
        actual: u64,
    },
    /// A listed file's SHA-256 differs from the manifest.
    BundleFileHash {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    /// A regular file under the bundle directory that the manifest does not list.
    BundleFileUnlisted {
        path: PathBuf,
    },
    /// A symbolic link or special file under the bundle directory.
    BundleEntryUnsupported {
        path: PathBuf,
    },
    /// A file or directory in the bundle could not be read.
    BundleFileRead {
        path: PathBuf,
        message: String,
    },
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoCandidates => write!(f, "no ONNX Runtime candidates were supplied"),
            Self::UnsafePath { path } => {
                write!(f, "runtime path must be absolute: {}", path.display())
            }
            Self::UnexpectedFilename { path, expected } => {
                write!(f, "runtime path {} must end in {expected}", path.display())
            }
            Self::NotFound { .. } => {
                write!(f, "no supplied ONNX Runtime candidate is a regular file")
            }
            Self::Canonicalize { path, message } => {
                write!(f, "cannot canonicalize {}: {message}", path.display())
            }
            Self::SystemRuntimeRejected { path } => write!(
                f,
                "runtime {} is the OS-provided copy; bundle and supply an app runtime",
                path.display()
            ),
            Self::AlreadyConfigured {
                configured,
                selected,
            } => write!(
                f,
                "ORT_DYLIB_PATH is already {}, refusing replacement with {}",
                configured.display(),
                selected.display()
            ),
            Self::BundleManifest { path, message } => {
                write!(
                    f,
                    "runtime manifest {} is invalid: {message}",
                    path.display()
                )
            }
            Self::BundlePath { path, reason } => {
                write!(f, "runtime manifest path {path:?} rejected: {reason}")
            }
            Self::BundleFileMissing { path } => {
                write!(f, "runtime bundle file is missing: {}", path.display())
            }
            Self::BundleFileSize {
                path,
                expected,
                actual,
            } => write!(
                f,
                "runtime bundle file {} is {actual} bytes; manifest says {expected}",
                path.display()
            ),
            Self::BundleFileHash {
                path,
                expected,
                actual,
            } => write!(
                f,
                "runtime bundle file {} has sha256 {actual}; manifest says {expected}",
                path.display()
            ),
            Self::BundleFileUnlisted { path } => write!(
                f,
                "runtime bundle file {} is not listed in the manifest",
                path.display()
            ),
            Self::BundleEntryUnsupported { path } => write!(
                f,
                "runtime bundle entry {} is a symbolic link or special file",
                path.display()
            ),
            Self::BundleFileRead { path, message } => {
                write!(
                    f,
                    "cannot read runtime bundle {}: {message}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for RuntimeError {}

pub fn runtime_filename() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "onnxruntime.dll"
    }
    #[cfg(target_os = "macos")]
    {
        "libonnxruntime.dylib"
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        "libonnxruntime.so"
    }
}

pub fn inspect_candidate(candidate: &RuntimeCandidate) -> CandidateDiagnostic {
    CandidateDiagnostic {
        path: candidate.path.clone(),
        source: candidate.source,
        absolute: candidate.path.is_absolute(),
        expected_filename: candidate.path.file_name().and_then(|name| name.to_str())
            == Some(runtime_filename()),
        exists: candidate.path.exists(),
        is_file: candidate.path.is_file(),
    }
}

pub fn resolve_runtime(
    candidates: impl IntoIterator<Item = RuntimeCandidate>,
) -> Result<RuntimeSelection, RuntimeError> {
    let candidates: Vec<_> = candidates.into_iter().collect();
    if candidates.is_empty() {
        return Err(RuntimeError::NoCandidates);
    }
    let mut diagnostics = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let diagnostic = inspect_candidate(&candidate);
        if !diagnostic.absolute {
            return Err(RuntimeError::UnsafePath {
                path: candidate.path,
            });
        }
        if !diagnostic.expected_filename && candidate.source != CandidateSource::ExplicitFile {
            return Err(RuntimeError::UnexpectedFilename {
                path: candidate.path,
                expected: runtime_filename(),
            });
        }
        if is_system_runtime(&candidate.path) {
            return Err(RuntimeError::SystemRuntimeRejected {
                path: candidate.path,
            });
        }
        diagnostics.push(diagnostic.clone());
        if diagnostic.is_file {
            let path =
                candidate
                    .path
                    .canonicalize()
                    .map_err(|error| RuntimeError::Canonicalize {
                        path: candidate.path,
                        message: error.to_string(),
                    })?;
            if candidate.source == CandidateSource::ExplicitFile
                && (!path.is_file() || is_system_runtime(&path))
            {
                return Err(if is_system_runtime(&path) {
                    RuntimeError::SystemRuntimeRejected { path }
                } else {
                    RuntimeError::NotFound { diagnostics }
                });
            }
            return Ok(RuntimeSelection {
                path,
                source: candidate.source,
                diagnostics,
            });
        }
    }
    Err(RuntimeError::NotFound { diagnostics })
}

/// True when the path sits under a Windows `System32`/`SysWOW64` directory.
/// The OS copy of `onnxruntime.dll` (Windows ML) hangs at session init, so it
/// is never an acceptable candidate (ScrapeRight `ort_common` finding).
pub fn is_system_runtime(path: &Path) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .map(|name| {
                name.eq_ignore_ascii_case("system32") || name.eq_ignore_ascii_case("syswow64")
            })
            .unwrap_or(false)
    })
}

/// Resources directory of an installed application bundle, derived from its
/// executable only (no checkout or working-directory fallback):
/// macOS `Foo.app/Contents/MacOS/foo` -> `Contents/Resources`; Windows the
/// executable directory; elsewhere `<exe dir>/resources`.
pub fn installed_resource_dir(executable: &Path) -> Option<PathBuf> {
    if !executable.is_absolute() {
        return None;
    }
    let dir = executable.parent()?.to_path_buf();
    let is = |p: &Path, name: &str| {
        p.file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.eq_ignore_ascii_case(name))
            .unwrap_or(false)
    };
    #[cfg(target_os = "macos")]
    {
        if is(&dir, "MacOS") {
            if let Some(contents) = dir.parent() {
                if is(contents, "Contents") {
                    return Some(contents.join("Resources"));
                }
            }
        }
        if is(&dir, "Resources") {
            return Some(dir);
        }
        Some(dir.join("Resources"))
    }
    #[cfg(target_os = "windows")]
    {
        let _ = is;
        Some(dir)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        if is(&dir, "resources") {
            Some(dir)
        } else {
            Some(dir.join("resources"))
        }
    }
}

/// Bundled-runtime candidate for an installed app: `<resources>/<subdir>/<runtime file>`.
/// Apps pass their own `subdir` (ScrapeRight/HeardRight/CodeRight all use `runtime`).
pub fn installed_runtime_candidate(executable: &Path, subdir: &str) -> Option<RuntimeCandidate> {
    let dir = installed_resource_dir(executable)?;
    Some(RuntimeCandidate::new(
        dir.join(subdir).join(runtime_filename()),
        CandidateSource::Bundled,
    ))
}

/// Bundled-runtime candidate for Android: `<native_lib_dir>/libonnxruntime.so`.
///
/// `native_lib_dir` is `ApplicationInfo.nativeLibraryDir`, handed in from Kotlin (an absolute
/// path; a relative one is `UnsafePath`). Android has no executable-relative resource layout, so
/// this replaces [`installed_runtime_candidate`] there. Pure path logic: it compiles and runs on
/// any host, but nothing here has been run on a device.
///
/// The file exists in `nativeLibraryDir` only when the APK extracts its native libraries
/// (`android:extractNativeLibs="true"` / `useLegacyPackaging = true`). With the AGP default for a
/// recent `minSdk` the library stays inside the APK and this path is not a file; then resolution
/// reports `NotFound` and the app uses [`init_environment_default_loader`], whose bare
/// `libonnxruntime.so` lets the platform linker find it in the APK. Which of the two a given
/// packaging produces is unverified.
pub fn android_runtime_candidate(native_lib_dir: &Path) -> Result<RuntimeCandidate, RuntimeError> {
    if !native_lib_dir.is_absolute() {
        return Err(RuntimeError::UnsafePath {
            path: native_lib_dir.to_path_buf(),
        });
    }
    Ok(RuntimeCandidate::new(
        native_lib_dir.join("libonnxruntime.so"),
        CandidateSource::Bundled,
    ))
}

/// [`configure_runtime`] for an Android `nativeLibraryDir` (see [`android_runtime_candidate`]).
/// Call before any ORT use and before threads start, like [`configure_runtime`].
pub fn configure_android_runtime(native_lib_dir: &Path) -> Result<RuntimeSelection, RuntimeError> {
    configure_runtime([android_runtime_candidate(native_lib_dir)?])
}

/// Explicit legacy candidate; never searched unless the caller supplies it.
/// Relative roots are rejected rather than resolved against process cwd.
pub fn legacy_runtime_candidate(workspace_root: &Path) -> Result<RuntimeCandidate, RuntimeError> {
    if !workspace_root.is_absolute() {
        return Err(RuntimeError::UnsafePath {
            path: workspace_root.to_path_buf(),
        });
    }
    Ok(RuntimeCandidate::new(
        workspace_root
            .join("tools")
            .join("bin")
            .join(runtime_filename()),
        CandidateSource::LegacyDefault,
    ))
}

/// Configure ORT only after resolving a caller-supplied, absolute regular file.
/// Existing configuration is preserved and must canonicalize to the same file.
pub fn configure_runtime(
    candidates: impl IntoIterator<Item = RuntimeCandidate>,
) -> Result<RuntimeSelection, RuntimeError> {
    configure_runtime_with_options(candidates, &RuntimeBindingOptions::default())
        .map(|binding| binding.selection)
}

/// [`configure_runtime`] that fails closed on the library digest. The candidate is
/// resolved first (absolute regular file, caller order), then checked with
/// [`verify_library_in_manifest`] against the `manifest.json` beside it; only a
/// verified library is bound to `ORT_DYLIB_PATH`. A missing manifest, an unlisted
/// library or a size or digest mismatch returns the error and leaves the
/// environment untouched. Unrelated manifest entries and unlisted files do not
/// fail it. The file is verified before it is bound, not while ort loads it; a
/// local writer replacing it in that window is outside this check.
pub fn configure_runtime_verified(
    candidates: impl IntoIterator<Item = RuntimeCandidate>,
) -> Result<RuntimeSelection, RuntimeError> {
    let selection = resolve_runtime(candidates)?;
    verify_library_in_manifest(&selection.path)?;
    configure_runtime([RuntimeCandidate::new(
        selection.path.clone(),
        selection.source,
    )])?;
    Ok(selection)
}

/// Bind explicit candidates under caller-selected policy. Available regular
/// files win in caller order; missing mode falls back to the first absent path
/// only when no file resolves. Missing paths retain their supplied absolute
/// spelling because they cannot be canonicalized.
///
/// Call before starting worker threads or using any ORT API: this mutates a
/// process environment variable, not ort's already-loaded library handle.
pub fn configure_runtime_with_options(
    candidates: impl IntoIterator<Item = RuntimeCandidate>,
    options: &RuntimeBindingOptions,
) -> Result<RuntimeBinding, RuntimeError> {
    let binding = configure_runtime_with_env(
        candidates,
        options,
        nonempty_env_path("ORT_DYLIB_PATH"),
        |path| std::env::set_var("ORT_DYLIB_PATH", path),
    )?;
    *CONFIGURED_RUNTIME
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(binding.selection.path.clone());
    Ok(binding)
}

// Injected environment writer avoids process-global mutation in unit tests.
fn configure_runtime_with_env(
    candidates: impl IntoIterator<Item = RuntimeCandidate>,
    options: &RuntimeBindingOptions,
    configured: Option<PathBuf>,
    set_path: impl FnOnce(&Path),
) -> Result<RuntimeBinding, RuntimeError> {
    let binding = match resolve_runtime(candidates) {
        Ok(selection) => RuntimeBinding {
            selection,
            status: RuntimeBindingStatus::Ready,
        },
        Err(RuntimeError::NotFound { diagnostics }) if options.allow_missing => {
            let Some(missing) = diagnostics.iter().find(|candidate| !candidate.exists) else {
                return Err(RuntimeError::NotFound { diagnostics });
            };
            RuntimeBinding {
                selection: RuntimeSelection {
                    path: missing.path.clone(),
                    source: missing.source,
                    diagnostics,
                },
                status: RuntimeBindingStatus::Missing,
            }
        }
        Err(error) => return Err(error),
    };
    if let Some(configured) = configured.filter(|_| !options.force_replace) {
        if options.allow_missing && configured == binding.selection.path {
            return Ok(binding);
        }
        let configured = canonical_if_file(&configured)?;
        if configured != binding.selection.path {
            return Err(RuntimeError::AlreadyConfigured {
                configured,
                selected: binding.selection.path,
            });
        }
        return Ok(binding);
    }
    set_path(&binding.selection.path);
    Ok(binding)
}

/// Ordered runtime search for processes that carry no app-supplied runtime
/// path (worker processes, CLIs): a non-empty `ORT_DYLIB_PATH`, then
/// `<exe dir>/runtime`, `<exe dir>`, then each caller directory in order.
/// Caller directories are joined with `runtime_filename()`; relative
/// directories are skipped. Candidates are not checked for existence.
pub fn discovery_candidates(
    extra_dirs: impl IntoIterator<Item = PathBuf>,
) -> Vec<RuntimeCandidate> {
    let mut out = Vec::new();
    if let Some(path) = nonempty_env_path("ORT_DYLIB_PATH") {
        out.push(RuntimeCandidate::new(path, CandidateSource::Explicit));
    }
    let name = runtime_filename();
    // On Android `current_exe` is the zygote's `app_process`, not the app: its directory says
    // nothing about where the app's libraries are. Android callers pass `nativeLibraryDir` in
    // `extra_dirs` (or use `android_runtime_candidate`).
    #[cfg(not(target_os = "android"))]
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    #[cfg(target_os = "android")]
    let exe_dir: Option<PathBuf> = None;
    if let Some(exe_dir) = exe_dir {
        out.push(RuntimeCandidate::new(
            exe_dir.join("runtime").join(name),
            CandidateSource::Bundled,
        ));
        out.push(RuntimeCandidate::new(
            exe_dir.join(name),
            CandidateSource::Bundled,
        ));
    }
    out.extend(
        extra_dirs
            .into_iter()
            .filter(|dir| dir.is_absolute())
            .map(|dir| RuntimeCandidate::new(dir.join(name), CandidateSource::AppData)),
    );
    out
}

/// First regular-file candidate from `discovery_candidates`, skipping the
/// Windows-ML `System32` copy. A stale `ORT_DYLIB_PATH` does not mask a runtime
/// installed in a later location. No environment mutation, no ORT loading.
pub fn discover_runtime(extra_dirs: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    discovery_candidates(extra_dirs)
        .into_iter()
        .map(|candidate| candidate.path)
        .find(|path| path.is_file() && !is_system_runtime(path))
}

/// Make the directory of `runtime` a dependency search directory for this process by putting it
/// first on `PATH`, so DLLs that sit beside the runtime load (Windows resolves a library's own
/// dependencies from `PATH`, not from the library's folder). Idempotent: an entry already present
/// is moved to the front, never duplicated. An unset `PATH` becomes just that directory, and a
/// path with no parent directory is ignored. Call single-threaded, before ORT use.
pub fn prepend_runtime_dir_to_path(runtime: &Path) {
    let Some(dir) = runtime.parent().filter(|dir| !dir.as_os_str().is_empty()) else {
        return;
    };
    let mut paths = vec![dir.to_path_buf()];
    if let Some(existing) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&existing).filter(|entry| entry != dir));
    }
    if let Ok(joined) = std::env::join_paths(paths) {
        std::env::set_var("PATH", joined);
    }
}

/// Export a discovered runtime to this process: set `ORT_DYLIB_PATH` unless
/// already set and put its directory first on `PATH` (see
/// [`prepend_runtime_dir_to_path`]) so dependent libraries load. The directory is added when
/// `ORT_DYLIB_PATH` was already set too: an explicit runtime keeps its sibling-DLL lookup.
/// Returns the runtime path now in effect. Call single-threaded, before ORT use.
pub fn export_discovered_runtime(extra_dirs: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    if let Some(existing) = nonempty_env_path("ORT_DYLIB_PATH") {
        prepend_runtime_dir_to_path(&existing);
        return Some(existing);
    }
    let path = discover_runtime(extra_dirs)?;
    std::env::set_var("ORT_DYLIB_PATH", &path);
    prepend_runtime_dir_to_path(&path);
    Some(path)
}

fn nonempty_env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn canonical_if_file(path: &Path) -> Result<PathBuf, RuntimeError> {
    if !path.is_absolute() {
        return Err(RuntimeError::UnsafePath {
            path: path.to_path_buf(),
        });
    }
    path.canonicalize()
        .map_err(|error| RuntimeError::Canonicalize {
            path: path.to_path_buf(),
            message: error.to_string(),
        })
}

#[cfg(test)]
mod discovery_tests {
    use super::*;

    #[test]
    fn caller_dirs_follow_exe_locations_and_relative_dirs_are_skipped() {
        let abs = std::env::temp_dir().join("rightkit-ort-discovery-absent");
        let candidates = discovery_candidates([PathBuf::from("relative/runtime"), abs.clone()]);
        let last = candidates.last().expect("caller candidate");
        assert_eq!(last.path, abs.join(runtime_filename()));
        assert_eq!(last.source, CandidateSource::AppData);
        assert!(candidates
            .iter()
            .all(|c| !c.path.starts_with("relative/runtime")));
        let exe_pos = candidates
            .iter()
            .position(|c| c.source == CandidateSource::Bundled);
        assert!(exe_pos.is_none_or(|pos| pos < candidates.len() - 1));
    }

    #[test]
    fn discovery_finds_a_regular_file_and_ignores_a_directory() {
        let root = std::env::temp_dir().join(format!("rightkit-ort-disc-{}", std::process::id()));
        let dir_hit = root.join("dir");
        let file_hit = root.join("file");
        std::fs::create_dir_all(dir_hit.join(runtime_filename())).unwrap();
        std::fs::create_dir_all(&file_hit).unwrap();
        std::fs::write(file_hit.join(runtime_filename()), b"x").unwrap();
        let found = discover_runtime([dir_hit, file_hit.clone()]);
        // A developer shell may export ORT_DYLIB_PATH; it wins when it is a file.
        if nonempty_env_path("ORT_DYLIB_PATH").is_none_or(|p| !p.is_file()) {
            assert_eq!(found, Some(file_hit.join(runtime_filename())));
        }
        let _ = std::fs::remove_dir_all(root);
    }
}

#[cfg(test)]
mod binding_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[cfg(feature = "dev-overrides")]
    #[test]
    fn clearing_inherited_runtime_is_noop_with_dev_overrides() {
        clear_inherited_runtime_with(|| panic!("debug must preserve inherited runtime"));
    }

    #[cfg(not(feature = "dev-overrides"))]
    #[test]
    fn clearing_inherited_runtime_removes_it_by_default() {
        let mut inherited = Some(PathBuf::from("shell-exported-dev-runtime"));
        clear_inherited_runtime_with(|| inherited = None);
        assert_eq!(inherited, None);
    }

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let root = std::env::temp_dir().join(format!(
                "rightkit-ort-binding-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&root).unwrap();
            Self(root.canonicalize().unwrap())
        }

        fn candidate(&self, directory: &str, exists: bool) -> RuntimeCandidate {
            let path = self.0.join(directory).join(runtime_filename());
            if exists {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, b"not a real ORT binary").unwrap();
            }
            RuntimeCandidate::new(path, CandidateSource::Explicit)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn binding_defaults_preserve_conflicts_and_require_files() {
        let options = RuntimeBindingOptions::default();
        assert!(!options.force_replace);
        assert!(!options.allow_missing);
        let fixture = Fixture::new();
        let first = fixture.candidate("first", true);
        let second = fixture.candidate("second", true);
        let error = configure_runtime_with_env([second], &options, Some(first.path), |_| {
            panic!("must not overwrite")
        })
        .unwrap_err();
        assert!(matches!(error, RuntimeError::AlreadyConfigured { .. }));
        let error = configure_runtime_with_env(
            [fixture.candidate("missing", false)],
            &options,
            None,
            |_| panic!("must not bind missing path by default"),
        )
        .unwrap_err();
        assert!(matches!(error, RuntimeError::NotFound { .. }));
    }

    #[test]
    fn forced_replacement_overwrites_existing_or_invalid_configuration() {
        let fixture = Fixture::new();
        let first = fixture.candidate("first", true);
        let selected = fixture.candidate("second", true);
        for configured in [first.path, PathBuf::from("invalid-relative-path")] {
            let mut written = None;
            let report = configure_runtime_with_env(
                [selected.clone()],
                &RuntimeBindingOptions {
                    force_replace: true,
                    ..Default::default()
                },
                Some(configured),
                |path| written = Some(path.to_path_buf()),
            )
            .unwrap();
            let expected = selected.path.canonicalize().unwrap();
            assert_eq!(written, Some(expected.clone()));
            assert_eq!(report.selection.path, expected);
            assert_eq!(report.status, RuntimeBindingStatus::Ready);
        }
    }

    #[test]
    fn missing_mode_binds_records_and_reuses_first_absent_candidate() {
        let fixture = Fixture::new();
        let first = fixture.candidate("missing-first", false);
        let second = fixture.candidate("missing-second", false);
        let options = RuntimeBindingOptions {
            allow_missing: true,
            ..Default::default()
        };
        let mut written = None;
        let report = configure_runtime_with_env([first.clone(), second], &options, None, |path| {
            written = Some(path.to_path_buf())
        })
        .unwrap();
        assert_eq!(report.status, RuntimeBindingStatus::Missing);
        assert_eq!(report.selection.path, first.path);
        assert_eq!(written, Some(first.path.clone()));
        assert_eq!(report.selection.diagnostics.len(), 2);
        assert!(report
            .selection
            .diagnostics
            .iter()
            .all(|d| !d.exists && !d.is_file));
        let repeated =
            configure_runtime_with_env([first.clone()], &options, Some(first.path), |_| {
                panic!("matching binding must be preserved")
            })
            .unwrap();
        assert_eq!(repeated.status, RuntimeBindingStatus::Missing);
    }

    #[test]
    fn missing_mode_prefers_available_files_and_never_binds_directories() {
        let fixture = Fixture::new();
        let available = fixture.candidate("available", true);
        let options = RuntimeBindingOptions {
            allow_missing: true,
            ..Default::default()
        };
        let report = configure_runtime_with_env(
            [fixture.candidate("absent", false), available.clone()],
            &options,
            None,
            |_| {},
        )
        .unwrap();
        assert_eq!(report.status, RuntimeBindingStatus::Ready);
        assert_eq!(
            report.selection.path,
            available.path.canonicalize().unwrap()
        );
        let directory = fixture.candidate("directory", false);
        std::fs::create_dir_all(&directory.path).unwrap();
        let error = configure_runtime_with_env([directory], &options, None, |_| {
            panic!("directory must not be bound")
        })
        .unwrap_err();
        assert!(matches!(error, RuntimeError::NotFound { .. }));
    }

    #[test]
    fn forced_missing_binding_overwrites_existing_configuration() {
        let fixture = Fixture::new();
        let previous = fixture.candidate("previous", true);
        let missing = fixture.candidate("missing", false);
        let mut written = None;
        let report = configure_runtime_with_env(
            [missing.clone()],
            &RuntimeBindingOptions {
                force_replace: true,
                allow_missing: true,
            },
            Some(previous.path),
            |path| written = Some(path.to_path_buf()),
        )
        .unwrap();
        assert_eq!(report.status, RuntimeBindingStatus::Missing);
        assert_eq!(written, Some(missing.path));
    }

    #[test]
    fn permissive_options_still_reject_unsafe_paths_and_system_runtime() {
        let fixture = Fixture::new();
        let options = RuntimeBindingOptions {
            force_replace: true,
            allow_missing: true,
        };
        for (candidate, expected) in [
            (
                RuntimeCandidate::new(runtime_filename(), CandidateSource::Explicit),
                "relative",
            ),
            (
                RuntimeCandidate::new(fixture.0.join("wrong.bin"), CandidateSource::Explicit),
                "filename",
            ),
            (fixture.candidate("System32", false), "system"),
        ] {
            let error = configure_runtime_with_env([candidate], &options, None, |_| {
                panic!("unsafe candidate must not be bound")
            })
            .unwrap_err();
            assert!(match expected {
                "relative" => matches!(error, RuntimeError::UnsafePath { .. }),
                "filename" => matches!(error, RuntimeError::UnexpectedFilename { .. }),
                _ => matches!(error, RuntimeError::SystemRuntimeRejected { .. }),
            });
        }
    }

    fn versioned_runtime_name() -> &'static str {
        if cfg!(target_os = "windows") {
            "onnxruntime-1.24.4.dll"
        } else if cfg!(target_os = "macos") {
            "libonnxruntime.1.24.4.dylib"
        } else {
            "libonnxruntime.so.1.24.0"
        }
    }

    #[test]
    fn explicit_file_binds_versioned_and_renamed_files_while_strict_candidates_refuse() {
        let fixture = Fixture::new();
        for name in [versioned_runtime_name(), "ort-runtime.bin"] {
            let file = fixture.0.join("opt").join(name);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, b"not a real ORT binary").unwrap();
            let mut written = None;
            let binding = configure_runtime_with_env(
                [explicit_file_candidate(&file)],
                &RuntimeBindingOptions::default(),
                None,
                |path| written = Some(path.to_path_buf()),
            )
            .unwrap();
            let expected = file.canonicalize().unwrap();
            assert_eq!(binding.selection.path, expected, "{name}");
            assert_eq!(binding.selection.source, CandidateSource::ExplicitFile);
            assert_eq!(binding.status, RuntimeBindingStatus::Ready);
            assert_eq!(written, Some(expected));
            // The same path as a strict candidate keeps the exact-basename rule.
            let error = configure_runtime_with_env(
                [RuntimeCandidate::new(&file, CandidateSource::Explicit)],
                &RuntimeBindingOptions::default(),
                None,
                |_| panic!("strict candidate must not bind"),
            )
            .unwrap_err();
            assert!(
                matches!(error, RuntimeError::UnexpectedFilename { .. }),
                "{name}"
            );
        }
    }

    #[test]
    fn explicit_file_still_refuses_unsafe_and_non_file_paths() {
        let fixture = Fixture::new();
        let options = RuntimeBindingOptions::default();
        let refuse = |candidate: RuntimeCandidate| {
            configure_runtime_with_env([candidate], &options, None, |_| panic!("must not bind"))
                .unwrap_err()
        };
        // Relative: a library-search hijack risk, refused whatever the name.
        for relative in [versioned_runtime_name(), "./libonnxruntime.so.1", "x/y.bin"] {
            assert!(matches!(
                refuse(explicit_file_candidate(relative)),
                RuntimeError::UnsafePath { .. }
            ));
        }
        // Missing and directories are not regular files.
        let missing = fixture.0.join(versioned_runtime_name());
        assert!(matches!(
            refuse(explicit_file_candidate(&missing)),
            RuntimeError::NotFound { .. }
        ));
        let dir = fixture.0.join("a-directory.so.1");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(matches!(
            refuse(explicit_file_candidate(&dir)),
            RuntimeError::NotFound { .. }
        ));
        // The OS copy is refused by location.
        let system = fixture.0.join("System32").join(versioned_runtime_name());
        std::fs::create_dir_all(system.parent().unwrap()).unwrap();
        std::fs::write(&system, b"x").unwrap();
        assert!(matches!(
            refuse(explicit_file_candidate(&system)),
            RuntimeError::SystemRuntimeRejected { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn explicit_file_follows_symlinks_to_regular_files_only() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let real = fixture.0.join(versioned_runtime_name());
        std::fs::write(&real, b"x").unwrap();
        let link = fixture.0.join("alias");
        symlink(&real, &link).unwrap();
        let selected = resolve_runtime([explicit_file_candidate(&link)]).unwrap();
        assert_eq!(selected.path, real.canonicalize().unwrap());
        // A link whose target is the SysWOW64 copy is refused after resolution.
        let system = fixture.0.join("SysWOW64");
        std::fs::create_dir_all(&system).unwrap();
        let hidden = system.join("hidden.so");
        std::fs::write(&hidden, b"x").unwrap();
        let sneaky = fixture.0.join("innocent");
        symlink(&hidden, &sneaky).unwrap();
        assert!(matches!(
            resolve_runtime([explicit_file_candidate(&sneaky)]),
            Err(RuntimeError::SystemRuntimeRejected { .. })
        ));
        // A link to a directory is not a regular file.
        let dir = fixture.0.join("dir");
        std::fs::create_dir_all(&dir).unwrap();
        let dir_link = fixture.0.join("dir-link");
        symlink(&dir, &dir_link).unwrap();
        assert!(matches!(
            resolve_runtime([explicit_file_candidate(&dir_link)]),
            Err(RuntimeError::NotFound { .. })
        ));
    }

    #[test]
    fn android_candidate_is_the_native_lib_dir_file_and_refuses_relative_dirs() {
        let dir = std::env::temp_dir()
            .join("data")
            .join("app")
            .join("lib")
            .join("arm64");
        let candidate = android_runtime_candidate(&dir).unwrap();
        assert_eq!(candidate.source, CandidateSource::Bundled);
        assert_eq!(candidate.path, dir.join("libonnxruntime.so"));
        assert!(matches!(
            android_runtime_candidate(Path::new("lib/arm64")),
            Err(RuntimeError::UnsafePath { .. })
        ));
        assert!(matches!(
            configure_android_runtime(Path::new("")),
            Err(RuntimeError::UnsafePath { .. })
        ));
    }

    #[test]
    fn android_candidate_resolves_when_the_library_is_extracted_and_not_found_otherwise() {
        let fixture = Fixture::new();
        let lib = fixture.0.join("lib").join("arm64");
        // Not extracted (AGP default packaging): the path is not a file.
        assert!(matches!(
            resolve_runtime([android_runtime_candidate(&lib).unwrap()]),
            Err(RuntimeError::NotFound { .. }) | Err(RuntimeError::UnexpectedFilename { .. })
        ));
        // Extracted: on an Android host the strict basename matches; elsewhere the file name
        // is the host's, so only the candidate shape is asserted above.
        if runtime_filename() == "libonnxruntime.so" {
            std::fs::create_dir_all(&lib).unwrap();
            std::fs::write(lib.join("libonnxruntime.so"), b"not a real ORT binary").unwrap();
            let selected = resolve_runtime([android_runtime_candidate(&lib).unwrap()]).unwrap();
            assert_eq!(
                selected.path,
                lib.join("libonnxruntime.so").canonicalize().unwrap()
            );
        }
    }

    #[test]
    fn installed_and_legacy_candidates_keep_the_strict_basename() {
        let exe = std::env::temp_dir().join("App").join("app");
        let installed = installed_runtime_candidate(&exe, "runtime").unwrap();
        assert_eq!(installed.source, CandidateSource::Bundled);
        let legacy = legacy_runtime_candidate(&std::env::temp_dir()).unwrap();
        assert_eq!(legacy.source, CandidateSource::LegacyDefault);
        for candidate in [installed, legacy] {
            assert_eq!(
                candidate.path.file_name().and_then(|n| n.to_str()),
                Some(runtime_filename())
            );
            // Not ExplicitFile, so a versioned name would still be refused.
            let versioned = candidate.path.with_file_name(versioned_runtime_name());
            let error =
                resolve_runtime([RuntimeCandidate::new(versioned, candidate.source)]).unwrap_err();
            assert!(matches!(error, RuntimeError::UnexpectedFilename { .. }));
        }
    }
}

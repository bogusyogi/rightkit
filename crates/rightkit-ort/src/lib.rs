//! Product-neutral ONNX Runtime dynamic-library resolution, environment and
//! session setup for Right Suite apps.
//!
//! Merged from ScrapeRight `ort_common` (installed-bundle layout, system-DLL
//! hazard) and HeardRight `heardright-onnx-asr` (environment, execution
//! providers, session builder). Product environment variables and data roots
//! stay in app adapters.

#[cfg(feature = "session")]
pub mod environment;
#[cfg(feature = "session")]
pub mod probe;
#[cfg(feature = "session")]
pub mod session;

#[cfg(feature = "session")]
pub use environment::{
    cpu_thread_budget, init_environment, shared_pool_active, EnvironmentOptions, EnvironmentReport,
    GlobalPool,
};
#[cfg(feature = "session")]
pub use probe::{probe_providers, ProviderDiagnostic, ProviderKind};
#[cfg(feature = "session")]
pub use session::{ExecutionProvider, SessionError, SessionOptions};

use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateSource {
    Explicit,
    Bundled,
    AppData,
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
        if !diagnostic.expected_filename {
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

/// Configure ORT only after resolving a caller-supplied, absolute regular file.
/// Existing configuration is preserved and must canonicalize to the same file.
pub fn configure_runtime(
    candidates: impl IntoIterator<Item = RuntimeCandidate>,
) -> Result<RuntimeSelection, RuntimeError> {
    let selection = resolve_runtime(candidates)?;
    if let Some(configured) = nonempty_env_path("ORT_DYLIB_PATH") {
        let configured = canonical_if_file(&configured)?;
        if configured != selection.path {
            return Err(RuntimeError::AlreadyConfigured {
                configured,
                selected: selection.path,
            });
        }
        return Ok(selection);
    }
    std::env::set_var("ORT_DYLIB_PATH", &selection.path);
    Ok(selection)
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

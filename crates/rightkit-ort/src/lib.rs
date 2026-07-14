//! Product-neutral ONNX Runtime dynamic-library resolution and diagnostics.

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    DirectMl,
    CoreMl,
    Cpu,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderDiagnostic {
    pub provider: ProviderKind,
    pub available: bool,
    pub detail: String,
}

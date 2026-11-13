//! Find, verify, bind and report the installed ONNX Runtime in one call
//! ([`bind_installed_runtime`]), so an app declares only its [`BindSpec`] and keeps no glue of its
//! own: candidate discovery, the digest check, the `ORT_DYLIB_PATH` binding, the `PATH` entry for
//! sibling DLLs and the report a Doctor screen shows.
//!
//! The steps, in order (none of them loads the library; ort does that later):
//!
//! 1. [`clear_inherited_runtime_for_release`]\: an
//!    inherited `ORT_DYLIB_PATH` is dropped.
//! 2. Candidates, first regular file wins: each of [`BindSpec::leading_candidates`] (a path the host
//!    was handed), the bundled runtime beside the installed executable
//!    ([`installed_runtime_candidate`]), each of [`BindSpec::bundled_candidates`] (further bundled
//!    locations), then each of [`BindSpec::extra_dirs`] (app data). A bundled runtime beats an
//!    app-data copy.
//! 3. Verification of the resolved file, before anything is bound and before any environment
//!    variable is written: [`BindVerification::Manifest`] checks the `manifest.json` beside it
//!    ([`verify_library_in_manifest`]); `BindVerification::Catalog` (feature `catalog`) checks it
//!    against the native-library catalog for this host and also every companion file that sits
//!    beside it. [`BindVerification::None`] skips the check.
//! 4. The existing binding ([`configure_runtime_with_options`] semantics: the known-bad SME2
//!    refusal, the conflict check against an existing `ORT_DYLIB_PATH`), then the optional `PATH`
//!    entry, then the recorded binding ([`configured_runtime`]).
//!
//! The file is verified before it is bound, not while ort loads it: a local writer replacing it in
//! that window is outside the check, as for [`configure_runtime_verified`].

use std::cell::RefCell;
use std::fmt;
use std::path::{Path, PathBuf};

#[cfg(feature = "catalog")]
pub use rightkit_ffmpeg::native::{HostProof, NativeCatalogError, VerifiedLibrary};
#[cfg(feature = "catalog")]
use rightkit_ffmpeg::native::{NativeCatalog, NativeLibrary};

use crate::runtime_bundle::{verify_library_in_manifest, VerifiedFile};
use crate::{
    clear_inherited_runtime_for_release, configure_runtime_checked, installed_runtime_candidate,
    kleidiai, nonempty_env_path, prepend_runtime_dir_to_path, resolve_runtime, CandidateDiagnostic,
    CandidateSource, RuntimeBinding, RuntimeBindingOptions, RuntimeCandidate, RuntimeError,
    RuntimeSelection, CONFIGURED_RUNTIME,
};

/// How the resolved runtime file is verified before it is bound.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindVerification {
    /// The `manifest.json` beside the library lists it with its size and SHA-256
    /// ([`verify_library_in_manifest`]).
    Manifest,
    /// The native-library catalog: the file must be this host's `onnxruntime` row (an `approved`
    /// or `pinned` one) by name and digest, and every companion file of that row that sits beside
    /// it must match its own row. A companion that is absent is reported, not an error, so a
    /// CPU-only app can run without the DirectML library; [`BindSpec::require_companions`] turns
    /// an absent companion into [`VerificationFailure::MissingCompanions`]. Needs the `catalog`
    /// feature.
    #[cfg(feature = "catalog")]
    Catalog,
    /// No integrity check: the resolved file is bound as found. For a host that verified the
    /// runtime by other means (it ships signed inside its installer, or it checks the file itself)
    /// or that has never checked it; prefer [`BindVerification::Manifest`] or `Catalog` when an
    /// install can carry a `manifest.json` or a catalog row. The known-bad runtime refusal and the
    /// path safety checks still apply; the report carries [`VerificationReport::Unverified`].
    None,
}

/// What [`bind_installed_runtime`] needs from the app. Build it with [`BindSpec::new`] and set the
/// fields that differ from the defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindSpec {
    /// Candidates searched before everything else, in order: a runtime path the host was handed
    /// (by its shell, a resource resolver). Not filtered: the path checks of
    /// [`crate::resolve_runtime`] apply, so pass absolute paths. Default empty.
    pub leading_candidates: Vec<RuntimeCandidate>,
    /// Further bundled locations, searched after the `resource_subdir` one and before
    /// [`BindSpec::extra_dirs`] (for example the library directly beside the executable). Not
    /// filtered, like [`BindSpec::leading_candidates`]. Default empty.
    pub bundled_candidates: Vec<RuntimeCandidate>,
    /// App-data directories searched last, each joined with the platform file name. A relative
    /// directory is skipped, as in [`crate::discovery_candidates`].
    pub extra_dirs: Vec<PathBuf>,
    /// The installed executable the bundled candidate is derived from. `None` uses
    /// [`std::env::current_exe`] (no bundled candidate on Android, where that is the zygote: pass
    /// `nativeLibraryDir` in `extra_dirs`). A relative path yields no bundled candidate.
    pub executable: Option<PathBuf>,
    /// Directory of the bundled runtime inside the app's resource directory
    /// ([`crate::installed_resource_dir`]); [`BindSpec::new`] sets `runtime`. Relative.
    pub resource_subdir: String,
    pub verification: BindVerification,
    /// Put the runtime's directory first on `PATH` ([`crate::prepend_runtime_dir_to_path`]) so DLLs
    /// that sit beside it load. Windows resolves a library's own dependencies from `PATH`.
    ///
    /// Windows host proof pending: the directory put on `PATH` (and the file named by
    /// `ORT_DYLIB_PATH`) comes from the selected runtime's canonical path, and
    /// `std::fs::canonicalize` returns a verbatim path (`\\?\C:\...`) on Windows. The catalog
    /// check's own `VerifiedLibrary::path` is canonical the same way. This crate has not run a
    /// verbatim directory as a `PATH` entry (the loader's DLL dependency search) on a Windows
    /// host, so that this works is unproven. Run it on a Windows host before relying on
    /// `prepend_path` there, and strip the prefix of a non-UNC path in the app if the DirectML or
    /// other sibling DLLs do not load.
    pub prepend_path: bool,
    /// Overwrite an existing `ORT_DYLIB_PATH` ([`RuntimeBindingOptions::force_replace`]).
    pub force_replace: bool,
    /// QA only ([`RuntimeBindingOptions::allow_known_bad_runtime`]).
    pub allow_known_bad_runtime: bool,
    /// With [`BindVerification::Catalog`]: refuse the bind when a companion row of the runtime
    /// (for example `DirectML.dll` beside the Windows DirectML runtime) has no file beside it,
    /// with [`BindError::VerificationFailed`] carrying
    /// [`VerificationFailure::MissingCompanions`]. Nothing is bound. Default `false`: an absent
    /// companion is only listed in [`CatalogReport::companions_missing`], so a CPU-only app runs
    /// without it. A companion that is present is always verified, whatever this says. No effect
    /// with [`BindVerification::Manifest`] (a manifest names no companions). Needs the `catalog`
    /// feature.
    #[cfg(feature = "catalog")]
    pub require_companions: bool,
}

impl BindSpec {
    /// A spec with the `runtime` resource directory, no override, no extra directories, no `PATH`
    /// entry and the default binding policy.
    pub fn new(verification: BindVerification) -> Self {
        Self {
            leading_candidates: Vec::new(),
            bundled_candidates: Vec::new(),
            extra_dirs: Vec::new(),
            executable: None,
            resource_subdir: "runtime".to_owned(),
            verification,
            prepend_path: false,
            force_replace: false,
            allow_known_bad_runtime: false,
            #[cfg(feature = "catalog")]
            require_companions: false,
        }
    }
}

/// A runtime that passed verification and is bound.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundRuntime {
    pub binding: RuntimeBinding,
    pub report: RuntimeReport,
}

/// What a Doctor screen shows about the bound runtime.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeReport {
    /// Canonical path of the selected runtime file, the one that was verified and bound.
    pub path: PathBuf,
    pub source: CandidateSource,
    /// The candidates inspected up to and including the selected one, in search order.
    pub candidates: Vec<CandidateDiagnostic>,
    pub verification: VerificationReport,
}

/// Which check passed, with the identity it established.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerificationReport {
    /// The manifest entry of the library (its listed path, SHA-256 and size).
    Manifest(VerifiedFile),
    #[cfg(feature = "catalog")]
    Catalog(CatalogReport),
    /// [`BindVerification::None`]: the file was bound without an integrity check.
    Unverified,
}

/// The catalog outcome: the runtime row and the companions found beside the runtime.
#[cfg(feature = "catalog")]
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogReport {
    /// The runtime's row (`id`, `version`, `status`, `host_proof`).
    pub library: VerifiedLibrary,
    /// Companion files that were present and matched their rows.
    pub companions_verified: Vec<VerifiedLibrary>,
    /// Companion rows whose file is absent beside the runtime.
    pub companions_missing: Vec<MissingCompanion>,
}

#[cfg(feature = "catalog")]
impl CatalogReport {
    /// `Some(HostProof::NeverRun)` when the runtime's row was never run on a host of its OS.
    pub fn host_proof(&self) -> Option<HostProof> {
        self.library.host_proof()
    }
}

/// A companion row whose file is not beside the runtime.
#[cfg(feature = "catalog")]
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingCompanion {
    /// Catalog row id, for example `directml-1.15.4-windows-x64`.
    pub id: String,
    pub file_name: String,
}

/// Why [`bind_installed_runtime`] did not bind.
#[non_exhaustive]
#[derive(Debug)]
pub enum BindError {
    /// No candidate is a regular file. `candidates` is what was inspected, in search order
    /// (empty when the spec produced no candidate at all).
    NoRuntime {
        candidates: Vec<CandidateDiagnostic>,
    },
    /// A candidate or the binding was refused: an unsafe or `System32` path, a wrong file name, a
    /// conflicting `ORT_DYLIB_PATH` or [`RuntimeError::KnownBadRuntime`].
    Rejected(RuntimeError),
    /// The resolved file is not the one the manifest or the catalog describes. Nothing was bound.
    VerificationFailed {
        path: PathBuf,
        reason: VerificationFailure,
    },
}

/// The verification error behind [`BindError::VerificationFailed`].
#[non_exhaustive]
#[derive(Debug)]
pub enum VerificationFailure {
    Manifest(RuntimeError),
    #[cfg(feature = "catalog")]
    Catalog(NativeCatalogError),
    /// [`BindSpec::require_companions`] is set and these companion rows have no file beside the
    /// runtime. The runtime and every companion that is present matched their rows.
    #[cfg(feature = "catalog")]
    MissingCompanions(Vec<MissingCompanion>),
}

impl fmt::Display for BindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoRuntime { candidates } => write!(
                f,
                "no ONNX Runtime found; {} candidate(s) inspected",
                candidates.len()
            ),
            Self::Rejected(error) => write!(f, "ONNX Runtime rejected: {error}"),
            Self::VerificationFailed { path, reason } => {
                write!(f, "ONNX Runtime {} failed verification: ", path.display())?;
                match reason {
                    VerificationFailure::Manifest(error) => write!(f, "{error}"),
                    #[cfg(feature = "catalog")]
                    VerificationFailure::Catalog(error) => write!(f, "{error}"),
                    #[cfg(feature = "catalog")]
                    VerificationFailure::MissingCompanions(missing) => {
                        write!(f, "required companion file(s) missing beside the runtime:")?;
                        for companion in missing {
                            write!(f, " {} ({})", companion.file_name, companion.id)?;
                        }
                        Ok(())
                    }
                }
            }
        }
    }
}

impl std::error::Error for BindError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::NoRuntime { .. } => None,
            Self::Rejected(error) => Some(error),
            Self::VerificationFailed { reason, .. } => match reason {
                VerificationFailure::Manifest(error) => Some(error),
                #[cfg(feature = "catalog")]
                VerificationFailure::Catalog(error) => Some(error),
                #[cfg(feature = "catalog")]
                VerificationFailure::MissingCompanions(_) => None,
            },
        }
    }
}

/// Find, verify and bind the installed ONNX Runtime described by `spec`, and report it (see the
/// module documentation for the order). Nothing is bound and no environment variable other than
/// the inherited `ORT_DYLIB_PATH` (cleared first) is touched unless verification passed.
///
/// Call once at startup, single-threaded, before any ORT use: it removes and sets process
/// environment variables, which needs exclusive access to the environment (see
/// [`clear_inherited_runtime_for_release`]). It does not load the library or replace one that ort
/// already loaded.
pub fn bind_installed_runtime(spec: &BindSpec) -> Result<BoundRuntime, BindError> {
    bind_with(
        spec,
        &mut ProcessHost {
            runtime_check: None,
        },
    )
}

/// Everything [`bind_with`] reads from or writes to the process, so unit tests drive it without
/// touching the real environment.
trait BindHost {
    fn clear_inherited_runtime(&mut self);
    fn env_path(&self, name: &str) -> Option<PathBuf>;
    fn current_executable(&self) -> Option<PathBuf>;
    /// The bind-time runtime check (the known-bad SME2 guard) on the resolved file.
    fn check_runtime(
        &mut self,
        selection: &RuntimeSelection,
        allow_known_bad: bool,
    ) -> Result<(), RuntimeError>;
    fn set_runtime_env(&mut self, path: &Path);
    fn prepend_to_path(&mut self, runtime: &Path);
    /// Record the finished binding for [`crate::configured_runtime`].
    fn record(&mut self, binding: &RuntimeBinding);
    #[cfg(feature = "catalog")]
    fn catalog(&self) -> Result<CatalogView, NativeCatalogError>;
}

/// The catalog and the platform a runtime is verified for.
#[cfg(feature = "catalog")]
struct CatalogView {
    catalog: NativeCatalog,
    os: String,
    arch: String,
}

struct ProcessHost {
    runtime_check: Option<kleidiai::BindCheck>,
}

impl BindHost for ProcessHost {
    fn clear_inherited_runtime(&mut self) {
        clear_inherited_runtime_for_release();
    }

    fn env_path(&self, name: &str) -> Option<PathBuf> {
        nonempty_env_path(name)
    }

    fn current_executable(&self) -> Option<PathBuf> {
        // On Android `current_exe` is the zygote's `app_process`, not the app.
        #[cfg(not(target_os = "android"))]
        {
            std::env::current_exe().ok()
        }
        #[cfg(target_os = "android")]
        {
            None
        }
    }

    fn check_runtime(
        &mut self,
        selection: &RuntimeSelection,
        allow_known_bad: bool,
    ) -> Result<(), RuntimeError> {
        self.runtime_check = Some(kleidiai::check_bind(&selection.path, allow_known_bad)?);
        Ok(())
    }

    fn set_runtime_env(&mut self, path: &Path) {
        std::env::set_var("ORT_DYLIB_PATH", path);
    }

    fn prepend_to_path(&mut self, runtime: &Path) {
        prepend_runtime_dir_to_path(runtime);
    }

    fn record(&mut self, binding: &RuntimeBinding) {
        if let Some(check) = self.runtime_check.take() {
            check.commit();
        }
        *CONFIGURED_RUNTIME
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            Some(binding.selection.path.clone());
    }

    #[cfg(feature = "catalog")]
    fn catalog(&self) -> Result<CatalogView, NativeCatalogError> {
        Ok(CatalogView {
            catalog: NativeCatalog::embedded()?,
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
        })
    }
}

fn bind_with(spec: &BindSpec, host: &mut dyn BindHost) -> Result<BoundRuntime, BindError> {
    host.clear_inherited_runtime();

    let selection = match resolve_runtime(candidates(spec, host)) {
        Ok(selection) => selection,
        Err(RuntimeError::NoCandidates) => {
            return Err(BindError::NoRuntime {
                candidates: Vec::new(),
            })
        }
        Err(RuntimeError::NotFound { diagnostics }) => {
            return Err(BindError::NoRuntime {
                candidates: diagnostics,
            })
        }
        Err(error) => return Err(BindError::Rejected(error)),
    };

    // Verify the resolved file before anything is bound or written.
    let verification = verify(spec, host, &selection.path)?;

    let options = RuntimeBindingOptions {
        force_replace: spec.force_replace,
        allow_missing: false,
        allow_known_bad_runtime: spec.allow_known_bad_runtime,
    };
    let configured = host.env_path("ORT_DYLIB_PATH");
    let host = RefCell::new(host);
    let mut binding = configure_runtime_checked(
        [RuntimeCandidate::new(
            selection.path.clone(),
            selection.source,
        )],
        &options,
        configured,
        |path| host.borrow_mut().set_runtime_env(path),
        |checked| {
            host.borrow_mut()
                .check_runtime(checked, options.allow_known_bad_runtime)
        },
    )
    .map_err(BindError::Rejected)?;
    // The binding resolved the one verified path; keep the whole search in its diagnostics.
    binding.selection.diagnostics = selection.diagnostics.clone();

    let mut host = host.borrow_mut();
    host.record(&binding);
    if spec.prepend_path {
        host.prepend_to_path(&binding.selection.path);
    }
    let report = RuntimeReport {
        path: selection.path,
        source: selection.source,
        candidates: selection.diagnostics,
        verification,
    };
    Ok(BoundRuntime { binding, report })
}

/// The search order: the leading candidates, bundled beside the executable, the further bundled
/// candidates, app-data directories.
fn candidates(spec: &BindSpec, host: &dyn BindHost) -> Vec<RuntimeCandidate> {
    let mut out = spec.leading_candidates.clone();
    let executable = spec
        .executable
        .clone()
        .or_else(|| host.current_executable());
    if let Some(candidate) =
        executable.and_then(|exe| installed_runtime_candidate(&exe, &spec.resource_subdir))
    {
        out.push(candidate);
    }
    out.extend(spec.bundled_candidates.iter().cloned());
    out.extend(
        spec.extra_dirs
            .iter()
            .filter(|dir| dir.is_absolute())
            .map(|dir| {
                RuntimeCandidate::new(
                    dir.join(crate::runtime_filename()),
                    CandidateSource::AppData,
                )
            }),
    );
    out
}

#[cfg_attr(not(feature = "catalog"), allow(unused_variables))]
fn verify(
    spec: &BindSpec,
    host: &dyn BindHost,
    path: &Path,
) -> Result<VerificationReport, BindError> {
    let failed = |reason| BindError::VerificationFailed {
        path: path.to_path_buf(),
        reason,
    };
    match spec.verification {
        BindVerification::Manifest => verify_library_in_manifest(path)
            .map(VerificationReport::Manifest)
            .map_err(|error| failed(VerificationFailure::Manifest(error))),
        #[cfg(feature = "catalog")]
        BindVerification::Catalog => {
            let report = verify_catalog(host, path)
                .map_err(|error| failed(VerificationFailure::Catalog(error)))?;
            if spec.require_companions && !report.companions_missing.is_empty() {
                return Err(failed(VerificationFailure::MissingCompanions(
                    report.companions_missing,
                )));
            }
            Ok(VerificationReport::Catalog(report))
        }
        BindVerification::None => Ok(VerificationReport::Unverified),
    }
}

/// The runtime against its catalog row, then every companion file of that row that sits beside it.
#[cfg(feature = "catalog")]
fn verify_catalog(host: &dyn BindHost, path: &Path) -> Result<CatalogReport, NativeCatalogError> {
    use rightkit_ffmpeg::native::verified_library_in;
    use std::io::ErrorKind;

    let CatalogView { catalog, os, arch } = host.catalog()?;
    let library = verified_library_in(&catalog, &os, &arch, NativeLibrary::Onnxruntime, path)?;
    let invalid = |id: &str, reason: &str| NativeCatalogError::Invalid {
        id: id.to_owned(),
        reason: reason.to_owned(),
    };
    let row = catalog
        .libraries
        .iter()
        .find(|row| row.id == library.id())
        .ok_or_else(|| invalid(library.id(), "the verified row is not in the catalog"))?;
    let dir = library
        .path()
        .parent()
        .ok_or_else(|| invalid(library.id(), "the runtime path has no directory"))?;

    let mut companions_verified = Vec::new();
    let mut companions_missing = Vec::new();
    for id in &row.companions {
        let companion = catalog
            .libraries
            .iter()
            .find(|row| row.id == *id)
            .ok_or_else(|| invalid(id, "the companion is not in the catalog"))?;
        let file_name = companion
            .artifact_file_name
            .as_deref()
            .ok_or_else(|| invalid(id, "the companion has no artifactFileName"))?;
        let file = dir.join(file_name);
        match std::fs::symlink_metadata(&file) {
            Err(error) if error.kind() == ErrorKind::NotFound => {
                companions_missing.push(MissingCompanion {
                    id: id.clone(),
                    file_name: file_name.to_owned(),
                });
            }
            Err(error) => return Err(error.into()),
            // Present: a symbolic link, a directory or a different file is an error, not "missing".
            Ok(_) => companions_verified.push(verified_library_in(
                &catalog,
                &os,
                &arch,
                companion.library,
                &file,
            )?),
        }
    }
    Ok(CatalogReport {
        library,
        companions_verified,
        companions_missing,
    })
}

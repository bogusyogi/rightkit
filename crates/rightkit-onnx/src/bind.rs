//! Find, verify, bind and report the installed ONNX Runtime in one call
//! ([`bind_installed_runtime`]), so an app declares only its [`BindSpec`] and keeps no glue of its
//! own: candidate discovery, the digest check, the `ORT_DYLIB_PATH` binding, the `PATH` entry for
//! sibling DLLs and the report a Doctor screen shows.
//!
//! The steps, in order (none of them loads the library; ort does that later):
//!
//! 1. [`clear_inherited_runtime_for_release`](crate::clear_inherited_runtime_for_release): an
//!    inherited `ORT_DYLIB_PATH` is dropped unless the `dev-overrides` feature is on.
//! 2. Candidates, first regular file wins: the developer override named by
//!    [`BindSpec::developer_env`] (only with `dev-overrides`; a release build never reads the
//!    variable), then the bundled runtime beside the installed executable
//!    ([`installed_runtime_candidate`]), then each of [`BindSpec::extra_dirs`] (app data). A
//!    bundled runtime beats an app-data copy.
//! 3. Verification of the resolved file, before anything is bound and before any environment
//!    variable is written: [`BindVerification::Manifest`] checks the `manifest.json` beside it
//!    ([`verify_library_in_manifest`]); `BindVerification::Catalog` (feature `catalog`) checks it
//!    against the native-library catalog for this host and also every companion file that sits
//!    beside it.
//! 4. The existing binding ([`configure_runtime_with_options`] semantics: the known-bad SME2
//!    refusal, the conflict check against an existing `ORT_DYLIB_PATH`), then the optional `PATH`
//!    entry, then the recorded binding ([`configured_runtime`]).
//!
//! The file is verified before it is bound, not while ort loads it: a local writer replacing it in
//! that window is outside the check, as for [`configure_runtime_verified`](crate::configure_runtime_verified).

use std::cell::RefCell;
use std::fmt;
use std::path::{Path, PathBuf};

#[cfg(feature = "catalog")]
pub use rightkit_ffmpeg::native::{HostProof, NativeCatalogError, VerifiedLibrary};
#[cfg(feature = "catalog")]
use rightkit_ffmpeg::native::{NativeCatalog, NativeLibrary};

use crate::runtime_bundle::{verify_library_in_manifest, VerifiedFile};
use crate::{
    clear_inherited_runtime_for_release, configure_runtime_checked, developer_override_with,
    installed_runtime_candidate, kleidiai, nonempty_env_path, prepend_runtime_dir_to_path,
    resolve_runtime, CandidateDiagnostic, CandidateSource, RuntimeBinding, RuntimeBindingOptions,
    RuntimeCandidate, RuntimeError, RuntimeSelection, CONFIGURED_RUNTIME,
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
    /// CPU-only app can run without the DirectML library. Needs the `catalog` feature.
    #[cfg(feature = "catalog")]
    Catalog,
}

/// What [`bind_installed_runtime`] needs from the app. Build it with [`BindSpec::new`] and set the
/// fields that differ from the defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindSpec {
    /// Name of the app's development override variable (for example `MYAPP_ORT_DYLIB`), read only
    /// with the `dev-overrides` feature: a release build never reads it. Its value names the
    /// runtime file and must end in the platform file name ([`crate::runtime_filename`]).
    pub developer_env: Option<String>,
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
    pub prepend_path: bool,
    /// Overwrite an existing `ORT_DYLIB_PATH` ([`RuntimeBindingOptions::force_replace`]).
    pub force_replace: bool,
    /// QA only ([`RuntimeBindingOptions::allow_known_bad_runtime`]).
    pub allow_known_bad_runtime: bool,
}

impl BindSpec {
    /// A spec with the `runtime` resource directory, no override, no extra directories, no `PATH`
    /// entry and the default binding policy.
    pub fn new(verification: BindVerification) -> Self {
        Self {
            developer_env: None,
            extra_dirs: Vec::new(),
            executable: None,
            resource_subdir: "runtime".to_owned(),
            verification,
            prepend_path: false,
            force_replace: false,
            allow_known_bad_runtime: false,
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

/// The search order: developer override, bundled beside the executable, app-data directories.
fn candidates(spec: &BindSpec, host: &dyn BindHost) -> Vec<RuntimeCandidate> {
    let mut out = Vec::new();
    if let Some(name) = &spec.developer_env {
        out.extend(developer_override_with(name, |name| host.env_path(name)));
    }
    let executable = spec
        .executable
        .clone()
        .or_else(|| host.current_executable());
    if let Some(candidate) =
        executable.and_then(|exe| installed_runtime_candidate(&exe, &spec.resource_subdir))
    {
        out.push(candidate);
    }
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
        BindVerification::Catalog => verify_catalog(host, path)
            .map(VerificationReport::Catalog)
            .map_err(|error| failed(VerificationFailure::Catalog(error))),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_filename;
    use serde_json::json;
    use sha2::{Digest, Sha256};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const ENV_NAME: &str = "MYAPP_ORT_DYLIB";

    fn sha256_hex(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// Records every effect instead of touching the process; the log order is asserted.
    #[derive(Default)]
    struct FakeHost {
        env: HashMap<String, PathBuf>,
        executable: Option<PathBuf>,
        known_bad: bool,
        events: Vec<String>,
        runtime_env: Option<PathBuf>,
        path_prepends: Vec<PathBuf>,
        recorded: Option<PathBuf>,
        #[cfg(feature = "catalog")]
        catalog: Option<(NativeCatalog, &'static str, &'static str)>,
        env_reads: RefCell<Vec<String>>,
    }

    impl BindHost for FakeHost {
        fn clear_inherited_runtime(&mut self) {
            self.events.push("clear".into());
        }

        fn env_path(&self, name: &str) -> Option<PathBuf> {
            self.env_reads.borrow_mut().push(name.to_owned());
            self.env.get(name).cloned()
        }

        fn current_executable(&self) -> Option<PathBuf> {
            self.executable.clone()
        }

        fn check_runtime(
            &mut self,
            selection: &RuntimeSelection,
            allow_known_bad: bool,
        ) -> Result<(), RuntimeError> {
            self.events.push(format!("check allow={allow_known_bad}"));
            if self.known_bad && !allow_known_bad {
                return Err(RuntimeError::KnownBadRuntime {
                    path: selection.path.clone(),
                    version: "1.26.0".into(),
                    host_feature: kleidiai::HOST_FEATURE,
                    issue: kleidiai::ISSUE,
                });
            }
            Ok(())
        }

        fn set_runtime_env(&mut self, path: &Path) {
            self.events.push("set_runtime".into());
            self.runtime_env = Some(path.to_path_buf());
        }

        fn prepend_to_path(&mut self, runtime: &Path) {
            self.events.push("prepend_path".into());
            self.path_prepends.push(runtime.to_path_buf());
        }

        fn record(&mut self, binding: &RuntimeBinding) {
            self.events.push("record".into());
            self.recorded = Some(binding.selection.path.clone());
        }

        #[cfg(feature = "catalog")]
        fn catalog(&self) -> Result<CatalogView, NativeCatalogError> {
            let (catalog, os, arch) = self.catalog.clone().expect("a catalog test injects one");
            Ok(CatalogView {
                catalog,
                os: os.to_owned(),
                arch: arch.to_owned(),
            })
        }
    }

    impl FakeHost {
        /// No effect on the process happened: nothing written, bound or recorded.
        fn assert_untouched(&self) {
            assert_eq!(self.runtime_env, None, "events: {:?}", self.events);
            assert_eq!(self.recorded, None);
            assert!(self.path_prepends.is_empty());
        }
    }

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let root = std::env::temp_dir().join(format!(
                "rightkit-ort-bind-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&root).unwrap();
            Self(root.canonicalize().unwrap())
        }

        fn executable(&self) -> PathBuf {
            self.0.join("App").join("app")
        }

        /// Where the installed layout puts the bundled runtime.
        fn bundled_dir(&self) -> PathBuf {
            installed_runtime_candidate(&self.executable(), "runtime")
                .unwrap()
                .path
                .parent()
                .unwrap()
                .to_path_buf()
        }

        fn app_data_dir(&self) -> PathBuf {
            self.0.join("appdata")
        }

        /// Writes a runtime file with a manifest that lists it with the right digest.
        fn runtime_in(&self, dir: &Path, bytes: &[u8]) -> PathBuf {
            let file = self.bare_runtime_in(dir, bytes);
            self.manifest(dir, &sha256_hex(bytes), bytes.len());
            file
        }

        fn bare_runtime_in(&self, dir: &Path, bytes: &[u8]) -> PathBuf {
            std::fs::create_dir_all(dir).unwrap();
            let file = dir.join(runtime_filename());
            std::fs::write(&file, bytes).unwrap();
            file.canonicalize().unwrap()
        }

        fn manifest(&self, dir: &Path, digest: &str, bytes: usize) {
            let manifest = json!({
                "files": [{ "path": runtime_filename(), "sha256": digest, "bytes": bytes }]
            });
            std::fs::write(
                dir.join("manifest.json"),
                serde_json::to_vec(&manifest).unwrap(),
            )
            .unwrap();
        }

        fn host(&self) -> FakeHost {
            FakeHost {
                executable: Some(self.executable()),
                ..Default::default()
            }
        }

        fn spec(&self) -> BindSpec {
            BindSpec {
                developer_env: Some(ENV_NAME.into()),
                extra_dirs: vec![self.app_data_dir()],
                ..BindSpec::new(BindVerification::Manifest)
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn it_clears_the_inherited_runtime_before_anything_else() {
        let fixture = Fixture::new();
        fixture.runtime_in(&fixture.bundled_dir(), b"bundled");
        let mut host = fixture.host();
        bind_with(&fixture.spec(), &mut host).unwrap();
        assert_eq!(host.events.first().map(String::as_str), Some("clear"));
    }

    #[test]
    fn a_bundled_runtime_beats_an_app_data_copy() {
        let fixture = Fixture::new();
        let bundled = fixture.runtime_in(&fixture.bundled_dir(), b"bundled");
        fixture.runtime_in(&fixture.app_data_dir(), b"app data copy");
        let mut host = fixture.host();
        let bound = bind_with(&fixture.spec(), &mut host).unwrap();
        assert_eq!(bound.report.path, bundled);
        assert_eq!(bound.report.source, CandidateSource::Bundled);
        assert_eq!(bound.binding.selection.path, bundled);
        assert_eq!(host.runtime_env, Some(bundled));
    }

    #[test]
    fn app_data_is_used_when_nothing_is_bundled() {
        let fixture = Fixture::new();
        let app_data = fixture.runtime_in(&fixture.app_data_dir(), b"app data copy");
        let mut host = fixture.host();
        let bound = bind_with(&fixture.spec(), &mut host).unwrap();
        assert_eq!(bound.report.path, app_data);
        assert_eq!(bound.report.source, CandidateSource::AppData);
        // The report keeps the search: the absent bundled candidate, then the one selected.
        let sources: Vec<_> = bound.report.candidates.iter().map(|c| c.source).collect();
        assert_eq!(
            sources,
            [CandidateSource::Bundled, CandidateSource::AppData]
        );
        assert!(!bound.report.candidates[0].exists);
        assert!(bound.report.candidates[1].is_file);
        assert_eq!(bound.binding.selection.diagnostics, bound.report.candidates);
    }

    #[test]
    fn a_relative_extra_dir_is_skipped() {
        let fixture = Fixture::new();
        let mut host = fixture.host();
        let spec = BindSpec {
            extra_dirs: vec![PathBuf::from("relative/runtime")],
            ..fixture.spec()
        };
        let Err(BindError::NoRuntime { candidates }) = bind_with(&spec, &mut host) else {
            panic!("expected NoRuntime");
        };
        assert!(candidates.iter().all(|c| c.absolute));
    }

    #[cfg(not(feature = "dev-overrides"))]
    #[test]
    fn a_release_build_ignores_the_developer_variable() {
        let fixture = Fixture::new();
        let developer = fixture.runtime_in(&fixture.0.join("dev"), b"developer build");
        let bundled = fixture.runtime_in(&fixture.bundled_dir(), b"bundled");
        let mut host = fixture.host();
        host.env.insert(ENV_NAME.into(), developer);
        let bound = bind_with(&fixture.spec(), &mut host).unwrap();
        assert_eq!(bound.report.path, bundled);
        assert!(
            !host.env_reads.borrow().iter().any(|name| name == ENV_NAME),
            "a release build must not even read the variable"
        );
    }

    #[cfg(feature = "dev-overrides")]
    #[test]
    fn with_dev_overrides_the_developer_variable_wins() {
        let fixture = Fixture::new();
        let developer = fixture.runtime_in(&fixture.0.join("dev"), b"developer build");
        fixture.runtime_in(&fixture.bundled_dir(), b"bundled");
        let mut host = fixture.host();
        host.env.insert(ENV_NAME.into(), developer.clone());
        let bound = bind_with(&fixture.spec(), &mut host).unwrap();
        assert_eq!(bound.report.path, developer);
        assert_eq!(bound.report.source, CandidateSource::Explicit);
    }

    #[test]
    fn a_missing_runtime_is_a_typed_error_with_the_diagnostics() {
        let fixture = Fixture::new();
        let mut host = fixture.host();
        let Err(BindError::NoRuntime { candidates }) = bind_with(&fixture.spec(), &mut host) else {
            panic!("expected NoRuntime");
        };
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].source, CandidateSource::Bundled);
        assert_eq!(candidates[1].source, CandidateSource::AppData);
        assert!(candidates.iter().all(|c| !c.exists && !c.is_file));
        host.assert_untouched();

        // No executable, no directory: nothing to inspect, still the same error.
        let mut nowhere = FakeHost::default();
        let spec = BindSpec::new(BindVerification::Manifest);
        assert!(matches!(
            bind_with(&spec, &mut nowhere),
            Err(BindError::NoRuntime { candidates }) if candidates.is_empty()
        ));
    }

    #[test]
    fn the_system32_copy_is_refused() {
        let fixture = Fixture::new();
        let system = fixture.0.join("System32");
        fixture.runtime_in(&system, b"os copy");
        let mut host = fixture.host();
        let spec = BindSpec {
            extra_dirs: vec![system],
            ..fixture.spec()
        };
        assert!(matches!(
            bind_with(&spec, &mut host),
            Err(BindError::Rejected(
                RuntimeError::SystemRuntimeRejected { .. }
            ))
        ));
        host.assert_untouched();
    }

    #[test]
    fn a_known_bad_runtime_is_an_error_and_binds_nothing() {
        let fixture = Fixture::new();
        fixture.runtime_in(&fixture.bundled_dir(), b"old runtime");
        let mut host = fixture.host();
        host.known_bad = true;
        assert!(matches!(
            bind_with(&fixture.spec(), &mut host),
            Err(BindError::Rejected(RuntimeError::KnownBadRuntime { .. }))
        ));
        host.assert_untouched();

        // The QA opt-out reaches the check and binds the same file.
        let spec = BindSpec {
            allow_known_bad_runtime: true,
            ..fixture.spec()
        };
        let mut qa = fixture.host();
        qa.known_bad = true;
        bind_with(&spec, &mut qa).unwrap();
        assert!(qa.events.contains(&"check allow=true".to_owned()));
        assert!(qa.runtime_env.is_some());
    }

    #[test]
    fn a_different_inherited_binding_conflicts_unless_replacement_is_forced() {
        let fixture = Fixture::new();
        let bundled = fixture.runtime_in(&fixture.bundled_dir(), b"bundled");
        let other = fixture.runtime_in(&fixture.0.join("other"), b"other");
        let mut host = fixture.host();
        host.env.insert("ORT_DYLIB_PATH".into(), other);
        assert!(matches!(
            bind_with(&fixture.spec(), &mut host),
            Err(BindError::Rejected(RuntimeError::AlreadyConfigured { .. }))
        ));
        host.assert_untouched();

        let spec = BindSpec {
            force_replace: true,
            ..fixture.spec()
        };
        bind_with(&spec, &mut host).unwrap();
        assert_eq!(host.runtime_env, Some(bundled));
    }

    // Without the `catalog` feature the manifest report is the only variant.
    #[allow(irrefutable_let_patterns)]
    #[test]
    fn the_manifest_mode_binds_a_listed_runtime_and_reports_its_entry() {
        let fixture = Fixture::new();
        let bytes = b"onnxruntime bytes";
        let bundled = fixture.runtime_in(&fixture.bundled_dir(), bytes);
        let mut host = fixture.host();
        let bound = bind_with(&fixture.spec(), &mut host).unwrap();
        let VerificationReport::Manifest(file) = &bound.report.verification else {
            panic!("expected the manifest report");
        };
        assert_eq!(file.path, runtime_filename());
        assert_eq!(file.sha256, sha256_hex(bytes));
        assert_eq!(file.bytes, bytes.len() as u64);
        assert_eq!(bound.binding.status, crate::RuntimeBindingStatus::Ready);
        assert_eq!(host.runtime_env, Some(bundled.clone()));
        assert_eq!(host.recorded, Some(bundled));
    }

    #[test]
    fn a_tampered_manifest_runtime_is_refused_before_anything_is_bound() {
        let fixture = Fixture::new();
        let dir = fixture.bundled_dir();
        fixture.runtime_in(&dir, b"onnxruntime bytes");
        std::fs::write(dir.join(runtime_filename()), b"tampered bytes!!").unwrap();
        let mut host = fixture.host();
        let Err(BindError::VerificationFailed { path, reason }) =
            bind_with(&fixture.spec(), &mut host)
        else {
            panic!("expected a verification failure");
        };
        assert_eq!(path, dir.canonicalize().unwrap().join(runtime_filename()));
        assert!(matches!(
            reason,
            VerificationFailure::Manifest(RuntimeError::BundleFileHash { .. })
                | VerificationFailure::Manifest(RuntimeError::BundleFileSize { .. })
        ));
        host.assert_untouched();
        assert!(!host.events.iter().any(|e| e.starts_with("check")));
    }

    #[test]
    fn a_runtime_without_a_manifest_is_refused() {
        let fixture = Fixture::new();
        fixture.bare_runtime_in(&fixture.bundled_dir(), b"no manifest beside me");
        let mut host = fixture.host();
        assert!(matches!(
            bind_with(&fixture.spec(), &mut host),
            Err(BindError::VerificationFailed {
                reason: VerificationFailure::Manifest(RuntimeError::BundleManifest { .. }),
                ..
            })
        ));
        host.assert_untouched();
    }

    #[test]
    fn the_path_entry_is_added_once_after_the_binding_and_only_when_asked() {
        let fixture = Fixture::new();
        let bundled = fixture.runtime_in(&fixture.bundled_dir(), b"bundled");
        let mut plain = fixture.host();
        bind_with(&fixture.spec(), &mut plain).unwrap();
        assert!(plain.path_prepends.is_empty());

        let spec = BindSpec {
            prepend_path: true,
            ..fixture.spec()
        };
        let mut host = fixture.host();
        bind_with(&spec, &mut host).unwrap();
        assert_eq!(host.path_prepends, [bundled]);
        assert_eq!(
            host.events,
            [
                "clear",
                "check allow=false",
                "set_runtime",
                "record",
                "prepend_path"
            ]
        );
    }

    #[cfg(feature = "catalog")]
    mod catalog {
        use super::*;
        use rightkit_ffmpeg::native::NativeCatalogError;

        const ORT_ID: &str = "onnxruntime-fixture-windows-x64";
        const DML_ID: &str = "directml-fixture-windows-x64";
        const ORT_BYTES: &[u8] = b"fixture onnxruntime";
        const DML_BYTES: &[u8] = b"fixture directml";

        fn row(
            id: &str,
            library: &str,
            status: &str,
            file: &str,
            bytes: &[u8],
            license: &str,
        ) -> serde_json::Value {
            json!({
                "id": id, "library": library, "version": "1.24.4", "os": "windows",
                "arch": "x86_64", "status": status, "linkage": "dynamic-library",
                "artifactFileName": file, "sha256": sha256_hex(bytes),
                "sourceUrl": "https://example.test/source", "licenseSpdx": license,
                "noticeFiles": ["third-party/NOTICE.txt"],
                "verification": { "method": "file-sha256", "evidence": "fixture" }
            })
        }

        /// A catalog for an injected Windows x86_64 host: the runtime row and its DirectML
        /// companion. The runtime row carries the host's runtime file name, so the fixture file
        /// is named like the real one on any test host.
        fn catalog(ort_status: &str, host_proof: bool) -> NativeCatalog {
            let mut ort = row(
                ORT_ID,
                "onnxruntime",
                ort_status,
                runtime_filename(),
                ORT_BYTES,
                "MIT",
            );
            ort["companions"] = json!([DML_ID]);
            if host_proof {
                ort["hostProof"] = json!("never-run");
            }
            let dml = row(
                DML_ID,
                "directml",
                "pinned",
                "DirectML.dll",
                DML_BYTES,
                "LicenseRef-Microsoft-DirectML",
            );
            NativeCatalog::parse_native_only(
                &json!({ "schemaVersion": 1, "libraries": [ort, dml] }).to_string(),
            )
            .unwrap()
        }

        fn catalog_host(fixture: &Fixture, catalog: NativeCatalog) -> FakeHost {
            FakeHost {
                catalog: Some((catalog, "windows", "x86_64")),
                ..fixture.host()
            }
        }

        fn catalog_spec(fixture: &Fixture) -> BindSpec {
            BindSpec {
                verification: BindVerification::Catalog,
                ..fixture.spec()
            }
        }

        fn catalog_report(bound: &BoundRuntime) -> &CatalogReport {
            let VerificationReport::Catalog(report) = &bound.report.verification else {
                panic!("expected the catalog report");
            };
            report
        }

        #[test]
        fn an_approved_runtime_binds_and_an_absent_companion_is_reported() {
            let fixture = Fixture::new();
            let dir = fixture.bundled_dir();
            let runtime = fixture.bare_runtime_in(&dir, ORT_BYTES);
            let mut host = catalog_host(&fixture, catalog("approved", false));
            let bound = bind_with(&catalog_spec(&fixture), &mut host).unwrap();
            let report = catalog_report(&bound);
            assert_eq!(report.library.id(), ORT_ID);
            assert_eq!(report.library.path(), runtime);
            assert_eq!(report.host_proof(), None);
            assert!(report.companions_verified.is_empty());
            assert_eq!(
                report.companions_missing,
                [MissingCompanion {
                    id: DML_ID.into(),
                    file_name: "DirectML.dll".into()
                }]
            );
            assert_eq!(host.runtime_env, Some(runtime));
        }

        #[test]
        fn a_tampered_runtime_is_refused_and_the_variable_is_not_written() {
            let fixture = Fixture::new();
            fixture.bare_runtime_in(&fixture.bundled_dir(), b"tampered onnxruntime");
            let mut host = catalog_host(&fixture, catalog("approved", false));
            assert!(matches!(
                bind_with(&catalog_spec(&fixture), &mut host),
                Err(BindError::VerificationFailed {
                    reason: VerificationFailure::Catalog(NativeCatalogError::DigestMismatch { .. }),
                    ..
                })
            ));
            host.assert_untouched();
            assert!(!host.events.iter().any(|e| e.starts_with("check")));
        }

        #[test]
        fn a_present_companion_must_match_its_row() {
            let fixture = Fixture::new();
            let dir = fixture.bundled_dir();
            fixture.bare_runtime_in(&dir, ORT_BYTES);
            let companion = dir.join("DirectML.dll");
            std::fs::write(&companion, DML_BYTES).unwrap();
            let mut host = catalog_host(&fixture, catalog("approved", false));
            let bound = bind_with(&catalog_spec(&fixture), &mut host).unwrap();
            let report = catalog_report(&bound);
            assert!(report.companions_missing.is_empty());
            let [verified] = report.companions_verified.as_slice() else {
                panic!("expected the one companion");
            };
            assert_eq!(verified.id(), DML_ID);
            assert_eq!(verified.path(), companion.canonicalize().unwrap());

            // The same layout with a tampered DirectML.dll is an error and binds nothing.
            std::fs::write(&companion, b"tampered directml").unwrap();
            let mut host = catalog_host(&fixture, catalog("approved", false));
            assert!(matches!(
                bind_with(&catalog_spec(&fixture), &mut host),
                Err(BindError::VerificationFailed {
                    reason: VerificationFailure::Catalog(NativeCatalogError::DigestMismatch { id, .. }),
                    ..
                }) if id == DML_ID
            ));
            host.assert_untouched();
        }

        #[test]
        fn a_companion_that_is_not_a_regular_file_is_an_error_not_missing() {
            let fixture = Fixture::new();
            let dir = fixture.bundled_dir();
            fixture.bare_runtime_in(&dir, ORT_BYTES);
            std::fs::create_dir_all(dir.join("DirectML.dll")).unwrap();
            let mut host = catalog_host(&fixture, catalog("approved", false));
            assert!(matches!(
                bind_with(&catalog_spec(&fixture), &mut host),
                Err(BindError::VerificationFailed {
                    reason: VerificationFailure::Catalog(NativeCatalogError::UnsafePath { .. }),
                    ..
                })
            ));
            host.assert_untouched();
        }

        #[test]
        fn a_pinned_never_run_row_binds_and_reports_the_missing_host_proof() {
            let fixture = Fixture::new();
            fixture.bare_runtime_in(&fixture.bundled_dir(), ORT_BYTES);
            let mut host = catalog_host(&fixture, catalog("pinned", true));
            let bound = bind_with(&catalog_spec(&fixture), &mut host).unwrap();
            let report = catalog_report(&bound);
            assert_eq!(report.host_proof(), Some(HostProof::NeverRun));
            assert_eq!(
                report.library.status(),
                rightkit_ffmpeg::native::NativeStatus::Pinned
            );
            assert!(host.runtime_env.is_some());
        }

        #[test]
        fn a_planned_row_never_serves() {
            let fixture = Fixture::new();
            fixture.bare_runtime_in(&fixture.bundled_dir(), ORT_BYTES);
            let mut host = catalog_host(&fixture, catalog("planned", true));
            assert!(matches!(
                bind_with(&catalog_spec(&fixture), &mut host),
                Err(BindError::VerificationFailed {
                    reason: VerificationFailure::Catalog(NativeCatalogError::NoApproved { .. }),
                    ..
                })
            ));
            host.assert_untouched();
        }
    }
}

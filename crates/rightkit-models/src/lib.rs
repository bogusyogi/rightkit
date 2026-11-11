//! `rightkit-models` - shared local-model manifest and installer.
//!
//! - [`spec`]: declarative manifest - revisions, quantizations, platform-filtered files.
//! - [`store::Store`]: the manifest-driven installer API over the suite layout (one store since
//!   0.7.0): verify, resolve, uninstall, GC, and migration of the pre-0.7 revisioned layout.
//! - [`install`]: preflight, resumable verified download, extraction, atomic activation,
//!   pause/resume vs cancel, adoption of existing installs ([`Store::adopt`]), progress events.
//! - [`hardware`]: host probe and measured-vs-estimated fit, plus tiered RAM/VRAM verdicts.
//! - [`probe`]: injectable child-process probes ([`probe::ProbeRunner`]) for confined callers.
//! - [`android`]: Android device facts and the injected files directory (unverified on a device).
//!
//! - [`suite`]: the one on-disk layout, shared by HeardRight, ScrapeRight and CutRight and,
//!   through [`Store`], MailRight and GenRight (trusted catalog, download grants, refs, runtime
//!   leases, migration journals, GC).
//!
//! Brand-neutral: callers choose the store root and lock-holder name.

mod alias;
pub mod android;
mod archive_core;
#[cfg(feature = "rightapps")]
pub mod rightapps;
#[cfg(feature = "rightapps")]
pub use rightapps::RuntimeArtifactClient;
pub mod archive;
pub mod disk;
pub mod download;
pub mod error;
pub mod hardware;
pub mod hash;
pub mod http;
pub mod ids;
pub mod install;
mod legacy_layout;
pub mod lock;
pub mod platform;
pub mod probe;
pub mod spec;
pub mod store;
pub mod suite;

pub use android::{
    android_files_dir, android_models_root, init_android_files_dir, AndroidAbi, AndroidFacts,
};
pub use error::{Error, ErrorCode, Result};
pub use hardware::{
    Fit, FitConfidence, FitKind, FitVerdict, Gpu, Hardware, HardwareOverride, Measurement,
    ProfileAlias, RemoteRoute, Requirements, TieredFit, TieredRequirements,
};
pub use ids::{encode_revision_key, ModelId, RevisionKey};
pub use install::{CrashPoint, Event, InstallOptions, InstallOutcome};
pub use platform::Platform;
pub use probe::{detect_inference_runtimes, InferenceRuntime, ProbeFuture, ProbeRunner};
pub use spec::{Extract, FileSpec, ModelSpec, RevisionSpec, MAX_MIRROR_URLS};
pub use store::{
    AliasStatus, AliasSync, FileRecord, PausedInstall, ResolvedRevision, RevisionRecord, State,
    Store, VerifyMode,
};
pub use suite::{EntitlementError, ModelStore};

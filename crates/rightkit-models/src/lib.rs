//! `rightkit-models` - shared local-model manifest and installer.
//!
//! - [`spec`]: declarative manifest - revisions, quantizations, platform-filtered files.
//! - [`store::Store`]: revisioned on-disk layout, verify, rollback, uninstall.
//! - [`install`]: preflight, resumable verified download, extraction, atomic activation, progress events.
//! - [`hardware`]: host probe and measured-vs-estimated fit.
//!
//! Brand-neutral: callers choose the store root and lock-holder name.

pub mod archive;
pub mod disk;
pub mod download;
pub mod error;
pub mod hardware;
pub mod hash;
pub mod install;
pub mod lock;
pub mod platform;
pub mod spec;
pub mod store;

pub use error::{Error, ErrorCode, Result};
pub use hardware::{Fit, FitKind, Gpu, Hardware, Measurement, Requirements};
pub use install::{Event, InstallOptions, InstallOutcome};
pub use platform::Platform;
pub use spec::{Extract, FileSpec, ModelSpec, RevisionSpec};
pub use store::{FileRecord, RevisionRecord, State, Store, VerifyMode};

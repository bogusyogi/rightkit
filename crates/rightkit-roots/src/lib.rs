#![allow(clippy::result_large_err)] // errors carry structured diagnostic context by design, as in rightkit-daemon
//! rightkit-roots: where a Right Suite app keeps its data.
//!
//! * [`mode`] - the one runtime decision about an app's roots: [`mode::decide`],
//!   [`mode::init_or_exit`] and [`mode::roots`] resolve the real per-user roots from the
//!   operating system's account database. There is no test mode. [`mode::AppId`] carries the
//!   layout an app really has: under `Orthic Labs` (the default), flat under the per-user local
//!   folder (`without_org`, for data that predates the organisation folder), legacy folder names
//!   and the dev or QA variant. [`AppRoot::for_app`] turns the resolved folder, in either layout,
//!   into an app root with the same writer lock, containment and cutover.
//! * [`Suite`] - the per-user suite directory (`.../Orthic Labs/shared`) that holds shared state
//!   and `runtime/<service>/service.json`. [`Suite::resolve`] errors when no absolute app-data
//!   root can be determined; it never falls back to the working directory.
//!   [`Suite::per_run_temp`] is the explicit opt-in alternative.
//! * [`AppRoot`] - an app's own folder under the suite, with its writer lock and the one-time
//!   migration of a legacy data folder into it.
//! * [`RootCutover`] - the crash-safe move of an app's data root to a new location, with the app's
//!   lifecycle participants, persisted phases and recovery.
//!
//! These moved unchanged from `rightkit-daemon` 0.7.0, which re-exports them under their old paths, deprecated,
//! (`rightkit_daemon::{mode, Suite, AppRoot, RootCutover, ...}`) for one release. An app that
//! only needs its paths depends on this crate and not on the local-service crate.
//!
//! [`app_dirs`] (`AppDirs`, the per-user data, local, cache, config and home folders; the
//! documented public entry for them) and [`StateDir`] (a descriptor-pinned directory opened at a
//! path the caller already resolved) are re-exported from `rightkit-fs`. [`mode::roots`] uses
//! `app_dirs` for a flat app, so the two cannot disagree about where that app lives. The module's
//! code stays in `rightkit-fs`: this crate depends on `rightkit-fs`, so moving it here would make
//! `rightkit-fs` depend on this crate to re-export it, a cycle. The README lists where the
//! resolvers still overlap.

mod app_root;
pub mod lock;
pub mod mode;
mod root_cutover;
mod suite;

/// The `rightkit-fs` per-user folder resolver, re-exported beside [`Suite`] and [`AppRoot`]. Not
/// the same rules: see the README, "Three root implementations".
pub use rightkit_fs::app_dirs;
/// The `rightkit-fs` descriptor-pinned state directory, re-exported beside [`Suite`] and
/// [`AppRoot`]. See the README, "Three root implementations".
pub use rightkit_fs::StateDir;

// Deprecated items stay exported for existing callers; their deprecation is reported at the
// caller's own use site, not on this re-export.
pub use app_root::{
    valid_app_name, AppRoot, AppRootError, LinkNotCreated, MigrateOptions, MigratedLink,
    MigrationOutcome, MigrationReport, MigrationScope, MigrationState, MigrationStatus,
    OmittedLink, ReparseKind, SkippedReparsePoint, SkippedSpecial, SpecialKind, WriterLock,
    MAX_APP_NAME_LEN, MIGRATION_MARKER, MIGRATION_STATE,
};
pub use root_cutover::{
    CutoverContext, CutoverError, CutoverParticipant, CutoverPhase, CutoverReport, CutoverStep,
    FsMove, ParticipantError, RecoverOutcome, RetireMode, Retired, RootCutover, RootSelection,
    RootStatus, Settlement, CUTOVER_RECORD,
};
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub use suite::passwd_home_dir;
pub use suite::{
    make_private_dir, Suite, SuiteError, ENV_SUITE_ROOT, SHARED_DIR_NAME, SUITE_DIR_NAME,
};

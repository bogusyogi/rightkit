//! Shared sandbox + PTY + process-tree + resource authority for Right Suite apps.
//!
//! * [`Sandbox::spawn`] confines a child to a [`SandboxPolicy`] (macOS Seatbelt,
//!   Linux Landlock/seccomp, Windows AppContainer inside a kill-on-close Job Object).
//! * [`Sandbox::spawn_pty`] (feature `pty`, default) gives a PTY child with lease-epoch input fencing ([`PtySession`]).
//! * [`process_tree`] enumerates and kills whole process trees (bounded).
//! * [`ResourceAuthority`] reserves capacity before any process exists; `Strict` denies.
//! * [`classify_denial`] turns raw error text into a cross-platform [`DenialSignature`].
//!
//! Windows APIs:
//! * `AppContainerAncestorLease::acquire(path, appcontainer_sid)` leases physical ancestor traverse access until drop.
//! * `AppContainerAncestorLease::acquire_with(path, sid, AncestorLeasePolicy::BestEffort)`
//!   reports denied ancestors via `unleasable()`; skipped traversal relies on bypass-traverse
//!   privilege & upward listing may need `GIT_CEILING_DIRECTORIES`.
//! * `sweep_stale_appcontainer_ancestor_aces_for_owner(path, owner_prefix)` sweeps stale
//!   ancestor ACEs for derivation-verified owner profiles, preserving mutex-backed live
//!   leases within the same logon session (`Local\`).

mod denial;
mod error;
mod policy;
pub mod process_tree;
#[cfg(feature = "pty")]
mod pty;
mod resources;
mod spawn;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
pub mod seatbelt;
#[cfg(windows)]
mod windows_appcontainer;
#[cfg(windows)]
mod windows_job;

pub use denial::{classify_denial, DenialCategory, DenialSignature, DenialSource};
pub use error::SandboxError;
pub use policy::{EnvPolicy, NetworkPolicy, SandboxCommand, SandboxPolicy};
#[cfg(feature = "pty")]
pub use pty::{PtyError, PtyLeaseSnapshot, PtySession, PtySize, UnacknowledgedInputDecision};
pub use resources::{
    Reservation, ResourceAuthority, ResourceDenied, ResourceKind, ResourceProfile,
};
pub use spawn::{Sandbox, SandboxOutput, SandboxedChild, DEFAULT_OUTPUT_CAP};
#[cfg(windows)]
pub use windows_appcontainer::{
    appcontainer_profile_name_for_test, delete_appcontainer_profile_for_test,
    remove_unused_appcontainer_profiles, sweep_stale_appcontainer_ancestor_aces,
    sweep_stale_appcontainer_ancestor_aces_for_owner, AncestorLeasePolicy,
    AppContainerAncestorLease, ProfileKept, ProfileSweepReport, UnleasableAncestor,
};

//! Shared sandbox + PTY + process-tree + resource authority for Right Suite apps.
//!
//! * [`Sandbox::spawn`] confines a child to a [`SandboxPolicy`] (macOS Seatbelt,
//!   Linux Landlock/seccomp, Windows AppContainer inside a kill-on-close Job Object).
//! * [`confine_command`] applies the same confinement to a command the caller owns (Linux and
//!   macOS), with [`is_landlock_available`] and [`landlock_abi_version`] to probe the host.
//! * [`Sandbox::spawn_pty`] (feature `pty`, default) gives a PTY child with lease-epoch input fencing ([`PtySession`]).
//! * [`process_tree`] enumerates and kills whole process trees (bounded).
//! * [`ResourceAuthority`] reserves capacity before any process exists; `Strict` denies.
//! * [`classify_denial`] turns raw error text into a cross-platform [`DenialSignature`];
//!   [`report_denial`] builds a [`DenialReport`] (denied path, protected-secret flag, the grant
//!   to request) from it plus the command text.
//! * [`SandboxPolicy::deny_read()`] and [`secret_paths_within`] keep secrets out of reach inside
//!   a wide grant (see [`deny_read_enforced`] for where it is enforced).
//! * [`Sandbox::spawn_piped`] is [`Sandbox::spawn`] with a stdin pipe
//!   ([`SandboxedChild::take_stdin`]); [`Sandbox::pin_read_roots`] shares interpreter install
//!   directories as read/execute roots through a [`PinHandle`].
//!
//! Windows APIs:
//! * `AppContainerProfile::for_workspace(root)` is a reusable container with reference-counted
//!   allow leases and an optional loopback exemption (any `deny_read` entry is refused, see
//!   [`deny_read_enforced`]), spawned into with
//!   `Sandbox::spawn_in_profile`; `resolve_program` finds the image a launch will use.
//! * `AppContainerAncestorLease::acquire(path, appcontainer_sid)` leases physical ancestor traverse access until drop.
//! * `AppContainerAncestorLease::acquire_with(path, sid, AncestorLeasePolicy::BestEffort)`
//!   reports denied ancestors via `unleasable()`; skipped traversal relies on bypass-traverse
//!   privilege & upward listing may need `GIT_CEILING_DIRECTORIES`.
//! * `sweep_stale_appcontainer_ancestor_aces_for_owner(path, owner_prefix)` sweeps stale
//!   ancestor ACEs for derivation-verified owner profiles, preserving mutex-backed live
//!   leases of any user in any logon session (`Global\`).

mod confine;
mod denial;
mod denial_report;
mod deny_read;
mod error;
mod pins;
mod policy;
pub mod process_tree;
#[cfg(feature = "pty")]
mod pty;
mod resources;
mod seccomp;
mod spawn;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
pub mod seatbelt;
#[cfg(windows)]
mod windows_appcontainer;
#[cfg(windows)]
mod windows_job;

pub use confine::{
    confine_command, deny_read_enforced, is_landlock_available, landlock_abi_version,
};
pub use denial::{classify_denial, DenialCategory, DenialSignature, DenialSource};
pub use denial_report::{
    looks_like_blocked_network, report_denial, ConfinementInfo, DenialReport, Grant,
};
pub use deny_read::{
    glob_matches, path_is_protected_secret, path_is_within, plan_deny_read_paths,
    secret_paths_within, secret_roots, secret_roots_under, user_profile_root,
    USERPROFILE_SECRET_FOLDERS,
};
pub use error::SandboxError;
pub use pins::{default_user_dirs, is_interpreter_dir, read_root_is_too_broad, PinHandle};
pub use policy::{
    ConfinementMode, DenyReadEntry, EnvPolicy, LandlockMode, LoopbackPolicy, NetworkPolicy,
    SandboxCommand, SandboxPolicy, SeccompMode,
};
#[cfg(feature = "pty")]
pub use pty::{PtyError, PtyLeaseSnapshot, PtySession, PtySize, UnacknowledgedInputDecision};
pub use resources::{
    Reservation, ResourceAuthority, ResourceDenied, ResourceKind, ResourceProfile,
};
pub use spawn::{Sandbox, SandboxOutput, SandboxedChild, DEFAULT_OUTPUT_CAP};
#[cfg(windows)]
pub use spawn::{WindowsAuxiliaryIo, WindowsPipeSpawn};
#[cfg(windows)]
pub use windows_appcontainer::{
    appcontainer_profile_name_for_test, delete_appcontainer_profile_for_test,
    remove_unused_appcontainer_profiles, resolve_program_str as resolve_program,
    sweep_stale_appcontainer_ancestor_aces, sweep_stale_appcontainer_ancestor_aces_for_owner,
    AncestorLeasePolicy, AppContainerAncestorLease, AppContainerProfile, ProfileKept,
    ProfileSweepReport, UnleasableAncestor, WithGrantError,
};

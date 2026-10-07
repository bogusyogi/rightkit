//! Shared sandbox + PTY + process-tree + resource authority for Right Suite apps.
//!
//! * [`Sandbox::spawn`] confines a child to a [`SandboxPolicy`] (macOS Seatbelt,
//!   Linux Landlock/seccomp, Windows AppContainer inside a kill-on-close Job Object).
//! * [`Sandbox::spawn_pty`] gives a PTY child with lease-epoch input fencing ([`PtySession`]).
//! * [`process_tree`] enumerates and kills whole process trees (bounded).
//! * [`ResourceAuthority`] reserves capacity before any process exists; `Strict` denies.
//! * [`classify_denial`] turns raw error text into a cross-platform [`DenialSignature`].

mod denial;
mod error;
mod policy;
pub mod process_tree;
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
pub use pty::{PtyError, PtyLeaseSnapshot, PtySession, PtySize, UnacknowledgedInputDecision};
pub use resources::{
    Reservation, ResourceAuthority, ResourceDenied, ResourceKind, ResourceProfile,
};
pub use spawn::{Sandbox, SandboxOutput, SandboxedChild};

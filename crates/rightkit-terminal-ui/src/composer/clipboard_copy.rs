//! Clipboard copy seam for textarea mouse selections.
//!
//! The Codex donor routes copy through a background worker with OSC52/native
//! clipboard leases; that worker machinery is parent-owned. This module keeps
//! the donor's `CopyStatus`/`worker::CopyResult` result types verbatim so the
//! lifted `textarea::mouse` code compiles unchanged, and gives the composer a
//! minimal `copy` entry point that emits an OSC52 write intent upstream via
//! [`crate::composer::Intent::Copy`].

/// Whether the terminal confirmed a copy request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CopyStatus {
    Confirmed,
    Unconfirmed,
    Pending(u64),
    Busy,
}

pub(crate) mod worker {
    use super::CopyStatus;

    /// Result of one clipboard write, keyed by the request id the composer
    /// issued when the mouse selection ended.
    pub(crate) type CopyResult = Result<CopyStatus, String>;
}

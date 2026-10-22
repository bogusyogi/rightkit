//! Descriptor sanitation for children built outside [`OwnedCommand`](crate::OwnedCommand)
//! (Orthic-Labs/rightkit#835). Unix only; Windows has no equivalent for a std or tokio
//! `Command` (see the README, "Inherited descriptors and handles").

use crate::process_unix::Inheritance;
use std::os::fd::RawFd;

/// Keeps a child from inheriting the parent's open descriptors.
///
/// `sanitize_descriptors` adds a `pre_exec` hook that, after the command's own stdio wiring
/// (std dups stdio before it runs any hook), marks every descriptor above stderr close-on-exec
/// so none survives `exec`, except those in `keep`, whose close-on-exec flag is cleared. A
/// descriptor in `keep` that is not open makes the spawn fail with `EBADF`.
///
/// The hook is async-signal-safe by construction: `fcntl` and `close_range(2)` only, no
/// allocation, no lock (see `Inheritance::apply`). Everything it needs is computed here, in
/// the parent. A hook runs after the ones registered before it, so a descriptor a caller's
/// own earlier `pre_exec` opens is swept too; register the caller's hooks after this call to
/// keep their descriptors. Calling it again with a larger `keep` list restores those
/// descriptors, because the hook only ever marks and never closes.
///
/// [`command_std`](crate::command_std) and `tokio_support::command` call it with an empty list.
pub trait SanitizeDescriptors {
    /// Registers the sanitizing hook. Returns `self` for chaining.
    fn sanitize_descriptors(&mut self, keep: &[RawFd]) -> &mut Self;

    /// Re-lists the parent's open descriptors and publishes the sweep bound. Call it
    /// immediately before `spawn()`, `output()` or `status()` on a command registered earlier.
    ///
    /// Where the kernel has no one-call sweep (macOS and other non-Linux Unix; Linux uses
    /// `close_range`, which has no bound) the child marks descriptors only up to the highest one
    /// open in the parent plus a margin of 256, never up to the descriptor limit (that cost
    /// hundreds of thousands of `fcntl` calls per spawn). `sanitize_descriptors` takes that
    /// snapshot when it is called; a bare `spawn()` long after registration uses the larger of
    /// the registration-time bound and the one most recently published by any registration or
    /// refresh in the process, so a descriptor opened since, above both, is not swept. This
    /// refresh closes that window up to the instant of the call; the remaining race (a
    /// descriptor another thread opens between the listing and the fork) is not covered.
    fn refresh_sanitized(&mut self) -> &mut Self;
}

impl SanitizeDescriptors for std::process::Command {
    fn sanitize_descriptors(&mut self, keep: &[RawFd]) -> &mut Self {
        use std::os::unix::process::CommandExt;
        let inheritance = Inheritance::unchecked(keep.to_vec());
        // SAFETY: the hook runs only `Inheritance::apply`, which is async-signal-safe (see its
        // documentation): fcntl and close_range, no allocation, no locks.
        unsafe {
            self.pre_exec(move || inheritance.apply());
        }
        self
    }

    fn refresh_sanitized(&mut self) -> &mut Self {
        crate::process_unix::refresh_shared_bound();
        self
    }
}

#[cfg(feature = "tokio")]
impl SanitizeDescriptors for tokio::process::Command {
    fn sanitize_descriptors(&mut self, keep: &[RawFd]) -> &mut Self {
        let inheritance = Inheritance::unchecked(keep.to_vec());
        // SAFETY: as for `std::process::Command` above.
        unsafe {
            self.pre_exec(move || inheritance.apply());
        }
        self
    }

    fn refresh_sanitized(&mut self) -> &mut Self {
        crate::process_unix::refresh_shared_bound();
        self
    }
}

//! Identity proof and exit observation for a child that this crate does not own, such as a PTY
//! child spawned by another library. It is the same rule as [`OwnedChild`](crate::OwnedChild)
//! applies to its own leader (see `identity`): record who the process is at spawn, prove it
//! before every signal or reap that names its pid, and learn of its exit from the kernel instead
//! of a timer.

use crate::{
    exit_watch::ExitWatch,
    identity::{Origin, Proof},
};
use std::{io, time::Duration};

/// What this process knows about one direct child, taken right after it was spawned.
///
/// The child must be a direct child of this process that nobody has reaped yet when
/// [`capture`](Self::capture) runs. Later, [`is_same`](Self::is_same) says whether its pid still
/// names that process, and [`wait_exit`](Self::wait_exit) sleeps until the kernel reports its
/// exit (a kqueue knote on macOS, a pidfd on Linux), without reaping it.
///
/// There is no lock here to hold between a proof and the action it guards, so
/// [`while_same`](Self::while_same) leaves the residual described in the `identity` module:
/// other code that reaps the child inside that gap, and a replacement child that takes its pid
/// there. On Linux with pidfd support the proof is an exact liveness probe of the original
/// process; on macOS it compares the kernel's parent and start time.
#[derive(Debug)]
pub struct PidProof {
    pid: u32,
    origin: Origin,
    watch: ExitWatch,
}

impl PidProof {
    /// Records the identity of `pid` and opens its exit watch. Fails when this host can describe
    /// processes but would not describe this one: nothing could later be proven about it.
    pub fn capture(pid: u32) -> io::Result<Self> {
        let watch = ExitWatch::open(pid, true);
        let origin = if watch.proves_exactly() {
            Origin::Unavailable
        } else {
            Origin::capture(pid)
        };
        if matches!(origin, Origin::Missing) {
            return Err(io::Error::other(
                "the child could not be identified; a later signal could not be proven to reach it",
            ));
        }
        Ok(Self { pid, origin, watch })
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// True while the pid still names the process captured at spawn (running, or exited and
    /// unreaped); false once it was reaped or the number changed hands. `Err` when the kernel
    /// cannot be asked: do nothing to the pid then.
    pub fn is_same(&self) -> io::Result<bool> {
        if let Some(alive) = self.watch.exact_proof() {
            return alive;
        }
        Ok(self.origin.check(self.pid)? == Proof::Same)
    }

    /// Runs `act` with the pid if it is proven to still name the captured process; `Ok(None)`
    /// otherwise, without calling `act`.
    pub fn while_same<R>(&self, act: impl FnOnce(u32) -> R) -> io::Result<Option<R>> {
        Ok(if self.is_same()? {
            Some(act(self.pid))
        } else {
            None
        })
    }

    /// Sleeps until the process has exited (`true`) or `timeout` has passed (`false`); `None`
    /// waits without limit. Holds no lock and reaps nothing. `Err(Unsupported)` on a host with
    /// no event source.
    pub fn wait_exit(&self, timeout: Option<Duration>) -> io::Result<bool> {
        self.watch.wait(timeout)
    }
}

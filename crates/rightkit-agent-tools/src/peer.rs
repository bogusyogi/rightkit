//! The verified executable of the process that called a service method.
//!
//! A host that shows a consent prompt ("this program wants to drive CutRight") needs the
//! caller's executable, not the label the caller declared in `hello`. `rightkit-daemon`
//! (`CallContext::verify_peer_executable`) answers "is the caller this program?" but not "which
//! program is it?". [`CallContextExt::peer_executable`] answers the second question from the same
//! OS check, so no app has to probe with an impossible path and read the error.

use rightkit_daemon::{CallContext, HostExpectation, HostIdentityReason};
use std::path::PathBuf;

/// A path no file can have (a NUL byte is not valid in a file name on any supported platform), so
/// an expectation naming it never matches and the identity check always ends in a mismatch that
/// reports what the OS found.
const NO_SUCH_EXECUTABLE: &str = "\0rightkit-peer-executable-probe";

/// Adds [`peer_executable`](CallContextExt::peer_executable) to the daemon's [`CallContext`].
pub trait CallContextExt {
    /// The executable of the process on the other end of this connection, as the OS reports it
    /// and the transport verified it: the caller is a local app, its pid, uid or SID, and start
    /// time are the ones recorded at accept (so a reused pid is refused), and the connection is
    /// still open. Canonicalised when the file still exists.
    ///
    /// `None` fails closed: a device caller (no OS peer), a peer whose pid or executable cannot
    /// be established, a process that exited or changed while it was identified, a platform that
    /// cannot read a process's executable. Never a guess.
    ///
    /// This is a fact for display and audit (a consent prompt, a log line). It is not an
    /// authorisation: to gate a method on a program use
    /// `CallContext::verify_peer_executable`, which compares against the program you expect.
    /// Run it per call; do not cache the answer past the connection.
    fn peer_executable(&self) -> Option<PathBuf>;
}

impl CallContextExt for CallContext {
    fn peer_executable(&self) -> Option<PathBuf> {
        let probe = HostExpectation::executable(NO_SUCH_EXECUTABLE);
        match self.verify_peer_executable(&probe) {
            // Cannot happen (nothing matches the probe); were it ever so, the process is still
            // the verified one.
            Ok(host) => Some(host.executable),
            // Only a mismatch means every identity check passed and the executable was read.
            Err(e) if e.reason == HostIdentityReason::ExecutableMismatch => e.found_executable,
            Err(_) => None,
        }
    }
}

use super::caller::Caller;
use crate::error::ServiceError;
use crate::host_identity::{
    verify_peer_process, HostExpectation, HostIdentityError, HostIdentityReason, HostProcess,
};
use crate::peer::PeerIdentity;
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Sends an `event` frame for the request being handled.
pub type Emit = Arc<dyn Fn(Value) + Send + Sync>;

/// What a handler knows about the request it is serving.
#[derive(Clone)]
pub struct CallContext {
    conn: u64,
    caller: Caller,
    pub(crate) closed: Arc<AtomicBool>,
    handoff: Arc<AtomicBool>,
}

pub(crate) fn new_context(
    conn: u64,
    caller: Caller,
    closed: Arc<AtomicBool>,
    handoff: Arc<AtomicBool>,
) -> CallContext {
    CallContext {
        conn,
        caller,
        closed,
        handoff,
    }
}

impl CallContext {
    pub fn conn_id(&self) -> u64 {
        self.conn
    }
    /// Who is calling: a local app or a device.
    pub fn origin(&self) -> &Caller {
        &self.caller
    }
    /// The role of a device caller, from the scopes its enrolment granted (see
    /// [`ROLE_SCOPE_PREFIX`](crate::contract::ROLE_SCOPE_PREFIX)). `None` for a local app.
    pub fn role(&self) -> Option<&str> {
        self.caller.role()
    }
    /// The app label the client declared in `hello` (already allowlisted); for a device caller,
    /// the device id.
    ///
    /// The label is a claim: any process of the current user can declare any allowlisted
    /// label. Use it to choose what a caller may ask for, never as proof of who is asking.
    /// [`peer`](Self::peer) is what the OS verified.
    pub fn app(&self) -> &str {
        self.caller.label()
    }
    /// Who the OS says is on the other end of this connection, from the peer check that ran
    /// before any byte was read: the uid on Unix, the user SID on Windows, and the process id
    /// where the platform reports it (Linux, macOS and Windows). `None` for a caller that is not
    /// a local app (a device has no OS peer).
    ///
    /// This is the fact, as opposed to the [`app`](Self::app) label. The user is always the
    /// current user, since any other peer was refused. A pid identifies the process only while
    /// it runs: record it for an audit trail, do not authorise by it later.
    pub fn peer(&self) -> Option<&PeerIdentity> {
        self.caller.peer()
    }
    /// Does the process on the other end of this connection run the program `expect` names?
    /// The server-side counterpart of [`Client::connect_expecting`](crate::Client::connect_expecting):
    /// the same [`HostExpectation`] (executable paths compared after canonicalisation, same file
    /// by identity, then the optional [`HostExpectation::with_verifier`] check), applied to
    /// [`peer`](Self::peer), so an app gates an owner-only method with RightKit code instead of
    /// its own process inspection:
    ///
    /// ```ignore
    /// let owner = HostExpectation::executable(app_exe);
    /// if cx.verify_peer_executable(&owner).is_err() {
    ///     return Err(ServiceError::new(ERROR_FORBIDDEN, "owner only"));
    /// }
    /// ```
    ///
    /// Fails closed: a device caller (no OS peer), a peer whose pid, uid or executable cannot be
    /// established, a process that exited or changed while it was identified, an expectation
    /// naming no executable, a platform that cannot read a process's executable, a connection
    /// that has closed, and a refusing verifier are all `Err`. Never `Ok` on a guess.
    ///
    /// Code signing: this crate links no code-signing API. On macOS the signing requirement
    /// (`SecRequirementCreateWithString` against the process) and on Windows the Authenticode
    /// subject (`WinVerifyTrust` on [`HostProcess::executable`]) go in
    /// [`HostExpectation::with_verifier`], which runs after the executable matched. On macOS
    /// [`HostProcess::audit_token`] is the token the kernel captured when the client connected,
    /// the race-free handle for `SecCodeCopyGuestWithAttributes`.
    ///
    /// The process is pinned by pid plus its start time (Windows: an open process handle), and
    /// the connection must still be open when the check ends, so a peer that exited and whose
    /// pid was reused is refused. The window is the peer-gone watcher's lag, about 40 ms.
    /// Run it per call; do not cache the answer past the connection.
    pub fn verify_peer_executable(
        &self,
        expect: &HostExpectation,
    ) -> Result<HostProcess, HostIdentityError> {
        let host = verify_peer_process(self.peer(), expect)?;
        if self.peer_closed() {
            return Err(HostIdentityError {
                expected: expect.executables().to_vec(),
                found_pid: Some(host.pid),
                found_executable: Some(host.executable),
                reason: HostIdentityReason::HostChanged(
                    "the connection closed while the peer was identified".to_string(),
                ),
            });
        }
        Ok(host)
    }
    /// The client went away while this request was running.
    pub fn peer_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
    /// A higher-ranked host asked for the service: return
    /// [`ServiceError::handoff`] at the next safe boundary.
    pub fn handoff_requested(&self) -> bool {
        self.handoff.load(Ordering::SeqCst)
    }
}

pub trait Handler: Send + Sync + 'static {
    /// Serve one allowlisted request. Events go to `emit`; a returned
    /// retryable error is sent to the client as-is.
    fn call(
        &self,
        cx: &CallContext,
        method: &str,
        params: &Value,
        emit: &Emit,
    ) -> Result<Value, ServiceError>;
    /// A takeover was accepted: stop starting new steps, finish at a boundary.
    fn on_handoff(&self) {}
    /// A connection closed (also fired mid-request when the peer drops).
    fn connection_closed(&self, _conn: u64) {}
}

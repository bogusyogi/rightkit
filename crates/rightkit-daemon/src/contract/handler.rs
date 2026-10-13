use super::caller::Caller;
use crate::error::ServiceError;
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

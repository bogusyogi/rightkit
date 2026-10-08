//! Remote worker transport (RK8): the trait the controller calls, plus its
//! plain data and error types.
//!
//! [`WorkerTransport`] is implemented by `controller::ControllerLink`, which
//! dials a worker, authenticates both ends with the enrolled device key, and
//! carries commands over a sequenced MAC channel. Workers are served by
//! `worker::WorkerEndpoint`. Secrets are never carried in these types: an
//! enrolment grant is named by id, and a device key stays inside the transport
//! on each side.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Request to enrol a GUI worker beside its target app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnrolmentRequest {
    pub worker_label: String,
    /// Id of a pre-issued enrolment grant. The one-time code stays with the
    /// operator and the controller holds only its derived key.
    pub enrolment_id: String,
}

/// Identity of an enrolled worker, bound to its device key fingerprint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerIdentity {
    pub worker_id: String,
    pub key_fingerprint: String,
}

/// A capability a worker reports during discovery.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capability {
    pub name: String,
    pub version: u32,
}

/// A content-addressed bundle to transfer to a worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleRef {
    pub bundle_id: String,
    /// Lowercase hex BLAKE3 digest of the whole bundle.
    pub blake3: String,
    pub bytes: u64,
}

/// Outcome of a bundle transfer. `resumed_from` is the byte offset the worker
/// already held when this attempt began; zero means a fresh transfer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferReport {
    pub resumed_from: u64,
    pub bytes: u64,
}

/// Position a reconnecting controller resumes from: the number of commands it
/// has seen acknowledged in the session. A worker that reports fewer has lost
/// acknowledged work, which is an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReconnectState {
    pub last_acked_seq: u64,
}

/// Errors from the worker wire. Variants never carry key material.
#[derive(Debug)]
pub enum TransportError {
    Io(std::io::Error),
    /// A MAC, enrolment proof, or session check failed.
    Auth(String),
    /// A replayed, reordered, or malformed frame, or an unexpected reply.
    Protocol(String),
    /// The worker refused the request.
    Refused(String),
    /// The request carried a stale or unknown fencing token.
    Fenced {
        current: u64,
    },
    /// The link has no live connection. Reconnect first.
    NotConnected,
    /// The request names a worker other than the one this link is attached to.
    WrongWorker {
        expected: String,
        found: String,
    },
    /// Bundle bytes do not match the declared length or digest.
    Digest(String),
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TransportError::Io(e) => write!(f, "worker link I/O: {e}"),
            TransportError::Auth(r) => write!(f, "worker authentication failed: {r}"),
            TransportError::Protocol(r) => write!(f, "worker protocol error: {r}"),
            TransportError::Refused(r) => write!(f, "worker refused: {r}"),
            TransportError::Fenced { current } => {
                write!(f, "request fenced (current fence {current})")
            }
            TransportError::NotConnected => write!(f, "worker link is not connected"),
            TransportError::WrongWorker { expected, found } => {
                write!(f, "link is attached to {expected}, not {found}")
            }
            TransportError::Digest(r) => write!(f, "bundle rejected: {r}"),
        }
    }
}

impl std::error::Error for TransportError {}

impl From<std::io::Error> for TransportError {
    fn from(e: std::io::Error) -> Self {
        TransportError::Io(e)
    }
}

/// Transport for remote workers, called from the controller.
pub trait WorkerTransport {
    type Error: std::error::Error;

    /// Enrols a worker with its one-time grant and returns its identity. The
    /// device key is issued once and kept by both sides.
    fn enrol(&mut self, request: &EnrolmentRequest) -> Result<WorkerIdentity, Self::Error>;

    fn discover(&mut self, worker: &WorkerIdentity) -> Result<Vec<Capability>, Self::Error>;

    /// Sends `data` to the worker in resumable chunks. Calling again after an
    /// interruption resumes from the offset the worker already holds. The
    /// worker verifies the whole digest before it keeps the bundle.
    fn transfer_bundle(
        &mut self,
        worker: &WorkerIdentity,
        bundle: &BundleRef,
        data: &[u8],
    ) -> Result<TransferReport, Self::Error>;

    /// Re-establishes a dropped session and returns the acknowledged position.
    /// The session id must match the one the controller holds, so a worker
    /// cannot attach to a session it does not own.
    fn reconnect(
        &mut self,
        worker: &WorkerIdentity,
        state: ReconnectState,
    ) -> Result<ReconnectState, Self::Error>;
}

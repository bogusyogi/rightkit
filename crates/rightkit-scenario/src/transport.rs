//! Remote worker transport boundary (RK8).
//!
//! Traits and plain data only. Authenticated enrolment, capability discovery,
//! bundle transfer and reconnect are implemented by a later slice over the
//! shared transport. Nothing in this module opens a socket. Secrets are never
//! carried in these types; enrolment identifies a token by id only.

/// Request to enrol a GUI worker beside its target app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnrolmentRequest {
    pub worker_label: String,
    /// Id of a pre-issued enrolment grant. The secret stays with the transport.
    pub enrolment_id: String,
}

/// Identity of an enrolled worker, bound to its key fingerprint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerIdentity {
    pub worker_id: String,
    pub key_fingerprint: String,
}

/// A capability a worker reports during discovery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capability {
    pub name: String,
    pub version: u32,
}

/// A content-addressed bundle to transfer to a worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleRef {
    pub bundle_id: String,
    pub blake3: String,
    pub bytes: u64,
}

/// Position a reconnecting worker resumes from. It is the last journal `seq`
/// the coordinator has acknowledged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReconnectState {
    pub last_acked_seq: u64,
}

/// Transport for remote workers. Implementations are out of scope for RK8's
/// first slice.
pub trait WorkerTransport {
    type Error: std::error::Error;

    fn enrol(&mut self, request: &EnrolmentRequest) -> Result<WorkerIdentity, Self::Error>;

    fn discover(&mut self, worker: &WorkerIdentity) -> Result<Vec<Capability>, Self::Error>;

    fn transfer_bundle(
        &mut self,
        worker: &WorkerIdentity,
        bundle: &BundleRef,
    ) -> Result<(), Self::Error>;

    /// Re-establishes a dropped session and returns the sequence to resume from.
    fn reconnect(
        &mut self,
        worker: &WorkerIdentity,
        state: ReconnectState,
    ) -> Result<ReconnectState, Self::Error>;
}

//! Messages carried over the worker wire. Handshake messages are plain JSON
//! frames whose MAC fields authenticate them. Command and reply messages travel
//! inside a sealed `wire::Channel`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::driver::StepSpec;
use crate::transport::Capability;

/// Enrolment and session establishment, in the order each one runs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Handshake {
    /// Controller to worker: start an enrolment for `enrolment_id`.
    EnrolChallenge {
        enrolment_id: String,
        challenge: String,
    },
    /// Worker to controller: proves the one-time code.
    EnrolProof { worker_label: String, proof: String },
    /// Controller to worker: the device key, wrapped under the code.
    EnrolGrant {
        worker_id: String,
        wrapped: String,
        mac: String,
    },
    /// Controller to worker: opens a session with a fresh nonce.
    Challenge { controller_nonce: String },
    /// Worker to controller: identity, session claim, applied count and proof.
    Identify {
        worker_id: String,
        session_claim: Option<String>,
        applied: u64,
        worker_nonce: String,
        mac: String,
    },
    /// Controller to worker: the session id and the controller's proof.
    Accept { session_id: String, mac: String },
    /// Either side: the handshake failed.
    Reject { reason: String },
}

/// Controller to worker, inside the sealed channel. `key` is the idempotency
/// key of a step's run, and `fence` must match the worker's current grant.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Command {
    Grant {
        step: String,
        take: u32,
        fence: u64,
    },
    Discover,
    Perform {
        key: String,
        step: String,
        take: u32,
        fence: u64,
        spec: StepSpec,
    },
    Observe {
        key: String,
        step: String,
        take: u32,
        fence: u64,
        spec: StepSpec,
    },
    Cancel {
        key: String,
        step: String,
        take: u32,
        fence: u64,
    },
    BundleBegin {
        bundle_id: String,
        blake3: String,
        bytes: u64,
    },
    BundleChunk {
        bundle_id: String,
        /// Digest declared at `BundleBegin`. A chunk for a different bundle
        /// under the same id is refused, so resume cannot splice bytes.
        blake3: String,
        offset: u64,
        data: String,
    },
    BundleFinish {
        bundle_id: String,
        blake3: String,
    },
}

/// Worker to controller, one per command.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Reply {
    Granted,
    Capabilities { items: Vec<Capability> },
    Performed,
    PerformRefused { reason: String },
    PerformErrored { reason: String },
    Observed { outcome: WireObservation },
    Cancelled,
    Fenced { current: u64 },
    Error { reason: String },
    BundleStatus { received: u64 },
    BundleVerified,
    BundleRejected { reason: String },
}

/// The driver's observation, as it crosses the wire.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum WireObservation {
    Satisfied { value: Value },
    Pending,
    Failed { reason: String },
    Error { reason: String },
}

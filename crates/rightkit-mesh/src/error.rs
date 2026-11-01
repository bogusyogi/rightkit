//! One error type for the mesh. None of its variants carry key material.

use crate::caps::Refusal;

#[derive(Debug, thiserror::Error)]
pub enum MeshError {
    /// The device mesh switch is off (the default until the mesh is verified).
    #[error("the device mesh is switched off")]
    Disabled,
    /// "Connect through the Orthic relay" is on but no relay address is configured. There is no
    /// built-in default relay: a build never reaches a relay nobody configured.
    #[error("no relay address is configured")]
    NoRelay,
    #[error("bad address: {0}")]
    Address(String),
    #[error("endpoint: {0}")]
    Endpoint(String),
    #[error("connect: {0}")]
    Connect(String),
    #[error("stream: {0}")]
    Stream(String),
    /// The caller stopped the operation (`send_bulk_until`'s cancel flag).
    #[error("cancelled")]
    Cancelled,
    /// The other end is not an active paired device.
    #[error("not an active paired device")]
    NotPaired,
    /// A relay cap, the monthly budget or a direct-only rule refused the transfer.
    #[error("{0}")]
    Refused(Refusal),
    #[error("malformed message: {0}")]
    Malformed(&'static str),
    #[error("crypto: {0}")]
    Crypto(&'static str),
    /// The mailbox server refused a request; `code` is its stable machine word.
    #[error("mailbox refused ({code}): {message}")]
    Mailbox { code: String, message: String },
    #[error("store: {0}")]
    Store(String),
    /// No mailbox key has been exchanged with this device yet; the first live connection does it.
    #[error("no mailbox key has been exchanged with this device yet")]
    NoKeys,
}

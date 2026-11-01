//! The device link: the qualified outbound link between paired devices. Transport-agnostic protocol
//! core only.
//!
//! This module has no socket, no relay and no IPC command. It holds the pairing protocol, the
//! paired-device registry, the device circle, revocation, resume tickets and the per-request
//! capability check. A transport carries the messages defined here as JSON; `transport` has the
//! one that runs the pairing over a stream.
//!
//! Modules
//! * `identity`: Ed25519 device identity, seed held in the OS credential store.
//! * `pairing`: short-code pairing (commit-then-reveal authentication string, signed
//!   confirmations, then the host owner's explicit approval).
//! * `join_token`: the one-line join token, a single-use secret the host keeps only a hash of,
//!   proven inside the pairing transcript instead of a typed code.
//! * `circle`: the device circle: a signed member list for a new device, signed introductions and
//!   removals for the other members ("added once, trusted everywhere").
//! * `registry`: paired-device records (re-exported from `rightkit-device-link-types`) and their
//!   file.
//! * `link`: the service: registry mutations, revocation, resume tickets, per-request check,
//!   and the [`ReplayStore`] seam that makes request redelivery idempotent.
//! * `policy`: whether a paired computer may use chat here, and by which rule.
//! * `relay_admit`: relay admission for a guest with no direct path: bootstrap key, key proof,
//!   admission file.
//! * `wire`: canonical byte encoding, hex and randomness helpers.
//!
//! Owner approvals (`rightkit-approvals`) and the record types (`rightkit-device-link-types`) are
//! separate crates.

pub mod circle;
pub mod identity;
pub mod join_token;
pub mod link;
pub mod pairing;
pub mod policy;
pub mod registry;
pub mod relay_admit;
pub mod transport;
pub mod wire;

#[cfg(test)]
pub use identity::MemoryKeyVault;
pub use identity::{
    DEFAULT_VAULT_SERVICE, DeviceIdentity, KeyVault, OsKeyVault, device_id_for_key, open_vault,
};
pub use link::{
    AuthorizedRequest, DeviceLink, LinkRequest, ReplayDisposition, ReplayEntry, ReplayOutcome,
    ReplayRecord, ReplayStore, ResumeChallenge, ResumeProof, ResumeTicket, ResumedSession,
    RevokeMessage, principal_for,
};
pub use pairing::{
    EnrolGrant, HostPairing, JoinerPairing, PairAccept, PairConfirm, PairEnrol, PairHostNonce,
    PairOffer, PairReveal, PairingOutcome, PendingApproval, format_code, join_verifier,
    key_fingerprint,
};
pub use registry::{
    GUEST_IDLE_MS, GUEST_LEASE_DEFAULT_MS, GUEST_LEASE_MAX_MS, GUEST_REFUSED, GUEST_SEEN_WRITE_MS,
    GuestPass, MemberRole, PairedDevice, PairedState, Registry, RegistryStore,
};
pub use rightkit_device_link_types::{
    DevicePlatform, DeviceRef, EffectClass, PAIRING_TTL_MS, Principal, PrincipalKind,
};

/// Version of every message and record in this module. A change to a field or to a signed
/// byte layout bumps it; readers reject an unknown version.
///
/// 2: `PairOffer` carries `host_tls_pin` and `host_link_port`, both bound into the commitment
/// and the transcript; new `PairEnrol`; `PairedDevice.transport_id`; request replay state is
/// a [`ReplayStore`] instead of a start-time floor.
pub const DEVICE_LINK_SCHEMA_VERSION: u32 = 2;

/// At most this many pairing sessions may start per [`PAIRING_START_WINDOW_MS`]. Each session
/// gives an active attacker one guess at the authentication code, so starts are rate-limited.
pub const PAIRING_STARTS_PER_WINDOW: usize = 5;
/// Window for [`PAIRING_STARTS_PER_WINDOW`].
pub const PAIRING_START_WINDOW_MS: u64 = 600_000;
/// Digits in the pairing code.
pub const PAIRING_CODE_DIGITS: usize = 8;

/// Default life of a resume ticket.
pub const RESUME_TTL_DEFAULT_MS: u64 = 24 * 60 * 60 * 1000;
/// Longest life a resume ticket may be given, whatever the caller asks.
pub const RESUME_TTL_MAX_MS: u64 = 7 * 24 * 60 * 60 * 1000;
/// A resume challenge must be answered within this time and is single use.
pub const RESUME_CHALLENGE_TTL_MS: u64 = 30_000;
/// Longest window between `issued_at_ms` and `expires_at_ms` of one signed request.
pub const REQUEST_MAX_WINDOW_MS: u64 = 120_000;
/// Clock skew tolerated on `issued_at_ms` (a request issued this far in the future is refused).
pub const CLOCK_SKEW_MS: u64 = 30_000;
// A caller's `now_ms` that is more than CLOCK_SKEW_MS older than the newest one already seen by
// the same pairing session or the same `DeviceLink` is refused as `LinkError::ClockRolledBack`:
// a rolled-back clock must not bring an expired offer, challenge, ticket or request back to
// life. A restart forgets the in-memory mark; ticket and request expiries still bind to the
// signed times.

/// Why a device-link operation was refused. None of these carry key material.
#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    #[error("randomness unavailable")]
    Random,
    #[error("key vault: {0}")]
    Vault(String),
    #[error("registry file: {0}")]
    Store(String),
    #[error("malformed message: {0}")]
    Malformed(&'static str),
    #[error("unsupported schema version {0}")]
    SchemaVersion(u32),
    #[error("message sent out of order; the pairing session is void")]
    WrongPhase,
    #[error("expired")]
    Expired,
    #[error("the pairing commitment did not match; the pairing session is void")]
    CommitmentMismatch,
    #[error("the typed code did not match; the pairing session is void")]
    CodeMismatch,
    #[error("the host approval did not match this pairing session; the session is void")]
    ApprovalMismatch,
    #[error("signature invalid")]
    SignatureInvalid,
    #[error("too many pairing sessions started recently")]
    PairingRateLimited,
    #[error("unknown device")]
    UnknownDevice,
    #[error("device is revoked")]
    Revoked,
    #[error("pairing id does not match the current pairing with this device")]
    PairingMismatch,
    #[error("the record changed since it was read (have revision {have}, record is at {now})")]
    RevisionConflict { have: u64, now: u64 },
    #[error("the device holds no grant for this effect class")]
    NotGranted,
    #[error("the device's grants have expired")]
    GrantExpired,
    #[error("request already seen")]
    Replay,
    #[error("the replay store is unavailable: {0}")]
    ReplayStore(String),
    #[error("the certificate the host presented is not the one its offer names")]
    TlsPinMismatch,
    #[error("too many outstanding challenges")]
    Busy,
    #[error("the sender may not revoke this device")]
    NotAuthorized,
    #[error("the clock moved back; the check is refused until it catches up")]
    ClockRolledBack,
}

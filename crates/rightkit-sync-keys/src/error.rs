//! Errors. No variant carries key material; messages are safe to log.

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// The OS credential store failed (see the wrapped error; it never carries a secret).
    #[error("credential store: {0}")]
    Store(#[from] rightkit_credentials::Error),
    /// An argument was rejected before any work was done.
    #[error("invalid {what}: {reason}")]
    Invalid { what: &'static str, reason: String },
    /// This device holds no data key for the account yet.
    #[error("no sync key is enrolled on this device")]
    NotEnrolled,
    /// A different key already holds the version being written (a concurrent rotation or an
    /// install over a different current key without `replace`).
    #[error("a different sync key already exists at version {version}")]
    VersionConflict { version: u32 },
    /// A stored item is not a well-formed key (truncated, not hex, wrong length).
    #[error("stored sync key is corrupt")]
    Corrupt,
    /// The requested key id or version is not held on this device.
    #[error("sync key not held on this device")]
    UnknownKey,
    /// The OS random source failed.
    #[error("random source failed")]
    Random,
    /// A protocol message is malformed, of the wrong type or version, or from another session.
    #[error("transfer protocol violation: {0}")]
    Protocol(&'static str),
    /// The typed short code does not match the one this device computed. `remaining` further
    /// tries are left on this session.
    #[error("the verification codes do not match ({remaining} tries left)")]
    SasMismatch { remaining: u32 },
    /// Too many wrong codes: the session is burned; start a new one.
    #[error("too many wrong verification codes; the session is closed")]
    TooManyAttempts,
    /// The peer's public key is a low-order point (the exchange would not be contributory).
    #[error("peer public key rejected")]
    WeakPeerKey,
    /// The commit sent first does not match the later reveal.
    #[error("commitment does not match the revealed values")]
    CommitmentMismatch,
    /// AEAD authentication failed: the wrapped key was tampered with, bound to another
    /// transcript, or wrapped under another secret.
    #[error("authentication failed")]
    Authentication,
}

//! Errors. No variant can carry a credential value; messages are safe to log.

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// A test/dev guard environment variable disabled the OS store.
    #[error("credential store disabled by guard {guard}")]
    Guarded { guard: String },
    /// No usable OS credential store (unsupported platform, no logon session, no keychain).
    #[error("credential store unavailable: {0}")]
    Unavailable(String),
    /// The operation needs a user prompt (locked keychain, ACL) and the store is non-interactive.
    #[error("credential store needs user interaction but the store is non-interactive")]
    InteractionRequired,
    /// The user (or OS policy) denied or cancelled access.
    #[error("access to the credential was denied or cancelled")]
    Denied,
    #[error("credential store operation {op} timed out after {secs}s")]
    Timeout { op: &'static str, secs: u64 },
    /// An earlier mutation of the same credential timed out and is still running inside the
    /// OS credential store. Starting another one now could be overwritten by it, so the new
    /// call is refused; retry once the earlier call has finished.
    #[error("credential store operation {op} refused: an earlier change to this credential is still in progress")]
    InProgress { op: &'static str },
    #[error("invalid {what}: {reason}")]
    Invalid { what: &'static str, reason: String },
    /// Any other OS failure: operation name and OS status code only.
    #[error("credential store error during {op} (code {code})")]
    Backend { op: &'static str, code: i64 },
    #[error("credential value is not valid UTF-8")]
    NotUtf8,
    #[error("migration i/o error: {0}")]
    Io(String),
    #[error("apple helper error: {0}")]
    Helper(String),
}

impl Error {
    /// True when the store answered "no" rather than failing. Callers must never treat other
    /// errors as absence (minting a new key over an unreadable one strands data).
    pub fn is_unavailable_or_blocked(&self) -> bool {
        matches!(
            self,
            Error::Guarded { .. }
                | Error::Unavailable(_)
                | Error::InteractionRequired
                | Error::Denied
                | Error::Timeout { .. }
                | Error::InProgress { .. }
        )
    }
}

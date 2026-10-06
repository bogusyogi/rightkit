//! Error taxonomy shared by every adapter.

#[derive(Debug, Clone, PartialEq)]
pub enum SyncError {
    /// Sync cannot run at all right now (signed out, restricted, entitlement
    /// missing). Surfaced to the user; retried on the backoff cadence.
    Unavailable(String),
    /// Transient failure with a server- or policy-supplied delay.
    RetryAfter { secs: f64, detail: String },
    /// Transient failure without a delay hint.
    Transient(String),
    /// The backend invalidated the change token. The engine resets the token
    /// once and replays the full zone; local data is never discarded.
    TokenExpired,
    /// The zone was deleted or never created. The engine calls `prepare`
    /// again once and replays the full zone.
    ZoneMissing,
    /// Durable local state could not be written or read back.
    Persistence(String),
    /// Conditional save refused: the server record changed since the change
    /// tag the write was based on. The outcome carries the server record.
    Conflict,
    /// The auth generation changed or sync was disabled mid-pass.
    Cancelled,
}

impl SyncError {
    pub fn retry_hint_secs(&self) -> Option<f64> {
        match self {
            Self::RetryAfter { secs, .. } if secs.is_finite() && *secs > 0.0 => Some(*secs),
            _ => None,
        }
    }
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(d) | Self::Transient(d) | Self::Persistence(d) => write!(f, "{d}"),
            Self::RetryAfter { detail, .. } => write!(f, "{detail}"),
            Self::TokenExpired => write!(f, "change token expired"),
            Self::ZoneMissing => write!(f, "sync zone missing"),
            Self::Cancelled => write!(f, "sync cancelled"),
            Self::Conflict => write!(f, "server record changed"),
        }
    }
}

impl std::error::Error for SyncError {}

pub type SyncResult<T> = Result<T, SyncError>;

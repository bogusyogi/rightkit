//! Refusals with stable codes, and why a rule refused.

use serde_json::{json, Value};

/// A refusal or failure, with a stable `code` a caller can rely on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub code: &'static str,
    pub message: String,
}

impl Refusal {
    pub fn new(code: &'static str, message: impl Into<String>) -> Refusal {
        Refusal {
            code,
            message: message.into(),
        }
    }

    /// `{"error": message, "code": code}`.
    pub fn to_json(&self) -> Value {
        json!({"error": self.message, "code": self.code})
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for Refusal {}

/// `Result` with a [`Refusal`].
pub type Result<T> = std::result::Result<T, Refusal>;

/// Why a rule in [`crate::rules`] refused. Each app words these for its own callers;
/// [`Error::refusal`] gives a default code and sentence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// No such request: never filed, or expired and swept.
    Unknown,
    /// The request still waits for a decision.
    Pending,
    /// The decider refused it.
    Denied,
    /// It was used already: an approval is spent by one act.
    Used,
    /// It was given for another kind, scope, door, filer or destination.
    Different,
    /// The exact arguments (or their revision) changed after it was asked for.
    Changed,
    /// What the decider was shown is not what waits (display digest mismatch).
    Shown,
    /// Its time ran out.
    Expired,
    /// Too many requests wait already.
    Busy,
    /// The request is malformed (an empty kind, a field too long).
    Malformed,
    /// The proof of who decided did not verify; the proof's own refusal.
    Proof(Refusal),
    /// The store failed; the store's own refusal.
    Store(Refusal),
}

impl Error {
    /// The default refusal for this error.
    pub fn refusal(&self) -> Refusal {
        match self {
            Error::Unknown => Refusal::new(
                "approval_unknown",
                "no such approval: unknown, expired or already used",
            ),
            Error::Pending => Refusal::new("approval_pending", "still waiting for a decision"),
            Error::Denied => Refusal::new("approval_denied", "the approval was refused"),
            Error::Used => Refusal::new("approval_used", "the approval was already used"),
            Error::Different => Refusal::new(
                "approval_different",
                "the approval was given for a different action or target",
            ),
            Error::Changed => Refusal::new(
                "approval_changed",
                "the exact arguments changed after the approval was asked for",
            ),
            Error::Shown => Refusal::new(
                "approval_shown_changed",
                "what was answered is not what is waiting",
            ),
            Error::Expired => Refusal::new("approval_expired", "the approval expired"),
            Error::Busy => Refusal::new("approval_busy", "too many approval requests are waiting"),
            Error::Malformed => {
                Refusal::new("approval_invalid", "the approval request is malformed")
            }
            Error::Proof(r) | Error::Store(r) => r.clone(),
        }
    }
}

impl From<Refusal> for Error {
    fn from(r: Refusal) -> Error {
        Error::Store(r)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.refusal().fmt(f)
    }
}

impl std::error::Error for Error {}

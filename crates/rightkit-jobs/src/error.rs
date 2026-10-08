use std::fmt;

/// Typed engine error: `code` is the stable wire identifier. `retryable`
/// errors pause a running job (it stays recoverable) instead of failing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

/// Stable wire code for [`JobError::handoff`].
pub const HOST_HANDOFF: &str = "host_handoff";
/// Stable wire code for [`JobError::stalled`].
pub const STALLED: &str = "stalled";
/// Stable wire code for [`JobError::stale_attempt`].
pub const STALE_ATTEMPT: &str = "stale_attempt";

pub type JobResult<T> = Result<T, JobError>;

impl JobError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: false,
        }
    }
    pub fn retryable(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: true,
        }
    }
    /// The host is handing the service to a higher-ranked host. Retryable: the
    /// job pauses (still recoverable) without spending an attempt, and the
    /// client resubmits against the new host.
    pub fn handoff() -> Self {
        Self::retryable(
            HOST_HANDOFF,
            "host is handing the service to a higher-ranked host; retry",
        )
    }
    /// No progress for the stall timeout. Retryable, and it spends an attempt.
    pub fn stalled(idle_secs: f64) -> Self {
        Self::retryable(STALLED, format!("no progress for {idle_secs:.1}s"))
    }
    /// A write from an attempt that no longer owns the job (its generation was
    /// superseded by a newer run, e.g. after a stall watchdog fenced it). Not
    /// retryable: the stale attempt must stop, and its write has no effect.
    pub fn stale_attempt(job_id: &str) -> Self {
        Self::new(
            STALE_ATTEMPT,
            format!("job {job_id}: write from a superseded attempt was rejected"),
        )
    }
    pub fn cancelled() -> Self {
        Self::new("cancelled", "job was cancelled")
    }
    pub fn bad_input(message: impl Into<String>) -> Self {
        Self::new("bad_input", message)
    }
    pub fn to_json(&self) -> serde_json::Value {
        self.to_json_with(true)
    }
    /// Like [`JobError::to_json`], omitting `retryable` when `include_retryable`
    /// is false (an app whose wire `error` is `{code, message}`).
    pub fn to_json_with(&self, include_retryable: bool) -> serde_json::Value {
        if include_retryable {
            serde_json::json!({"code": self.code, "message": self.message, "retryable": self.retryable})
        } else {
            serde_json::json!({"code": self.code, "message": self.message})
        }
    }
}

impl fmt::Display for JobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for JobError {}

impl From<rightkit_db::rusqlite::Error> for JobError {
    fn from(e: rightkit_db::rusqlite::Error) -> Self {
        Self::new("db", format!("database error: {e}"))
    }
}
impl From<rightkit_db::DbError> for JobError {
    fn from(e: rightkit_db::DbError) -> Self {
        Self::new("db", format!("database error: {e}"))
    }
}
impl From<std::io::Error> for JobError {
    fn from(e: std::io::Error) -> Self {
        Self::new("io", format!("io error: {e}"))
    }
}

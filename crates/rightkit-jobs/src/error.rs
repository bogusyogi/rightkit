use std::fmt;

/// Where a [`JobError`] arose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ErrorOrigin {
    /// The engine answered with it (its handler, or its dispatcher).
    #[default]
    Engine,
    /// The shell raised it on its own side of the connection: `down`,
    /// `not_ready`, `timeout`, a dead connection, a rejected hello.
    Supervisor,
}

/// Typed engine error: `code` is the stable wire identifier. `retryable`
/// errors pause a running job (it stays recoverable) instead of failing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub origin: ErrorOrigin,
}

/// Stable wire code for [`JobError::handoff`].
pub const HOST_HANDOFF: &str = "host_handoff";
/// Stable wire code for [`JobError::stalled`].
pub const STALLED: &str = "stalled";
/// Stable wire code for [`JobError::stale_attempt`].
pub const STALE_ATTEMPT: &str = "stale_attempt";
/// Stable wire code of the terminal error a `Recovery::Never` job gets when its runner is lost.
pub const INTERRUPTED: &str = "interrupted";
/// Stable wire code of the cancel error a single-flight submit leaves on the job it replaced.
pub const SUPERSEDED: &str = "superseded";
/// Stable wire code for [`JobError::contract_mismatch`].
pub const CONTRACT_VERSION_MISMATCH: &str = "contract_version_mismatch";

pub type JobResult<T> = Result<T, JobError>;

impl JobError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: false,
            origin: ErrorOrigin::Engine,
        }
    }
    pub fn retryable(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: true,
            origin: ErrorOrigin::Engine,
        }
    }
    /// An error the shell raised itself, not relayed from an engine reply.
    pub fn supervisor(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(code, message).with_origin(ErrorOrigin::Supervisor)
    }
    /// As [`Self::supervisor`], retryable.
    pub fn supervisor_retryable(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::retryable(code, message).with_origin(ErrorOrigin::Supervisor)
    }
    /// Records where the error arose.
    pub fn with_origin(mut self, origin: ErrorOrigin) -> Self {
        self.origin = origin;
        self
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
    /// The engine's hello advertises a contract version the shell does not
    /// require. Not retryable: the engine binary has to change.
    pub fn contract_mismatch(advertised: impl fmt::Display, required: &str) -> Self {
        Self::supervisor(
            CONTRACT_VERSION_MISMATCH,
            format!("engine speaks contract_version {advertised}; this shell requires {required}"),
        )
    }
    /// A `Recovery::Never` job began an attempt and lost its runner (crash or lost lock). It is
    /// failed with this error and never handed to a handler again.
    pub fn interrupted(job_id: &str) -> Self {
        Self::new(
            INTERRUPTED,
            format!(
                "job {job_id} was interrupted after it started and its recovery class is never; it was not run again"
            ),
        )
    }
    /// A `Recovery::Never` job's handler ended with a retryable error. The handler may already
    /// have had effects, so the job fails instead of pausing for a later re-run.
    pub fn interrupted_by(job_id: &str, cause: &JobError) -> Self {
        Self::new(
            INTERRUPTED,
            format!(
                "job {job_id} stopped after it started ({}: {}) and its recovery class is never; it was not run again",
                cause.code, cause.message
            ),
        )
    }
    /// The job was replaced by a newer single-flight submit (`successor`).
    pub fn superseded(successor: &str) -> Self {
        Self::new(
            SUPERSEDED,
            format!("superseded by job {successor} (single-flight submit)"),
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

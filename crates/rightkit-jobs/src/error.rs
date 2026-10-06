use std::fmt;

/// Typed engine error: `code` is the stable wire identifier. `retryable`
/// errors pause a running job (it stays recoverable) instead of failing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

pub type JobResult<T> = Result<T, JobError>;

impl JobError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self { code: code.into(), message: message.into(), retryable: false }
    }
    pub fn retryable(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self { code: code.into(), message: message.into(), retryable: true }
    }
    pub fn bad_input(message: impl Into<String>) -> Self {
        Self::new("bad_input", message)
    }
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({"code": self.code, "message": self.message})
    }
}

impl fmt::Display for JobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for JobError {}

impl From<rusqlite::Error> for JobError {
    fn from(e: rusqlite::Error) -> Self {
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

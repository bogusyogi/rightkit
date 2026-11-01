use thiserror::Error;

/// Fail-closed errors at the Legion/the host app policy boundary.
#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub enum AdapterError {
    #[error("unsupported schema version: {0}")]
    UnsupportedSchema(u32),
    #[error("invalid policy: {0}")]
    InvalidPolicy(String),
    #[error("unknown effect or capability: {0}")]
    UnknownEffect(String),
    #[error("unmappable policy input: {0}")]
    Unmappable(String),
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("policy denied effect: {0}")]
    Denied(String),
    #[error("approval required")]
    ApprovalRequired,
    #[error("execution failed: {0}")]
    Execution(String),
}

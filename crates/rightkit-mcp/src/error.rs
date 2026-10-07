use std::fmt;

use crate::trust::PendingApproval;

/// Every failure the crate reports.
#[derive(Debug, Clone)]
pub enum McpError {
    Transport(String),
    Protocol(String),
    Rpc {
        code: i64,
        message: String,
    },
    ToolNotFound(String),
    /// Refused by an admission hook or a recorded trust denial; nothing ran.
    Denied(String),
    /// Workspace server not yet approved; nothing was spawned or contacted.
    NotApproved(Box<PendingApproval>),
    PoolFull(usize),
    Timeout,
    Closed,
    Unauthorized,
    Collision {
        qualified: String,
        originals: Vec<String>,
    },
    Config(String),
    Io(String),
}

pub type Result<T> = std::result::Result<T, McpError>;

impl fmt::Display for McpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            McpError::Transport(m) => write!(f, "transport error: {m}"),
            McpError::Protocol(m) => write!(f, "protocol error: {m}"),
            McpError::Rpc { code, message } => write!(f, "rpc error {code}: {message}"),
            McpError::ToolNotFound(n) => write!(f, "not found: {n}"),
            McpError::Denied(m) => write!(f, "denied: {m}"),
            McpError::NotApproved(p) => write!(
                f,
                "server '{}' in workspace '{}' is not approved (fingerprint {})",
                p.server, p.workspace, p.fingerprint
            ),
            McpError::PoolFull(n) => write!(f, "connection pool full ({n} in use)"),
            McpError::Timeout => write!(f, "request timed out"),
            McpError::Closed => write!(f, "connection closed"),
            McpError::Unauthorized => write!(f, "unauthorized"),
            McpError::Collision {
                qualified,
                originals,
            } => {
                write!(f, "tool name collision on '{qualified}': {originals:?}")
            }
            McpError::Config(m) => write!(f, "config error: {m}"),
            McpError::Io(m) => write!(f, "io error: {m}"),
        }
    }
}

impl std::error::Error for McpError {}

impl From<std::io::Error> for McpError {
    fn from(e: std::io::Error) -> Self {
        McpError::Io(e.to_string())
    }
}

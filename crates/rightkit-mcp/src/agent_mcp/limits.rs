//! The limits that can cut an MCP user off, in one place (contract R18). Each one that
//! reaches the user is named in its error text (`McpError::Timeout` carries the call limit).
//! Other limits stay beside the code they bound.

use std::time::Duration;

/// How long discovery waits for a server that is not ready before its tools join later.
pub const MCP_STARTUP_BOUND: Duration = Duration::from_secs(3);

/// The single total deadline for one tool call (lookup, listing, call and any retries).
pub const MCP_CALL_DEADLINE: Duration = Duration::from_secs(120);

/// Per-request timeout on a stdio server.
pub const MCP_STDIO_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Per-request timeout on an HTTP or SSE server.
pub const MCP_HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// How long a server's elicitation request waits for the operator before it is declined.
pub const MCP_ELICITATION_WAIT: Duration = Duration::from_secs(30 * 60);

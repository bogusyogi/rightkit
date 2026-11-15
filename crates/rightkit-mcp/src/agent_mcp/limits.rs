//! The limits that can cut an MCP user off, in one place (contract R18). Each one that
//! reaches the user is named in its error text (`McpError::Timeout` carries the call limit).
//! Other limits stay beside the code they bound.

use std::time::Duration;

/// How long discovery waits for a server that is not ready before its tools join later.
pub const MCP_STARTUP_BOUND: Duration = Duration::from_secs(3);

/// How long a call, or a listing of one named server, waits for that server to start. Longer than
/// [`MCP_STARTUP_BOUND`] because the caller asked for this server and a cold `npx -y` start takes
/// more than 3 s; a per-server `startup_timeout_sec` still wins.
pub const MCP_EXPLICIT_STARTUP_BOUND: Duration = Duration::from_secs(10);

/// The single total deadline for one tool call (lookup, listing, call and any retries).
pub const MCP_CALL_DEADLINE: Duration = Duration::from_secs(120);

/// Per-request timeout on a stdio server.
pub const MCP_STDIO_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Per-request timeout on an HTTP or SSE server.
pub const MCP_HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// How long a server's elicitation request waits for the operator before it is declined.
pub const MCP_ELICITATION_WAIT: Duration = Duration::from_secs(30 * 60);

/// Bound on one stored-token refresh at session build. Refreshes run side by side, so this is
/// also the most they add to session creation.
#[cfg(feature = "agent_mcp-os-keyring")]
pub const MCP_OAUTH_REFRESH_BOUND: Duration = Duration::from_secs(10);

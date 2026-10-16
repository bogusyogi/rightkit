//! Shared MCP client and server host for Right Suite apps. See API.md.
//!
//! `ClientOptions::connect_timeout` configures spawn/connect & initialize independently of requests.
//! `McpClient::connect_with_extensions_cancellable` aborts cold handshakes with `McpError::Cancelled`.
//! `McpError::StdioPrefixTooLarge` reports skipped startup output exceeding 64 KiB.
//! Responses require `jsonrpc: "2.0"` & either `error` or `result` (including null).
//! `EndpointOriginPolicy::allow_any_origin_insecure` explicitly opts into unrestricted HTTP(S) origins.
//! `ElicitationContext::request_id` exposes original wire id; `cancellation` observes originating call cancellation.

#[cfg(feature = "advanced-http")]
mod advanced_http;
#[cfg(not(feature = "advanced-http"))]
#[path = "advanced_http_off.rs"]
mod advanced_http;
pub mod auth;
pub mod client;
pub mod client_features;
pub mod config;
mod error;
pub mod events;
pub mod pool;
mod raw;
mod sanitize;
pub mod server;
pub mod trust;
mod types;

pub use auth::{AuthProvider, Secret};
pub use client::{ClientOptions, McpClient, ServerSpec};
pub use client_features::{
    CallOptions, CancellationToken, ClientExtensions, DefaultProcessLauncher, ElicitationContext,
    ElicitationHandler, EndpointOriginPolicy, LaunchRequest, LaunchedProcess, ProcessChild,
    ProcessLauncher, ResponseLimits,
};
pub use error::{McpError, Result};
pub use events::{EventSink, LifecycleKind, McpEvent};
pub use pool::{Catalog, ConnectionPool, PoolConfig, QualifiedTool, ServerEntry};
pub use raw::{to_wire_string, RawJson};
pub use sanitize::{parse_qualified_name, sanitize_tool_name};
pub use server::{
    Admission, AdmissionHook, AdmissionRequest, AsyncRequestHandler, BatchPolicy, HttpServeOptions,
    HttpServer, IdPolicy, InitializedPolicy, ReadContext, ReadOnlyPolicy, RequestContext,
    ServerHost, ServerOptions, ServerTransport, StdioInputMode, ToolProvider,
};
pub use trust::{fingerprint, PendingApproval, ServerSource, TrustDecision, TrustStore};
pub use types::*;

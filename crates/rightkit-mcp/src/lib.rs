//! Shared MCP client and server host for Right Suite apps. See API.md.

mod advanced_http;
pub mod auth;
pub mod client;
pub mod client_features;
pub mod config;
mod error;
pub mod events;
pub mod pool;
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
pub use sanitize::{parse_qualified_name, sanitize_tool_name};
pub use server::{
    Admission, AdmissionHook, AdmissionRequest, AsyncRequestHandler, BatchPolicy, HttpServeOptions,
    HttpServer, IdPolicy, InitializedPolicy, ReadContext, ReadOnlyPolicy, RequestContext,
    ServerHost, ServerOptions, ServerTransport, StdioInputMode, ToolProvider,
};
pub use trust::{fingerprint, PendingApproval, ServerSource, TrustDecision, TrustStore};
pub use types::*;

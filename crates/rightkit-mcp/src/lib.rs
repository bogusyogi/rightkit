//! Shared MCP client and server host for Right Suite apps. See API.md.

pub mod auth;
pub mod client;
pub mod config;
mod error;
pub mod pool;
mod sanitize;
pub mod server;
pub mod trust;
mod types;

pub use auth::{AuthProvider, Secret};
pub use client::{ClientOptions, McpClient, ServerSpec};
pub use error::{McpError, Result};
pub use pool::{Catalog, ConnectionPool, PoolConfig, QualifiedTool, ServerEntry};
pub use sanitize::{parse_qualified_name, sanitize_tool_name};
pub use server::{
    Admission, AdmissionHook, AdmissionRequest, HttpServeOptions, HttpServer, ReadOnlyPolicy, ServerHost,
    ServerTransport, ToolProvider,
};
pub use trust::{fingerprint, PendingApproval, ServerSource, TrustDecision, TrustStore};
pub use types::*;

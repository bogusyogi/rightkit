//! Binary-independent daemon primitives for an agent host: the trusted-source authority gate,
//! the daemon settings and engine configuration DTOs, the live-turn handle and the lifecycle
//! authority every operator surface submits commands to. See README.md.

mod authority;
mod dto;
pub mod http_client;
pub mod lifecycle;
mod state;

pub use authority::{
    enforce_trusted_source_authority, lower_source_trust, set_source_trust_header,
    source_trust_header, DEFAULT_SOURCE_TRUST_HEADER,
};
pub use dto::{DaemonSettingsStore, EngineConfig};
pub use lifecycle::*;
pub use state::{LiveTurnGuard, LiveTurnHandle};

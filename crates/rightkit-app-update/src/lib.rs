//! The native half of Right Suite app updates that the Tauri shell host does not carry.
//!
//! - [`probation`]: the failed-boot sentinel and BootStage probation after an update (reinstall
//!   guidance, no rollback), see [`BootProbation`].
//! - [`fetch`]: an allowlisted, redacted webview `fetch` over rightkit-http, see [`FetchBridge`].
//!
//! Update check, download, minisign verification and install are owned by
//! `rightkit-tauri-shell`'s `update` feature (`UpdateHost`, `TauriUpdateTransport`), which
//! authorizes every request through `RequestAuthorizer`; this crate does not duplicate them.

#![forbid(unsafe_code)]

pub mod fetch;
pub mod probation;

pub use fetch::{
    FetchBridge, FetchError, FetchErrorCode, FetchPolicy, FetchRequest, FetchResponse,
};
pub use probation::{
    BootDecision, BootFailure, BootProbation, ProbationError, Sentinel, DEFAULT_MAX_BOOT_ATTEMPTS,
    GUIDANCE_REINSTALL,
};

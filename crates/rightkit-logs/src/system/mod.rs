//! Process-free platform identity and content-free resource metrics.
//!
//! Ported from HeardRight `system_identity.rs` / `system_metrics.rs`. These are
//! the only modules in the crate that call OS APIs, so `unsafe` is permitted
//! here and nowhere else.
#![allow(unsafe_code)]

pub mod identity;
pub mod metrics;

pub use identity::{current as platform_identity, PlatformIdentity};
pub use metrics::snapshot as resource_snapshot;

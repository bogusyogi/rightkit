//! Backends that turn a capability into typed [`crate::Tool`]s.

pub mod control;
#[cfg(feature = "automation")]
pub mod desktop;
#[cfg(feature = "speech")]
pub mod speech;
#[cfg(feature = "browser")]
pub mod web;

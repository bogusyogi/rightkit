//! Agent Room core. Behavior is introduced test-first in the P0 implementation.

pub mod auth;
pub mod protocol;
pub mod runtime;
#[cfg(feature = "http")]
pub mod server;
pub mod store;

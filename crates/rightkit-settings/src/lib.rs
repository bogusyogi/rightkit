//! Versioned per-app settings persistence. See README.md.

mod atomic;
mod secret;
mod store;

pub use atomic::{atomic_write, atomic_write_with};
pub use secret::{Presence, SecretProbe, SecretRef};
pub use store::{Change, Error, Migration, Schema, Store, Subscription};

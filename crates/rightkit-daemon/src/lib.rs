//! rightkit-daemon: the daemon host capability (RK10).
//!
//! Implements the `daemon-api` v1 contract: three authorisation classes
//! (device-authenticated mutations, OAuth callbacks, signed webhooks), bounded
//! blake3-keyed resumable uploads, compare-and-swap mutations, durable state and
//! structured lifecycle events. The core ([`Daemon`]) is transport-free and
//! platform-neutral; [`router`] adapts it to axum and [`serve`] terminates TLS
//! with rustls.

pub mod config;
pub mod daemon;
pub mod events;
pub mod http;
#[cfg(feature = "oauth")]
pub mod oauth;
pub mod routes;
pub mod signing;
mod state;
mod util;

pub use config::{DaemonConfig, UploadLimits};
pub use daemon::{Daemon, DaemonMetrics, DaemonStatus, OAuthCallbackHandler, OAuthOutcome, Request, Response};
pub use events::{Event, EventLog};
pub use http::{load_tls, router, serve};
pub use util::{hex, sha256_hex, unhex, Clock, SystemClock};

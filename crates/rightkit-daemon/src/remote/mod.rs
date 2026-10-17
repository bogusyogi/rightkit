//! rightkit-daemon remote transport: the daemon host capability (RK10), moved here from
//! the retired standalone daemon crate by the M7 merge.
//!
//! Implements the `daemon-api` v1 contract: three authorisation classes
//! (device-authenticated mutations, OAuth callbacks, signed webhooks), bounded
//! blake3-keyed resumable uploads, compare-and-swap mutations, durable state and
//! structured lifecycle events. The core ([`self::core::Daemon`]) is transport-free and
//! platform-neutral; [`http::router`] adapts it to axum and [`http::serve`] terminates TLS
//! with rustls. [`http::serve_blocking`] runs it on a runtime the crate owns.
//!
//! Feature gates:
//! * `link` - the blocking device client ([`client`]) plus the request signing ([`signing`])
//!   and hashing helpers it needs. No tokio, axum or hyper.
//! * `remote` (implies `link`) - the server stack: core, HTTP listener, routes, OAuth adapter,
//!   durable state and events.
//! * The outbound link (`rk-link/1`, see [`crate::link`]): the host side is in [`core`]
//!   (`Daemon::link_accept`, `enqueue_task`, durable leases) and [`http`] (the upgrade).
//! * `testing` (implies `remote`) - a run-time test certificate and in-process host ([`testing`]).

#[cfg(feature = "remote")]
pub mod call;
#[cfg(feature = "link")]
pub mod client;
#[cfg(feature = "remote")]
pub mod config;
#[cfg(feature = "remote")]
pub mod core;
#[cfg(feature = "remote")]
pub mod events;
#[cfg(feature = "remote")]
pub mod http;
#[cfg(feature = "remote")]
pub mod oauth;
#[cfg(feature = "link")]
pub mod pairing;
#[cfg(feature = "remote")]
pub mod routes;
#[cfg(feature = "link")]
pub mod signing;
#[cfg(feature = "remote")]
mod state;
#[cfg(feature = "testing")]
pub mod testing;
#[cfg(feature = "link")]
pub(crate) mod util;

#[cfg(feature = "remote")]
pub use self::call::DeviceCalls;
#[cfg(feature = "remote")]
pub use self::config::{DaemonConfig, UploadLimits};
#[cfg(feature = "remote")]
pub use self::core::{
    Accepted, Daemon, DaemonMetrics, DaemonStatus, DeviceInfo, EventsPage, ForgetDeviceError,
    FrameOutcome, LinkSession,
    OAuthCallbackHandler, OAuthOutcome, Out, PairedDevice, PairingError, PairingInfo,
    PairingOptions, PairingStage, Request, Response, TaskInfo, TaskSpec,
};
#[cfg(feature = "remote")]
pub use self::events::{Event, EventLog};
#[cfg(feature = "remote")]
pub use self::http::{load_tls, router, serve, serve_blocking, StopHandle};
#[cfg(feature = "remote")]
pub use self::state::TaskStatus;
#[cfg(feature = "link")]
pub use self::util::{hex, sha256_hex, unhex, Clock, SystemClock};

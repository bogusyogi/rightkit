#![allow(clippy::result_large_err)] // errors carry structured diagnostic context by design, as in rightkit-daemon
//! rightkit-daemon-remote: the remote side of a Right Suite daemon, beside the local service in
//! `rightkit-daemon`.
//!
//! * [`remote`] (feature `remote`) - the daemon-api v1 HTTPS host for enrolled devices: device
//!   enrolment and pairing, signed requests, resumable uploads, OAuth callback and signed webhook
//!   routes, durable tasks and lifecycle events, and `POST /v1/call/{method}`, which reaches the
//!   same `rightkit_daemon::contract::Handler` and `MethodTable` a local service hosts. With
//!   `link` alone, `remote::client`, `remote::signing` and `remote::pairing` are the blocking
//!   device side of that contract.
//! * [`link`] (feature `link`) - `rk-link/1`, the outbound link from a machine with no inbound
//!   port to the host. The client is std sockets and rustls only (no tokio, axum or hyper).
//! * [`relay`] (feature `relay`) - `rk-relay/1`, the transport-free blind relay and mailbox
//!   protocol.
//!
//! No feature is on by default. These modules were `rightkit_daemon::{remote, link, relay}`
//! until rightkit-daemon 0.7.0; see README.md and CHANGELOG.md.

#[cfg(feature = "link")]
pub mod link;
#[cfg(feature = "relay")]
pub mod relay;
/// Daemon host (`remote` feature) and device client (`link` feature); see the module docs.
#[cfg(any(feature = "remote", feature = "link"))]
pub mod remote;

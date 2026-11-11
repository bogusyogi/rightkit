//! `rk-link/1`: the outbound link from a machine to the remote host (M7 lane D).
//!
//! A machine with no inbound port opens a TLS connection to the host and upgrades it
//! (`GET /v1/link`, `Upgrade: rk-link/1`, signed with the device key exactly like every other
//! device request). The connection then carries newline-delimited JSON frames in both
//! directions: the host pushes durable, leased tasks, the machine acknowledges and completes
//! them, and either side may call upward or heartbeat.
//!
//! * [`frame`]: the wire format and the per-frame authentication, shared by both sides.
//! * [`client`]: the blocking device-side client (std sockets and rustls; no tokio, axum or
//!   hyper). Feature `link`.
//!
//! The host side lives in `remote` (feature `remote`): `Daemon::link_accept`,
//! `Daemon::enqueue_task` and the lease logic.

pub mod client;
pub mod frame;

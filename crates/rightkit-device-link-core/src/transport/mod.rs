//! Network transport pieces of the device link (`docs/device-link.md`, "Transport").
//!
//! The device link has one transport: the mesh (`rightkit-mesh`, iroh streams; issue #138). The
//! TCP/TLS listener, the per-device certificate authority and the pinned rustls verifier are gone.
//!
//! * `pairing_channel`: the pairing messages in order, offer to enrolment, over one stream the
//!   mesh carries: `join_over` (Joiner) and `open_stream_host` (Host). `PairControl` is what the
//!   stream supplies besides `Read` and `Write` (a bounded wait, and ending the send side).
//!
//! Nothing here opens a socket; the engine's host (`engine/src/devicelink_host.rs`) owns the mesh.

pub mod pairing_channel;

pub use pairing_channel::{
    ChannelError, Decision, HostHooks, JoinPrompt, MeshJoinResult, PairControl, StreamHost,
    join_over, join_over_token, open_stream_host, open_stream_host_join,
};

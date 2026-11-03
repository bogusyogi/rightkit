// RightKit: our own implementation of the part of `netwatch` 0.19.3 that iroh 1.3.0, iroh-relay and portmapper call, for the
// phone build (rightkit-mesh-ffi). MIT OR Apache-2.0. Written against the public API of the crate it stands in for; no
// upstream source is vendored. Why it exists: rightkit-mesh-ffi/ios-netwatch/README.md.
//
// Surface (item -> iroh call site) is listed at the top of each module.

pub mod interfaces;
pub mod ip;
pub mod netmon;
mod udp;

pub use self::udp::{UdpSender, UdpSocket};

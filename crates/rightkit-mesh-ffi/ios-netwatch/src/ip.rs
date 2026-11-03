//! `netwatch::ip::LocalAddresses`: iroh socket.rs (`collect_local_addresses`, the network-change `Hint`).

use std::net::{IpAddr, Ipv6Addr};

/// This machine's IP addresses, split the way iroh wants them.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LocalAddresses {
    /// Loopback addresses.
    pub loopback: Vec<IpAddr>,
    /// Addresses a peer could plausibly reach.
    pub regular: Vec<IpAddr>,
}

/// RFC 4193 unique local unicast (fc00::/7).
pub(crate) fn is_unique_local(ip: &Ipv6Addr) -> bool {
    ip.octets()[0] & 0xfe == 0xfc
}

/// RFC 1918 / RFC 4193 private address.
pub(crate) fn is_private(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_private(),
        IpAddr::V6(ip) => is_unique_local(ip),
    }
}

pub(crate) fn is_link_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_link_local(),
        IpAddr::V6(ip) => (ip.segments()[0] & 0xffc0) == 0xfe80,
    }
}

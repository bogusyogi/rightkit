//! LAN service discovery over multicast DNS and DNS-SD (`lan-discovery` feature): find this
//! machine's network address, announce a service on it, and browse the local network for the same
//! kind of service.
//!
//! The crate stays product-neutral: the host passes the service type (for example
//! `_myapp._tcp.local.`), the instance and host names and the TXT properties, and builds its own
//! rows from [`Resolved`]. Nothing here opens a listener; the host binds its own port and passes
//! it to [`advertise`].
//!
//! The feature is off by default because it brings in `mdns-sd`, which spawns one background
//! thread per [`Advertisement`] (and one for the duration of each [`browse`] call).

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr};
use std::time::{Duration, Instant};

/// The address other machines reach this one on: the source address of the default route (a UDP
/// `connect` sends no packet). `None` when there is no route, or the route's address is
/// unspecified or loopback.
pub fn lan_ip() -> Option<IpAddr> {
    let socket = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect(("192.0.2.1", 9)).ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_unspecified() && !ip.is_loopback()).then_some(ip)
}

/// A service announced on the local network. The service stays registered while this value (the
/// multicast DNS daemon it holds) lives, so the host keeps it for as long as the service should
/// be found, for example in a `static` `OnceLock`.
pub struct Advertisement {
    #[allow(dead_code)] // held so the registration lives as long as this value
    daemon: mdns_sd::ServiceDaemon,
}

/// Announce one service instance and return the handle that keeps it announced.
///
/// `service_type` is the full DNS-SD type with its domain (`_name._tcp.local.`), `instance_name`
/// the human-readable instance label, `host_name` the host record name (`<host>.local.`), `ip`
/// and `port` where the service listens, and `properties` the TXT key/value pairs. Every failure
/// (daemon start, a malformed name or property, registration) is returned as its message.
pub fn advertise(
    service_type: &str,
    instance_name: &str,
    host_name: &str,
    ip: IpAddr,
    port: u16,
    properties: &[(&str, &str)],
) -> Result<Advertisement, String> {
    let daemon = mdns_sd::ServiceDaemon::new().map_err(|error| error.to_string())?;
    let info =
        mdns_sd::ServiceInfo::new(service_type, instance_name, host_name, ip, port, properties)
            .map_err(|error| error.to_string())?;
    daemon.register(info).map_err(|error| error.to_string())?;
    Ok(Advertisement { daemon })
}

/// One service instance found on the network, as the browse resolved it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The instance's full name, the instance label plus the service type
    /// (`<instance>.<service type>`), for example `Name._myapp._tcp.local.`.
    pub fullname: String,
    /// The host record name the instance resolved to (`<host>.local.`).
    pub hostname: String,
    /// Every address the instance resolved to, as text, sorted. Never empty in a [`browse`]
    /// result.
    pub addresses: Vec<String>,
    /// The port the instance listens on.
    pub port: u16,
    /// The TXT properties. Keys are lower-cased (DNS-SD keys are case-insensitive, RFC 6763
    /// section 6.4); a value that is absent or not UTF-8 is the empty string.
    pub properties: BTreeMap<String, String>,
}

/// Browse `service_type` (`_name._tcp.local.`) for `wait` and return every instance that resolved
/// to at least one address, once each (by full name), in the order they resolved. The multicast
/// DNS daemon it starts is shut down before the call returns.
pub fn browse(service_type: &str, wait: Duration) -> Result<Vec<Resolved>, String> {
    let daemon = mdns_sd::ServiceDaemon::new().map_err(|error| error.to_string())?;
    let receiver = daemon
        .browse(service_type)
        .map_err(|error| error.to_string())?;
    let deadline = Instant::now() + wait;
    let mut found: Vec<Resolved> = Vec::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match receiver.recv_timeout(left) {
            Ok(mdns_sd::ServiceEvent::ServiceResolved(info)) => {
                let mut addresses: Vec<String> = info
                    .get_addresses()
                    .iter()
                    .map(|ip| ip.to_string())
                    .collect();
                addresses.sort();
                if addresses.is_empty() {
                    continue;
                }
                let row = Resolved {
                    fullname: info.get_fullname().to_string(),
                    hostname: info.get_hostname().to_string(),
                    addresses,
                    port: info.get_port(),
                    properties: info
                        .get_properties()
                        .iter()
                        .map(|property| {
                            (
                                property.key().to_lowercase(),
                                property.val_str().to_string(),
                            )
                        })
                        .collect(),
                };
                if !found.iter().any(|seen| seen.fullname == row.fullname) {
                    found.push(row);
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let _ = daemon.shutdown();
    Ok(found)
}

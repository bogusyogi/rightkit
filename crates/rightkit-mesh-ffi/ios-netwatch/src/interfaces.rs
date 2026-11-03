//! Interface list from `getifaddrs(3)`.
//!
//! Surface (item -> iroh call site):
//! - `State` (`interfaces`, `local_addresses`, `have_v4`, `have_v6`, `default_route_interface`, `is_major_change`, `new`):
//!   socket.rs (usable-network check, link-change handling, direct addresses), net_report/reportgen.rs (`have_v4`/`have_v6`),
//!   relay/actor.rs (`State::new` for the relay connection check).
//! - `Interface::addrs`, `IpNet`, `Ipv6AddrFlags`: socket.rs `find_flags`, relay/actor.rs.
//! - `HomeRouter`: portmapper lib.rs `ip_and_gateway`. iOS exposes no routing table to apps, so it is always `None` and
//!   port mapping reports "no gateway" (the phone relies on relay and mDNS).

use std::{
    collections::HashMap,
    ffi::CStr,
    fmt,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
};

pub use ipnet::{Ipv4Net, Ipv6Net};
use n0_future::time::Instant;

use crate::ip::{LocalAddresses, is_link_local, is_private, is_unique_local};

const IFF_UP: u32 = 0x1;
const IFF_LOOPBACK: u32 = 0x8;

/// State flags of one IPv6 address. iOS offers no cheap way to read them (`SIOCGIFAFLAG_IN6` needs `in6_ifreq`, which libc
/// does not define for iOS), so they are all `false`.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct Ipv6AddrFlags {
    /// Preferred lifetime expired.
    pub deprecated: bool,
    /// Privacy address.
    pub temporary: bool,
    /// Duplicate address detection in progress.
    pub tentative: bool,
    /// Duplicate address detection failed.
    pub duplicated: bool,
    /// Configured manually.
    pub permanent: bool,
}

/// An IP network on an interface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IpNet {
    /// IPv4.
    V4(Ipv4Net),
    /// IPv6.
    V6 {
        /// Address and prefix.
        net: Ipv6Net,
        /// Interface index for a link-local address, else zero.
        scope_id: u32,
        /// Address flags (always default here).
        flags: Ipv6AddrFlags,
    },
}

impl IpNet {
    /// The address.
    pub fn addr(&self) -> IpAddr {
        match self {
            IpNet::V4(a) => IpAddr::V4(a.addr()),
            IpNet::V6 { net, .. } => IpAddr::V6(net.addr()),
        }
    }
}

/// One network interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interface {
    name: String,
    index: u32,
    flags: u32,
    mac_addr: Option<[u8; 6]>,
    addrs: Vec<IpNet>,
}

impl fmt::Display for Interface {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}. {} up={} addrs={:?}",
            self.index,
            self.name,
            self.is_up(),
            self.addrs
        )
    }
}

impl Interface {
    /// Is the interface up?
    pub fn is_up(&self) -> bool {
        self.flags & IFF_UP != 0
    }
    /// Interface name, such as `en0`.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// The interface's addresses.
    pub fn addrs(&self) -> impl Iterator<Item = IpNet> + '_ {
        self.addrs.iter().cloned()
    }
}

/// Snapshot of the machine's interfaces.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct State {
    /// Interface name -> interface.
    pub interfaces: HashMap<String, Interface>,
    /// Local addresses.
    pub local_addresses: LocalAddresses,
    /// Has a global or unique-local IPv6 address.
    pub have_v6: bool,
    /// Has a non-loopback IPv4 address.
    pub have_v4: bool,
    /// Cellular and the like. Not populated here.
    pub is_expensive: bool,
    /// Best guess at the interface carrying the default route (iOS gives apps no routing table): see [`guess_default`].
    /// iroh treats `None` as "no usable network", so this must be set whenever an interface can carry traffic.
    pub default_route_interface: Option<String>,
    /// When a long suspension was detected (set by the monitor, not by `new`).
    pub last_unsuspend: Option<Instant>,
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut ifaces: Vec<_> = self.interfaces.values().collect();
        ifaces.sort_by_key(|i| i.index);
        for iface in ifaces {
            write!(f, "{iface}")?;
            if self.default_route_interface.as_deref() == Some(iface.name()) {
                write!(f, " (default)")?;
            }
            write!(f, "; ")?;
        }
        Ok(())
    }
}

impl State {
    /// Reads the current interfaces.
    pub async fn new() -> Self {
        Self::read()
    }

    pub(crate) fn read() -> Self {
        let mut interfaces: HashMap<String, Interface> = HashMap::new();
        for iface in list_interfaces() {
            interfaces.insert(iface.name.clone(), iface);
        }
        let local_addresses = local_addresses(interfaces.values());
        let (mut have_v4, mut have_v6) = (false, false);
        for iface in interfaces.values().filter(|i| i.is_up()) {
            for net in &iface.addrs {
                let ip = net.addr();
                if ip.is_loopback() {
                    continue;
                }
                have_v4 |= ip.is_ipv4();
                have_v6 |= matches!(ip, IpAddr::V6(v6) if (v6.segments()[0] & 0xe000) == 0x2000 || is_unique_local(&v6));
            }
        }
        let default_route_interface = guess_default(&interfaces);
        State {
            interfaces,
            local_addresses,
            have_v6,
            have_v4,
            is_expensive: false,
            default_route_interface,
            last_unsuspend: None,
        }
    }

    /// Is this a major change from `old`? Same rule as upstream: family availability, default interface or an interesting
    /// interface's addresses changed. Link-local, loopback and multicast addresses do not count.
    pub fn is_major_change(&self, old: &State) -> bool {
        if self.have_v6 != old.have_v6
            || self.have_v4 != old.have_v4
            || self.is_expensive != old.is_expensive
            || self.default_route_interface != old.default_route_interface
        {
            return true;
        }
        for (name, old_if) in &old.interfaces {
            if !is_interesting(name) {
                continue;
            }
            let Some(new_if) = self.interfaces.get(name) else {
                return true;
            };
            if old_if != new_if || !prefixes_major_equal(old_if.addrs(), new_if.addrs()) {
                return true;
            }
        }
        self.interfaces
            .keys()
            .any(|n| is_interesting(n) && !old.interfaces.contains_key(n))
    }
}

/// iOS churns AWDL, low-latency WLAN, IPsec and tunnel interfaces constantly; they never carry our traffic.
fn is_interesting(name: &str) -> bool {
    let base = name.trim_end_matches(|c: char| c.is_ascii_digit());
    !matches!(
        base,
        "llw" | "awdl" | "ipsec" | "ap" | "anpi" | "gif" | "stf"
    )
}

fn prefixes_major_equal(a: impl Iterator<Item = IpNet>, b: impl Iterator<Item = IpNet>) -> bool {
    let keep = |p: &IpNet| {
        let a = p.addr();
        !(is_link_local(a) || a.is_loopback() || a.is_multicast())
    };
    a.filter(keep).eq(b.filter(keep))
}

/// iOS has no routing-table API for apps. Pick the interface most likely to carry the default route: Wi-Fi (`en0`), then
/// cellular (`pdp_ip0`), then any other up, non-loopback, interesting interface with a routable address.
fn guess_default(interfaces: &HashMap<String, Interface>) -> Option<String> {
    let usable = |i: &Interface| {
        i.flags & IFF_LOOPBACK == 0
            && i.is_up()
            && is_interesting(&i.name)
            && !i.name.starts_with("utun")
            && i.addrs.iter().any(|n| {
                let ip = n.addr();
                !ip.is_loopback()
                    && !is_link_local(ip)
                    && !ip.is_multicast()
                    && !ip.is_unspecified()
            })
    };
    for preferred in ["en0", "pdp_ip0"] {
        if interfaces.get(preferred).is_some_and(usable) {
            return Some(preferred.to_string());
        }
    }
    interfaces
        .values()
        .filter(|i| usable(i))
        .min_by_key(|i| i.index)
        .map(|i| i.name.clone())
}

/// Port of upstream's split of addresses into loopback and regular, with the same fallbacks (link-local v4 and ULA v6 when
/// nothing else exists).
fn local_addresses<'a>(ifaces: impl Iterator<Item = &'a Interface>) -> LocalAddresses {
    let (mut loopback, mut regular4, mut regular6, mut linklocal4, mut ula6) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for iface in ifaces.filter(|i| i.is_up()) {
        let if_loop = iface.flags & IFF_LOOPBACK != 0;
        for net in &iface.addrs {
            let ip = net.addr().to_canonical();
            if ip.is_loopback() || if_loop {
                loopback.push(ip);
            } else if is_link_local(ip) {
                if ip.is_ipv4() {
                    linklocal4.push(ip);
                }
            } else if ip.is_ipv6() && is_private(&ip) {
                ula6.push(ip);
            } else if ip.is_ipv4() {
                regular4.push(ip);
            } else {
                regular6.push(ip);
            }
        }
    }
    if regular4.is_empty() && regular6.is_empty() {
        regular4 = linklocal4;
        regular6 = ula6;
    }
    let mut regular = regular4;
    regular.extend(regular6);
    regular.sort();
    loopback.sort();
    LocalAddresses { loopback, regular }
}

/// Likely residential router. Always absent on iOS: see the module comment.
#[derive(Debug, Clone)]
pub struct HomeRouter {
    /// Router address.
    pub gateway: IpAddr,
    /// Our address on that network.
    pub my_ip: Option<IpAddr>,
}

impl HomeRouter {
    /// Always `None` on iOS.
    pub fn new() -> Option<Self> {
        None
    }
}

// ---- getifaddrs ----

/// Copies a `sockaddr` into a zeroed `T` honouring its `sa_len`: BSD trims trailing zero bytes of netmasks, so reading a whole
/// `sockaddr_in6` could run past the allocation.
///
/// # Safety
/// `sa` must point to a valid `sockaddr` whose `sa_len` bytes are readable, and `T` must be a plain-old-data sockaddr type.
unsafe fn read_sockaddr<T>(sa: *const libc::sockaddr) -> T {
    let mut out: T = unsafe { std::mem::zeroed() };
    let len = (unsafe { (*sa).sa_len } as usize).min(std::mem::size_of::<T>());
    unsafe { std::ptr::copy_nonoverlapping(sa as *const u8, &mut out as *mut T as *mut u8, len) };
    out
}

fn list_interfaces() -> Vec<Interface> {
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `head` with a list we free below with freeifaddrs.
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        tracing::debug!("getifaddrs failed: {}", std::io::Error::last_os_error());
        return Vec::new();
    }
    let mut by_name: HashMap<String, Interface> = HashMap::new();
    let mut cur = head;
    while !cur.is_null() {
        // SAFETY: `cur` is a node of the list returned by getifaddrs, valid until freeifaddrs.
        let ifa = unsafe { &*cur };
        cur = ifa.ifa_next;
        // SAFETY: ifa_name is a NUL-terminated string.
        let name = unsafe { CStr::from_ptr(ifa.ifa_name) }
            .to_string_lossy()
            .into_owned();
        let entry = by_name.entry(name.clone()).or_insert_with(|| Interface {
            // SAFETY: `ifa_name` is valid and NUL-terminated.
            index: unsafe { libc::if_nametoindex(ifa.ifa_name) },
            name,
            flags: ifa.ifa_flags,
            mac_addr: None,
            addrs: Vec::new(),
        });
        if ifa.ifa_addr.is_null() {
            continue;
        }
        // SAFETY: ifa_addr is non-null and carries sa_family and sa_len.
        let family = unsafe { (*ifa.ifa_addr).sa_family } as i32;
        match family {
            libc::AF_INET => {
                // SAFETY: see read_sockaddr; family says the address is a sockaddr_in.
                let a: libc::sockaddr_in = unsafe { read_sockaddr(ifa.ifa_addr) };
                let prefix = if ifa.ifa_netmask.is_null() {
                    32
                } else {
                    // SAFETY: as above, for the netmask.
                    let m: libc::sockaddr_in = unsafe { read_sockaddr(ifa.ifa_netmask) };
                    m.sin_addr.s_addr.count_ones() as u8
                };
                let ip = Ipv4Addr::from(u32::from_be(a.sin_addr.s_addr));
                if let Ok(net) = Ipv4Net::new(ip, prefix) {
                    entry.addrs.push(IpNet::V4(net));
                }
            }
            libc::AF_INET6 => {
                // SAFETY: as above, family is AF_INET6.
                let a: libc::sockaddr_in6 = unsafe { read_sockaddr(ifa.ifa_addr) };
                let prefix = if ifa.ifa_netmask.is_null() {
                    128
                } else {
                    // SAFETY: as above, for the netmask.
                    let m: libc::sockaddr_in6 = unsafe { read_sockaddr(ifa.ifa_netmask) };
                    m.sin6_addr
                        .s6_addr
                        .iter()
                        .map(|b| b.count_ones())
                        .sum::<u32>() as u8
                };
                let mut octets = a.sin6_addr.s6_addr;
                let mut scope_id = a.sin6_scope_id;
                // The BSD kernel embeds the zone of a link-local address in bytes 2 and 3; hand out the plain address.
                if octets[0] == 0xfe && octets[1] & 0xc0 == 0x80 {
                    if scope_id == 0 {
                        scope_id = u32::from(u16::from_be_bytes([octets[2], octets[3]]));
                    }
                    octets[2] = 0;
                    octets[3] = 0;
                }
                if let Ok(net) = Ipv6Net::new(Ipv6Addr::from(octets), prefix) {
                    entry.addrs.push(IpNet::V6 {
                        net,
                        scope_id,
                        flags: Ipv6AddrFlags::default(),
                    });
                }
            }
            libc::AF_LINK => {
                // SAFETY: family is AF_LINK, so the address is a sockaddr_dl; the link-layer address follows the name.
                let dl = unsafe { &*(ifa.ifa_addr as *const libc::sockaddr_dl) };
                if dl.sdl_alen == 6 {
                    let start = dl.sdl_nlen as usize;
                    let mut mac = [0u8; 6];
                    let data = dl.sdl_data.as_ptr();
                    for (i, b) in mac.iter_mut().enumerate() {
                        // SAFETY: sdl_data is declared 12 bytes but the sockaddr_dl is sa_len long and holds name + address; a
                        // name such as "pdp_ip0" puts the MAC past the declared array, so index through the raw pointer.
                        *b = unsafe { *data.add(start + i) } as u8;
                    }
                    entry.mac_addr = Some(mac);
                }
            }
            _ => {}
        }
    }
    // SAFETY: `head` came from getifaddrs and nothing borrows it any more.
    unsafe { libc::freeifaddrs(head) };

    // Sorted so that successive snapshots of an unchanged network compare equal.
    let mut out: Vec<Interface> = by_name.into_values().collect();
    for iface in &mut out {
        iface
            .addrs
            .sort_by(|a, b| (a.addr().is_ipv6(), a.addr()).cmp(&(b.addr().is_ipv6(), b.addr())));
    }
    out.sort_by_key(|i| i.index);
    out
}

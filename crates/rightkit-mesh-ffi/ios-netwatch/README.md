# ios-netwatch

RightKit's own implementation of the slice of `netwatch` 0.19.3 that iroh 1.3.0, iroh-relay and portmapper use, for the phone
build (`rightkit-mesh-ffi`, handed over from Pulse #197, rightkit #786). The FFI workspace wires it with `[patch.crates-io] netwatch = { path = "ios-netwatch" }`
(same name and version, so the patch matches); the desktop workspace never sees it.

## Why

Upstream `netwatch` needs `netdev` and a BSD routing socket (`net/route.h`), neither of which builds for `aarch64-apple-ios`
(libc has no `in6_ifreq` or route constants there). iroh uses a small part of the crate, so RightKit writes that part (about a
tenth of upstream) instead of carrying patched copies of `netwatch` and `netdev` or waiting on upstream (Adrian, 2026-10-18).
No donor tree is vendored; there is no dependency on `netdev`.

## Surface and call sites

| Item | Used by |
| --- | --- |
| `UdpSocket::{bind_full, local_addr, rebind, poll_recv_noq, max_gso_segments, gro_segments, may_fragment, create_sender}`, `UdpSender::poll_send` | iroh `socket/transports/ip.rs` |
| `UdpSocket::{bind_full, connect, send, recv}` | portmapper `nat_pmp.rs`, `pcp.rs` |
| `netmon::{Monitor::new, interface_state, network_change, State}` | iroh `socket.rs`, `net_report/reportgen.rs` |
| `interfaces::{State, Interface, IpNet, Ipv6AddrFlags, HomeRouter}`, `ip::LocalAddresses` | iroh `socket.rs`, `relay/actor.rs`; portmapper `lib.rs` |

## Behaviour on iOS

- Interfaces come from `getifaddrs`. There is no routing table, so the default-route interface is a guess (`en0`, then
  `pdp_ip0`, then any usable interface) and `HomeRouter::new()` is `None` (port mapping reports no gateway).
- No route monitor: `netmon::Monitor` re-reads interfaces every 3 s and on `network_change()`, and reports a suspension
  (tick gap) as `last_unsuspend`. The app should call `Endpoint::network_change` from its `NWPathMonitor` handler for an
  immediate reaction.
- IPv6 address flags (tentative, deprecated, ...) are not read; all are `false`.
- Sockets rebind on `ENOTCONN`/`EPIPE` and on iroh's `rebind` after a suspension. Buffers fall back from 7 MB down to 512 KB.

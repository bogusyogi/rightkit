//! Socket policy for the RK8 worker wire.
//!
//! The wire authenticates every frame but encrypts none, so bundle bytes,
//! step specs and observations are readable by anyone on the path. That is
//! acceptable on loopback, where the path is the local kernel. It is not
//! acceptable across a LAN or WAN. Until the wire runs inside TLS (rustls with
//! pinned certificates) or a Noise transport, these helpers refuse any
//! non-loopback address unless the caller passes [`Exposure::UnencryptedLan`]
//! explicitly. They also set connect and I/O timeouts, because the generic
//! `Read + Write` endpoints cannot bound a silent peer themselves.

use std::io;
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

/// Where a socket may reach.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Exposure {
    /// Loopback addresses only. The default.
    #[default]
    LoopbackOnly,
    /// Any address, with no confidentiality. Authenticity still holds. Never
    /// use this across an untrusted network.
    UnencryptedLan,
}

/// Default connect, read and write timeout.
pub const IO_TIMEOUT: Duration = Duration::from_secs(30);

/// Whether `ip` is a loopback address, including IPv4-mapped IPv6 loopback.
pub fn is_loopback(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => {
            v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
    }
}

/// Refuses `addr` unless it is loopback or `exposure` allows more.
pub fn check_addr(addr: &SocketAddr, exposure: Exposure) -> io::Result<()> {
    if exposure == Exposure::LoopbackOnly && !is_loopback(&addr.ip()) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{addr} is not loopback: the worker wire is unencrypted; use TLS or Noise, or opt in with Exposure::UnencryptedLan"
            ),
        ));
    }
    Ok(())
}

/// Binds a worker listener, refusing non-loopback addresses by default.
pub fn bind(addr: SocketAddr, exposure: Exposure) -> io::Result<TcpListener> {
    check_addr(&addr, exposure)?;
    TcpListener::bind(addr)
}

/// Accepts the next connection, applying timeouts and refusing a non-loopback
/// peer under [`Exposure::LoopbackOnly`].
pub fn accept(listener: &TcpListener, exposure: Exposure) -> io::Result<TcpStream> {
    let (stream, peer) = listener.accept()?;
    check_addr(&peer, exposure)?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    Ok(stream)
}

/// Dials a worker, refusing non-loopback addresses by default and applying
/// connect, read and write timeouts.
pub fn connect(addr: SocketAddr, exposure: Exposure) -> io::Result<TcpStream> {
    check_addr(&addr, exposure)?;
    let stream = TcpStream::connect_timeout(&addr, IO_TIMEOUT)?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    stream.set_nodelay(true)?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_loopback_is_refused_unless_opted_in() {
        let lan: SocketAddr = "192.168.1.20:7000".parse().unwrap();
        let wan: SocketAddr = "[2001:db8::1]:7000".parse().unwrap();
        let any: SocketAddr = "0.0.0.0:7000".parse().unwrap();
        for addr in [lan, wan, any] {
            assert!(check_addr(&addr, Exposure::LoopbackOnly).is_err());
            assert!(bind(addr, Exposure::LoopbackOnly).is_err());
            assert!(connect(addr, Exposure::LoopbackOnly).is_err());
            assert!(check_addr(&addr, Exposure::UnencryptedLan).is_ok());
        }
    }

    #[test]
    fn loopback_forms_are_allowed() {
        for text in [
            "127.0.0.1:1",
            "127.9.9.9:1",
            "[::1]:1",
            "[::ffff:127.0.0.1]:1",
        ] {
            let addr: SocketAddr = text.parse().unwrap();
            assert!(check_addr(&addr, Exposure::LoopbackOnly).is_ok(), "{text}");
        }
    }

    #[test]
    fn a_loopback_listener_accepts_and_dials_with_timeouts() {
        let listener = bind("127.0.0.1:0".parse().unwrap(), Exposure::LoopbackOnly).unwrap();
        let addr = listener.local_addr().unwrap();
        let client = connect(addr, Exposure::LoopbackOnly).unwrap();
        let server = accept(&listener, Exposure::LoopbackOnly).unwrap();
        assert_eq!(client.read_timeout().unwrap(), Some(IO_TIMEOUT));
        assert_eq!(server.write_timeout().unwrap(), Some(IO_TIMEOUT));
    }
}

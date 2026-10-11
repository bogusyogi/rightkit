//! A loopback port that refuses connections and that no other test server can be given.
//!
//! Binding a listener on port 0, reading its port and dropping it does not give this: the
//! system hands port-0 requests out of its dynamic range, so the freed port can go to the next
//! test server that binds port 0, and the "refused" request then gets an answer (seen on the
//! Windows CI runner).
//!
//! * Windows: the port is held. A socket is bound with exclusive address use and never
//!   listens, until the value drops; a connect to it is refused.
//! * Unix: the port cannot be held that way. On macOS a connect to a bound socket that does
//!   not listen is not refused, it hangs until the timeout (seen on the macOS CI runner). So
//!   the port is taken from below every system's dynamic range (Linux starts at 32768, macOS
//!   at 49152), where a port-0 bind never lands, and it is checked before use: it can be bound
//!   here, so nothing has it, and a connect to it is refused.
#![allow(dead_code)]

pub struct RefusedPort {
    held: sys::Held,
    port: u16,
}

impl RefusedPort {
    /// A port on `127.0.0.1` that refuses connections for as long as the value lives. Panics
    /// if none can be had.
    pub fn hold() -> Self {
        let (held, port) = sys::refused_port().expect("a loopback port that refuses connections");
        assert_ne!(port, 0);
        Self { held, port }
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }
}

impl Drop for RefusedPort {
    fn drop(&mut self) {
        sys::release(&mut self.held);
    }
}

#[cfg(windows)]
mod sys {
    use std::io;

    pub type Held = usize;
    const INVALID_SOCKET: usize = usize::MAX;
    const AF_INET: i32 = 2;
    const SOCK_STREAM: i32 = 1;
    const IPPROTO_TCP: i32 = 6;
    const SOL_SOCKET: i32 = 0xffff;
    /// `SO_EXCLUSIVEADDRUSE`: no other socket may bind this address, `SO_REUSEADDR` or not.
    const SO_EXCLUSIVEADDRUSE: i32 = !4;

    #[repr(C)]
    struct SockaddrIn {
        family: u16,
        port: u16,
        addr: [u8; 4],
        zero: [u8; 8],
    }

    #[link(name = "ws2_32")]
    unsafe extern "system" {
        fn socket(family: i32, kind: i32, protocol: i32) -> usize;
        fn setsockopt(socket: usize, level: i32, name: i32, value: *const u8, len: i32) -> i32;
        fn bind(socket: usize, address: *const SockaddrIn, len: i32) -> i32;
        fn getsockname(socket: usize, address: *mut SockaddrIn, len: *mut i32) -> i32;
        fn closesocket(socket: usize) -> i32;
    }

    pub fn refused_port() -> io::Result<(Held, u16)> {
        // The standard library starts Winsock on its first socket; this one is dropped at once.
        drop(std::net::UdpSocket::bind("127.0.0.1:0")?);
        // SAFETY: plain Winsock calls on a socket this function owns; every pointer is to a live
        // local of the stated size.
        unsafe {
            let socket = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
            if socket == INVALID_SOCKET {
                return Err(io::Error::last_os_error());
            }
            let fail = |socket: usize| {
                let error = io::Error::last_os_error();
                closesocket(socket);
                Err(error)
            };
            let on = 1i32.to_ne_bytes();
            if setsockopt(socket, SOL_SOCKET, SO_EXCLUSIVEADDRUSE, on.as_ptr(), 4) != 0 {
                return fail(socket);
            }
            let mut address = SockaddrIn {
                family: AF_INET as u16,
                port: 0,
                addr: [127, 0, 0, 1],
                zero: [0; 8],
            };
            let mut len = std::mem::size_of::<SockaddrIn>() as i32;
            if bind(socket, &address, len) != 0 {
                return fail(socket);
            }
            if getsockname(socket, &mut address, &mut len) != 0 {
                return fail(socket);
            }
            Ok((socket, u16::from_be(address.port)))
        }
    }

    pub fn release(socket: &mut Held) {
        // SAFETY: the socket came from `refused_port` and is closed once.
        unsafe { closesocket(*socket) };
    }
}

#[cfg(unix)]
mod sys {
    use std::io::{self, ErrorKind};
    use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    /// Nothing is held on Unix; the port is one no port-0 bind is given.
    pub type Held = ();

    /// Candidate ports: 20000..30000, below every dynamic range and above the well-known ones.
    const FIRST: u32 = 20_000;
    const SPAN: u32 = 10_000;
    /// A refusal on loopback comes at once; this bounds a port that answers nothing.
    const CONNECT_WAIT: Duration = Duration::from_secs(2);

    pub fn refused_port() -> io::Result<(Held, u16)> {
        static CALLS: AtomicU32 = AtomicU32::new(0);
        // Different tests and different test processes start at different candidates.
        let start = std::process::id()
            .wrapping_mul(2_654_435_761)
            .wrapping_add(CALLS.fetch_add(1, Ordering::Relaxed).wrapping_mul(7_919));
        for step in 0..SPAN {
            let port = (FIRST + start.wrapping_add(step) % SPAN) as u16;
            let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
            // Free: nothing else is bound to it. The listener is dropped before the connect.
            if TcpListener::bind(address).is_err() {
                continue;
            }
            if let Err(error) = TcpStream::connect_timeout(&address, CONNECT_WAIT) {
                if error.kind() == ErrorKind::ConnectionRefused {
                    return Ok(((), port));
                }
            }
            // Something answered, or nothing did: not a port to assert a refusal on.
        }
        Err(io::Error::other("no refused loopback port in 20000..30000"))
    }

    pub fn release(_held: &mut Held) {}
}

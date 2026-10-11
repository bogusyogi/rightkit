//! A loopback port that refuses connections for as long as the value lives.
//!
//! The socket is bound and never listens. The port stays taken, so no other test or process
//! can be given it, and a connect to it is refused because nothing accepts there. Binding a
//! listener, reading its port and dropping it does not give this: the freed port can be handed
//! to the next server that binds port 0, and the "refused" request then gets an answer.
#![allow(dead_code)]

pub struct RefusedPort {
    socket: sys::Socket,
    port: u16,
}

impl RefusedPort {
    /// Bind `127.0.0.1` on a port the system picks and hold it. Panics if that fails.
    pub fn hold() -> Self {
        let (socket, port) =
            sys::bind_loopback().expect("bind a loopback socket that never listens");
        assert_ne!(port, 0);
        Self { socket, port }
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
        sys::close(self.socket);
    }
}

#[cfg(windows)]
mod sys {
    use std::io;

    pub type Socket = usize;
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

    pub fn bind_loopback() -> io::Result<(Socket, u16)> {
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

    pub fn close(socket: Socket) {
        // SAFETY: the socket came from `bind_loopback` and is closed once.
        unsafe { closesocket(socket) };
    }
}

#[cfg(unix)]
mod sys {
    use std::io;

    pub type Socket = libc::c_int;

    pub fn bind_loopback() -> io::Result<(Socket, u16)> {
        // SAFETY: plain socket calls on a descriptor this function owns; every pointer is to a
        // live local of the stated size.
        unsafe {
            let socket = libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0);
            if socket < 0 {
                return Err(io::Error::last_os_error());
            }
            let fail = |socket: libc::c_int| {
                let error = io::Error::last_os_error();
                libc::close(socket);
                Err(error)
            };
            let mut address: libc::sockaddr_in = std::mem::zeroed();
            address.sin_family = libc::AF_INET as libc::sa_family_t;
            address.sin_addr.s_addr = u32::from_ne_bytes([127, 0, 0, 1]);
            let mut len = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
            if libc::bind(socket, (&raw const address).cast(), len) != 0 {
                return fail(socket);
            }
            if libc::getsockname(socket, (&raw mut address).cast(), &mut len) != 0 {
                return fail(socket);
            }
            Ok((socket, u16::from_be(address.sin_port)))
        }
    }

    pub fn close(socket: Socket) {
        // SAFETY: the descriptor came from `bind_loopback` and is closed once.
        unsafe { libc::close(socket) };
    }
}

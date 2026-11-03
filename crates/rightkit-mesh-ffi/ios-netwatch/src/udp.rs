//! `netwatch::UdpSocket` and `UdpSender`: a tokio UDP socket with noq-udp's batched send/receive state, and a rebind for
//! when iOS invalidates sockets across a suspension.
//!
//! Surface (item -> iroh call site):
//! - `UdpSocket::bind_full`: iroh socket/transports/ip.rs `IpTransport::bind`; portmapper nat_pmp.rs and pcp.rs.
//! - `local_addr`: ip.rs (after bind and after rebind).
//! - `rebind`: ip.rs `IpNetworkChangeSender::rebind` (iroh calls it on a major network change).
//! - `poll_recv_noq`: ip.rs `poll_recv`, the QUIC receive path.
//! - `max_gso_segments`, `gro_segments`, `may_fragment`: ip.rs transport capabilities.
//! - `create_sender` and `UdpSender::poll_send`: ip.rs `IpSender::poll_send`, the QUIC send path.
//! - `connect`, `send`, `recv`: portmapper NAT-PMP and PCP request/response (unreachable on iOS: no `HomeRouter`).
//!
//! Each socket is bound to one address family (IPv6 sockets are v6-only): iroh binds a v4 and a v6 socket and maps v4-mapped
//! addresses back itself.

use std::{
    future::poll_fn,
    io,
    net::SocketAddr,
    num::NonZeroUsize,
    pin::Pin,
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
};

use atomic_waker::AtomicWaker;
use noq_udp::{Transmit, UdpSockRef, UdpSocketState};
use tokio::io::Interest;
use tracing::{debug, trace};

/// Receive and send buffer sizes tried in order; iOS clamps or refuses the larger ones and a refusal would leave the 9 KB
/// default, which drops QUIC bursts.
const BUFFER_SIZES: [usize; 5] = [7 << 20, 4 << 20, 2 << 20, 1 << 20, 512 << 10];

#[derive(Debug)]
enum Inner {
    Bound {
        socket: tokio::net::UdpSocket,
        state: UdpSocketState,
        addr: SocketAddr,
    },
    /// Dropped for a rebind that has not (yet) succeeded; the port to take again.
    Unbound { addr: SocketAddr },
}

/// UDP socket.
#[derive(Debug)]
pub struct UdpSocket {
    inner: RwLock<Inner>,
    recv_waker: AtomicWaker,
    send_waker: AtomicWaker,
    /// An error showed the OS socket is dead (iOS does this after suspension): rebind on next use.
    broken: AtomicBool,
}

impl UdpSocket {
    /// Binds `addr` exactly (port 0 picks one). A non-zero port that is not the one obtained is an error.
    pub fn bind_full(addr: impl Into<SocketAddr>) -> io::Result<Self> {
        Ok(UdpSocket {
            inner: RwLock::new(bind(addr.into())?),
            recv_waker: AtomicWaker::new(),
            send_waker: AtomicWaker::new(),
            broken: AtomicBool::new(false),
        })
    }

    /// Local address.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.with_bound(|s, _| s.local_addr())?
    }

    /// Closes and binds again on the same address (port).
    pub fn rebind(&self) -> io::Result<()> {
        {
            let mut guard = self.write();
            rebind_inner(&mut guard).inspect_err(|_| self.broken.store(true, Ordering::Release))?;
            self.broken.store(false, Ordering::Release);
        }
        self.recv_waker.wake();
        self.send_waker.wake();
        Ok(())
    }

    /// Connects to `addr`: default destination for `send`, filter for `recv`.
    pub fn connect(&self, addr: SocketAddr) -> io::Result<()> {
        self.with_bound(|s, _| socket2::SockRef::from(s).connect(&socket2::SockAddr::from(addr)))?
    }

    /// Sends to the connected peer.
    pub async fn send(&self, buf: &[u8]) -> io::Result<usize> {
        poll_fn(|cx| self.poll_io(cx, false, |s| s.try_send(buf))).await
    }

    /// Receives from the connected peer.
    pub async fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        poll_fn(|cx| self.poll_io(cx, true, |s| s.try_recv(&mut *buf))).await
    }

    /// Receive for noq: fills `bufs` and `meta`, returns the datagram count.
    pub fn poll_recv_noq(
        &self,
        cx: &mut Context<'_>,
        bufs: &mut [io::IoSliceMut<'_>],
        meta: &mut [noq_udp::RecvMeta],
    ) -> Poll<io::Result<usize>> {
        loop {
            if let Err(e) = self.maybe_rebind() {
                return Poll::Ready(Err(e));
            }
            self.recv_waker.register(cx.waker());
            let step = self.with_bound(|s, st| match s.poll_recv_ready(cx) {
                Poll::Pending => Ok(None),
                Poll::Ready(Err(e)) => Err(e),
                Poll::Ready(Ok(())) => s
                    .try_io(Interest::READABLE, || st.recv(s.into(), bufs, meta))
                    .map(Some),
            });
            match step.and_then(|r| r) {
                Ok(Some(n)) => {
                    trace!(count = n, "UDP recv");
                    return Poll::Ready(Ok(n));
                }
                Ok(None) => return Poll::Pending,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue, // spurious wakeup, readiness was cleared
                Err(e) => match self.read_error(e) {
                    Some(e) => return Poll::Ready(Err(e)),
                    None => continue,
                },
            }
        }
    }

    /// Creates a sender for noq's send path.
    pub fn create_sender(self: Arc<Self>) -> UdpSender {
        UdpSender { socket: self }
    }

    /// Whether the IP layer may fragment our datagrams.
    pub fn may_fragment(&self) -> bool {
        self.with_bound(|_, st| st.may_fragment()).unwrap_or(true)
    }

    /// Segments per GSO send (1: iOS has no GSO).
    pub fn max_gso_segments(&self) -> NonZeroUsize {
        self.with_bound(|_, st| st.max_gso_segments())
            .unwrap_or(NonZeroUsize::MIN)
    }

    /// Segments per GRO receive (1: iOS has no GRO).
    pub fn gro_segments(&self) -> NonZeroUsize {
        self.with_bound(|_, st| st.gro_segments())
            .unwrap_or(NonZeroUsize::MIN)
    }

    // ---- internals ----

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Inner> {
        self.inner.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Inner> {
        self.inner.write().unwrap_or_else(|e| e.into_inner())
    }

    /// Runs `f` on the live socket. The lock is held only for the call (every use is a non-blocking syscall), so a rebind never
    /// waits on a task.
    fn with_bound<R>(
        &self,
        f: impl FnOnce(&tokio::net::UdpSocket, &UdpSocketState) -> R,
    ) -> io::Result<R> {
        match &*self.read() {
            Inner::Bound { socket, state, .. } => Ok(f(socket, state)),
            Inner::Unbound { .. } => {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "socket closed"))
            }
        }
    }

    fn maybe_rebind(&self) -> io::Result<()> {
        if !self.broken.load(Ordering::Acquire) {
            return Ok(());
        }
        let mut guard = self.write();
        if !self.broken.load(Ordering::Acquire) {
            return Ok(()); // another caller rebound while we waited
        }
        rebind_inner(&mut guard)?;
        self.broken.store(false, Ordering::Release);
        drop(guard);
        self.recv_waker.wake();
        self.send_waker.wake();
        Ok(())
    }

    /// `Some(err)` if fatal. A dead socket is queued for rebind; a reset (ICMP echo of an earlier send) is never fatal.
    fn read_error(&self, e: io::Error) -> Option<io::Error> {
        match e.kind() {
            io::ErrorKind::NotConnected | io::ErrorKind::BrokenPipe => {
                self.broken.store(true, Ordering::Release);
                None
            }
            io::ErrorKind::ConnectionReset => None,
            _ => Some(e),
        }
    }

    fn write_error(&self, e: io::Error) -> Option<io::Error> {
        match e.kind() {
            io::ErrorKind::BrokenPipe | io::ErrorKind::NotConnected => {
                self.broken.store(true, Ordering::Release);
                None
            }
            _ => Some(e),
        }
    }

    /// Readiness-then-try loop shared by `send`/`recv`.
    fn poll_io(
        &self,
        cx: &mut Context<'_>,
        recv: bool,
        mut op: impl FnMut(&tokio::net::UdpSocket) -> io::Result<usize>,
    ) -> Poll<io::Result<usize>> {
        loop {
            if let Err(e) = self.maybe_rebind() {
                return Poll::Ready(Err(e));
            }
            let (waker, interest) = if recv {
                (&self.recv_waker, Interest::READABLE)
            } else {
                (&self.send_waker, Interest::WRITABLE)
            };
            waker.register(cx.waker());
            let step = self.with_bound(|s, _| {
                let ready = if recv {
                    s.poll_recv_ready(cx)
                } else {
                    s.poll_send_ready(cx)
                };
                match ready {
                    Poll::Pending => Ok(None),
                    Poll::Ready(Err(e)) => Err(e),
                    Poll::Ready(Ok(())) => s.try_io(interest, || op(s)).map(Some),
                }
            });
            match step.and_then(|r| r) {
                Ok(Some(n)) => return Poll::Ready(Ok(n)),
                Ok(None) => return Poll::Pending,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                Err(e) => match if recv {
                    self.read_error(e)
                } else {
                    self.write_error(e)
                } {
                    Some(e) => return Poll::Ready(Err(e)),
                    None => continue,
                },
            }
        }
    }

    /// Send for noq (`UdpSender::poll_send`).
    fn poll_send_noq(&self, cx: &mut Context<'_>, transmit: &Transmit<'_>) -> Poll<io::Result<()>> {
        loop {
            if let Err(e) = self.maybe_rebind() {
                return Poll::Ready(Err(e));
            }
            self.send_waker.register(cx.waker());
            let step = self.with_bound(|s, st| match s.poll_send_ready(cx) {
                Poll::Pending => Ok(false),
                Poll::Ready(Err(e)) => Err(e),
                Poll::Ready(Ok(())) => s
                    .try_io(Interest::WRITABLE, || st.send(s.into(), transmit))
                    .map(|()| true),
            });
            match step.and_then(|r| r) {
                Ok(true) => return Poll::Ready(Ok(())),
                Ok(false) => return Poll::Pending,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                Err(e) => match self.write_error(e) {
                    Some(e) => return Poll::Ready(Err(e)),
                    None => continue,
                },
            }
        }
    }
}

fn bind(addr: SocketAddr) -> io::Result<Inner> {
    let domain = if addr.is_ipv4() {
        socket2::Domain::IPV4
    } else {
        socket2::Domain::IPV6
    };
    let socket = socket2::Socket::new(domain, socket2::Type::DGRAM, Some(socket2::Protocol::UDP))?;
    if let Some(size) = BUFFER_SIZES
        .iter()
        .copied()
        .find(|&n| socket.set_recv_buffer_size(n).is_ok())
    {
        debug!(size, "recv buffer");
    }
    if let Some(size) = BUFFER_SIZES
        .iter()
        .copied()
        .find(|&n| socket.set_send_buffer_size(n).is_ok())
    {
        debug!(size, "send buffer");
    }
    if addr.is_ipv6() {
        socket.set_only_v6(true)?;
    }
    // Bind before handing to noq-udp, otherwise local_addr is not yet available on all OSes.
    socket.bind(&addr.into())?;
    socket.set_nonblocking(true)?;
    let socket = tokio::net::UdpSocket::from_std(socket.into())?;
    let state = UdpSocketState::new(UdpSockRef::from(&socket))?;
    let local = socket.local_addr()?;
    if addr.port() != 0 && local.port() != addr.port() {
        return Err(io::Error::other(format!(
            "wrong port bound: wanted {} got {}",
            addr.port(),
            local.port()
        )));
    }
    Ok(Inner::Bound {
        socket,
        state,
        addr: local,
    })
}

/// Drops the old socket first so the port is free, then binds the same address. On failure it stays `Unbound` and the
/// caller keeps `broken` set so the next use retries.
fn rebind_inner(inner: &mut Inner) -> io::Result<()> {
    let addr = match inner {
        Inner::Bound { addr, .. } | Inner::Unbound { addr } => *addr,
    };
    debug!(%addr, "rebinding");
    *inner = Inner::Unbound { addr };
    *inner = bind(addr)?;
    Ok(())
}

/// Sender handed to noq. Clones share the socket.
#[derive(Debug, Clone)]
pub struct UdpSender {
    socket: Arc<UdpSocket>,
}

impl UdpSender {
    /// Polls one transmit to completion.
    pub fn poll_send(
        self: Pin<&mut Self>,
        transmit: &Transmit<'_>,
        cx: &mut Context<'_>,
    ) -> Poll<io::Result<()>> {
        self.socket.poll_send_noq(cx, transmit)
    }
}

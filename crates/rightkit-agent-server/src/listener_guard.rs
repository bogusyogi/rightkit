//! Limits on what a listener accepts: a cap on open connections and a deadline for the first
//! request head. The LAN listener serves through [`Guarded`], so a
//! client that opens many sockets, or trickles one header byte at a time, cannot hold the daemon.

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::Sleep;

/// How long a new connection may take to deliver its first complete request head.
pub(crate) const HEAD_TIMEOUT: Duration = Duration::from_secs(30);
/// Open connections on the LAN listener (guest links only).
pub(crate) const LAN_MAX_CONNECTIONS: usize = 128;

/// A listener that holds at most `max` open connections and closes one that has not sent a full
/// request head within `head_timeout` of being accepted.
pub(crate) struct Guarded {
    listener: TcpListener,
    slots: Arc<Semaphore>,
    head_timeout: Duration,
}

impl Guarded {
    pub(crate) fn new(listener: TcpListener, max: usize, head_timeout: Duration) -> Self {
        Self {
            listener,
            slots: Arc::new(Semaphore::new(max)),
            head_timeout,
        }
    }
}

impl axum::serve::Listener for Guarded {
    type Io = GuardedIo;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (GuardedIo, SocketAddr) {
        // Wait for a free slot before taking the next connection; the rest queue in the kernel.
        let permit = self
            .slots
            .clone()
            .acquire_owned()
            .await
            .expect("the connection semaphore is never closed");
        let (stream, addr) = axum::serve::Listener::accept(&mut self.listener).await;
        (GuardedIo::new(stream, permit, self.head_timeout), addr)
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }
}

/// Waits for the blank line that ends the first request head.
struct HeadWatch {
    deadline: Pin<Box<Sleep>>,
    tail: Vec<u8>,
}

impl HeadWatch {
    /// True once `\r\n\r\n` has been seen across everything read so far.
    fn finished_by(&mut self, fresh: &[u8]) -> bool {
        self.tail.extend_from_slice(fresh);
        if self.tail.windows(4).any(|window| window == b"\r\n\r\n") {
            return true;
        }
        let keep = self.tail.len().saturating_sub(3);
        self.tail.drain(..keep);
        false
    }
}

pub(crate) struct GuardedIo {
    inner: TcpStream,
    _permit: OwnedSemaphorePermit,
    head: Option<HeadWatch>,
}

impl GuardedIo {
    fn new(inner: TcpStream, permit: OwnedSemaphorePermit, head_timeout: Duration) -> Self {
        Self {
            inner,
            _permit: permit,
            head: Some(HeadWatch {
                deadline: Box::pin(tokio::time::sleep(head_timeout)),
                tail: Vec::new(),
            }),
        }
    }
}

impl AsyncRead for GuardedIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = &mut *self;
        if let Some(watch) = this.head.as_mut() {
            if watch.deadline.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "request head not received in time",
                )));
            }
        }
        let before = buf.filled().len();
        let polled = Pin::new(&mut this.inner).poll_read(cx, buf);
        if let (Poll::Ready(Ok(())), Some(watch)) = (&polled, this.head.as_mut()) {
            if watch.finished_by(&buf.filled()[before..]) {
                this.head = None;
            }
        }
        polled
    }
}

impl AsyncWrite for GuardedIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, data)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn a_connection_that_never_finishes_its_head_is_closed() {
        let tcp = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = tcp.local_addr().unwrap();
        let guarded = Guarded::new(tcp, 4, Duration::from_millis(300));
        let app = axum::Router::new().route("/", axum::routing::get(|| async { "ok" }));
        tokio::spawn(async move { axum::serve(guarded, app).await });

        let mut slow = TcpStream::connect(addr).await.unwrap();
        slow.write_all(b"GET / HT").await.unwrap();
        let mut sink = Vec::new();
        let closed =
            tokio::time::timeout(Duration::from_secs(5), slow.read_to_end(&mut sink)).await;
        assert!(
            closed.is_ok(),
            "the half-sent request head was never cut off"
        );

        let mut fine = TcpStream::connect(addr).await.unwrap();
        fine.write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut reply = String::new();
        fine.read_to_string(&mut reply).await.unwrap();
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    }
}

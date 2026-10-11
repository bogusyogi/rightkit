//! Blocking client only: observe the response head's `Content-Encoding` on the wire.
//!
//! ureq decodes `gzip` (and `br`) bodies transparently when its own `gzip` feature is
//! compiled in, and it then removes `Content-Encoding` and `Content-Length` from the headers
//! it hands back. There is no switch to turn that off, and any crate in the build graph can
//! turn the feature on. A download needs encoded-byte accounting, so this transport wrapper
//! reads the head from the connection's input buffer, before ureq parses and strips it, and
//! leaves the encoding the server really sent in a thread-local that the caller reads after
//! `Agent::run`. ureq runs synchronously on the calling thread, so the thread-local belongs
//! to that call.

use std::cell::RefCell;

use ureq::unversioned::transport::{
    Buffers, ConnectionDetails, Connector, DefaultConnector, NextTimeout, Transport,
};
use ureq::Error;

thread_local! {
    static HEAD: RefCell<Option<Option<String>>> = const { RefCell::new(None) };
}

/// Forget any previous observation. Call before `Agent::run`.
pub(crate) fn clear() {
    HEAD.with(|h| *h.borrow_mut() = None);
}

/// `Some(encoding)` when a final response head was seen and carried `Content-Encoding`;
/// `None` when it carried none or no head was observed.
pub(crate) fn take_content_encoding() -> Option<String> {
    HEAD.with(|h| h.borrow_mut().take()).flatten()
}

/// Parse the first complete response head in `input`. Returns `None` until the head is
/// complete, `Some(None)` for an interim (1xx) head, and `Some(Some(encoding))` otherwise.
pub(crate) fn parse_head(input: &[u8]) -> Option<Option<Option<String>>> {
    if !input.starts_with(b"HTTP/") {
        return None;
    }
    let end = input.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&input[..end]);
    let mut lines = head.split("\r\n");
    let status = lines.next()?.split_whitespace().nth(1)?;
    if status.starts_with('1') {
        return Some(None);
    }
    let encoding = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("content-encoding"))
        .map(|(_, v)| v.trim().to_string());
    Some(Some(encoding))
}

#[derive(Debug)]
pub(crate) struct SniffConnector(pub(crate) DefaultConnector);

impl Connector<()> for SniffConnector {
    type Out = SniffTransport;

    fn connect(
        &self,
        details: &ConnectionDetails,
        chained: Option<()>,
    ) -> Result<Option<Self::Out>, Error> {
        Ok(self.0.connect(details, chained)?.map(|inner| SniffTransport {
            inner,
            awaiting_head: true,
        }))
    }
}

#[derive(Debug)]
pub(crate) struct SniffTransport {
    inner: Box<dyn Transport>,
    awaiting_head: bool,
}

impl SniffTransport {
    fn sniff(&mut self) {
        if !self.awaiting_head {
            return;
        }
        match parse_head(self.inner.buffers().input()) {
            Some(Some(encoding)) => {
                self.awaiting_head = false;
                HEAD.with(|h| *h.borrow_mut() = Some(encoding));
            }
            // An interim head: the final one follows.
            Some(None) | None => {}
        }
    }
}

impl Transport for SniffTransport {
    fn buffers(&mut self) -> &mut dyn Buffers {
        self.inner.buffers()
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), Error> {
        // A new request on this (possibly reused) connection: its head is next.
        self.awaiting_head = true;
        self.inner.transmit_output(amount, timeout)
    }

    fn maybe_await_input(&mut self, timeout: NextTimeout) -> Result<bool, Error> {
        let r = if self.inner.buffers().can_use_input() {
            Ok(true)
        } else {
            self.inner.await_input(timeout)
        };
        self.sniff();
        r
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, Error> {
        let r = self.inner.await_input(timeout);
        self.sniff();
        r
    }

    fn is_open(&mut self) -> bool {
        self.inner.is_open()
    }

    fn is_tls(&self) -> bool {
        self.inner.is_tls()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_parsing() {
        assert_eq!(parse_head(b"HTTP/1.1 200 OK\r\nContent-Enc"), None);
        assert_eq!(
            parse_head(b"HTTP/1.1 200 OK\r\nX: y\r\n\r\nbody"),
            Some(Some(None))
        );
        assert_eq!(
            parse_head(b"HTTP/1.1 200 OK\r\nCONTENT-encoding:  gzip \r\n\r\n"),
            Some(Some(Some("gzip".into())))
        );
        assert_eq!(parse_head(b"HTTP/1.1 100 Continue\r\n\r\n"), Some(None));
        // Body bytes that look like a head are not a head: the caller stops looking after
        // the first final one until the next request is written.
        assert_eq!(parse_head(b"not http"), None);
    }
}

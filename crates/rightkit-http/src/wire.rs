//! Blocking client only: observe the response head's `Content-Encoding` on the wire.
//!
//! ureq decodes `gzip` (and `br`) bodies transparently when its own `gzip` feature is
//! compiled in, and it then removes `Content-Encoding` and `Content-Length` from the headers
//! it hands back. There is no switch to turn that off, and any crate in the build graph can
//! turn the feature on. A download needs encoded-byte accounting, so this transport wrapper
//! reads the head from the connection's input buffer, before ureq parses and strips it, and
//! leaves what it saw in a thread-local that the caller reads after `Agent::run`. ureq runs
//! synchronously on the calling thread, so the thread-local belongs to that call.
//!
//! The result keeps two different facts apart: "a head was observed and it carried no
//! `Content-Encoding`" ([`WireHead::Observed`] with `None`) and "no head was observed at all"
//! ([`WireHead::Unobserved`]). Only the first says the body is not encoded. A download refuses
//! the second, because an unseen head may be exactly the encoded one that ureq then decoded.
//!
//! The head syntax follows what ureq's parser (httparse) accepts, not the strict form: leading
//! blank lines before the status line, and a bare `\n` as a line ending, are both skipped or
//! accepted by the transport, so they are here too.

use std::cell::RefCell;

use ureq::unversioned::transport::{
    Buffers, ConnectionDetails, Connector, DefaultConnector, NextTimeout, Transport,
};
use ureq::Error;

/// What the wrapper saw of the final response head of one `Agent::run`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum WireHead {
    /// No final head was seen, or its syntax was not recognised. This says nothing about
    /// `Content-Encoding`: never read it as "not encoded".
    #[default]
    Unobserved,
    /// A final head was seen. `content_encoding` is every `Content-Encoding` value in it,
    /// joined with `", "`, or `None` when the head carried none.
    Observed { content_encoding: Option<String> },
}

thread_local! {
    static HEAD: RefCell<WireHead> = const { RefCell::new(WireHead::Unobserved) };
}

/// Forget any previous observation. Call before `Agent::run`.
pub(crate) fn clear() {
    HEAD.with(|h| *h.borrow_mut() = WireHead::Unobserved);
}

/// What was observed since the last [`clear`]; resets the observation.
pub(crate) fn take_head() -> WireHead {
    HEAD.with(|h| std::mem::take(&mut *h.borrow_mut()))
}

/// Result of parsing the bytes at the front of the input buffer.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Parsed {
    /// More bytes are needed, or the bytes are not a response head.
    Incomplete,
    /// Only interim (1xx) heads so far; the final one follows.
    Interim,
    /// A complete final head.
    Final { content_encoding: Option<String> },
}

/// One head parsed from the front of the input.
enum One {
    Incomplete,
    /// A complete interim head; the value is the number of bytes it occupies (including any
    /// blank lines before it).
    Interim(usize),
    Final(Option<String>),
}

/// Parse the first complete final response head in `input`, looking past interim (1xx) heads
/// that are already in the buffer.
pub(crate) fn parse_head(input: &[u8]) -> Parsed {
    let mut offset = 0usize;
    let mut saw_interim = false;
    loop {
        match parse_one(&input[offset..]) {
            One::Final(content_encoding) => return Parsed::Final { content_encoding },
            One::Incomplete if saw_interim => return Parsed::Interim,
            One::Incomplete => return Parsed::Incomplete,
            // A complete interim head always occupies at least its status line, so this
            // advances.
            One::Interim(used) => {
                saw_interim = true;
                offset += used;
            }
        }
    }
}

fn parse_one(input: &[u8]) -> One {
    // Blank lines before the status line are skipped by the transport's parser.
    let mut start = 0usize;
    loop {
        let rest = &input[start..];
        if rest.starts_with(b"\r\n") {
            start += 2;
        } else if rest.starts_with(b"\n") {
            start += 1;
        } else {
            break;
        }
    }
    let rest = &input[start..];
    if !rest.starts_with(b"HTTP/") {
        return One::Incomplete;
    }
    let mut pos = 0usize;
    let mut interim: Option<bool> = None;
    let mut values: Vec<String> = Vec::new();
    while let Some(offset) = rest[pos..].iter().position(|b| *b == b'\n') {
        let mut line = &rest[pos..pos + offset];
        pos += offset + 1;
        if line.last() == Some(&b'\r') {
            line = &line[..line.len() - 1];
        }
        let text = String::from_utf8_lossy(line);
        match interim {
            None => {
                // `HTTP/1.1 200 OK`: the code is the second token.
                let Some(code) = text.split_whitespace().nth(1) else {
                    return One::Incomplete;
                };
                interim = Some(code.starts_with('1'));
            }
            Some(is_interim) => {
                if line.is_empty() {
                    return if is_interim {
                        One::Interim(start + pos)
                    } else {
                        One::Final((!values.is_empty()).then(|| values.join(", ")))
                    };
                }
                if let Some((name, value)) = text.split_once(':') {
                    if name.trim().eq_ignore_ascii_case("content-encoding") {
                        values.push(value.trim().to_string());
                    }
                }
            }
        }
    }
    One::Incomplete
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
        Ok(self
            .0
            .connect(details, chained)?
            .map(|inner| SniffTransport {
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
        // An interim head, or not all of the head yet, leaves `awaiting_head` set: look again
        // after the next read.
        if let Parsed::Final { content_encoding } = parse_head(self.inner.buffers().input()) {
            self.awaiting_head = false;
            HEAD.with(|h| *h.borrow_mut() = WireHead::Observed { content_encoding });
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

    fn fin(encoding: Option<&str>) -> Parsed {
        Parsed::Final {
            content_encoding: encoding.map(str::to_string),
        }
    }

    #[test]
    fn head_parsing() {
        assert_eq!(
            parse_head(b"HTTP/1.1 200 OK\r\nContent-Enc"),
            Parsed::Incomplete
        );
        assert_eq!(
            parse_head(b"HTTP/1.1 200 OK\r\nX: y\r\n\r\nbody"),
            fin(None)
        );
        assert_eq!(
            parse_head(b"HTTP/1.1 200 OK\r\nCONTENT-encoding:  gzip \r\n\r\n"),
            fin(Some("gzip"))
        );
        assert_eq!(
            parse_head(b"HTTP/1.1 100 Continue\r\n\r\n"),
            Parsed::Interim
        );
        // Body bytes that look like a head are not a head: the caller stops looking after
        // the first final one until the next request is written.
        assert_eq!(parse_head(b"not http"), Parsed::Incomplete);
        assert_eq!(parse_head(b""), Parsed::Incomplete);
    }

    /// The finding: the transport accepts blank lines before the status line, so the sniffer
    /// must too, or an encoded response reads as "no head seen".
    #[test]
    fn leading_blank_lines_do_not_hide_the_head() {
        assert_eq!(
            parse_head(b"\r\nHTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\n\r\n"),
            fin(Some("gzip"))
        );
        assert_eq!(
            parse_head(b"\r\n\r\n\nHTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\n\r\n"),
            fin(Some("gzip"))
        );
        assert_eq!(
            parse_head(b"\nHTTP/1.1 200 OK\nContent-Encoding: gzip\n\n"),
            fin(Some("gzip"))
        );
        // Blank lines alone, or a partial head after them, are not yet a head.
        assert_eq!(parse_head(b"\r\n\r\n"), Parsed::Incomplete);
        assert_eq!(
            parse_head(b"\r\nHTTP/1.1 200 OK\r\nContent-Encoding: gz"),
            Parsed::Incomplete
        );
    }

    #[test]
    fn bare_line_feeds_end_a_head() {
        assert_eq!(parse_head(b"HTTP/1.1 200 OK\nX: y\n\nbody"), fin(None));
        assert_eq!(
            parse_head(b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\n\r\nbody"),
            fin(Some("gzip"))
        );
    }

    #[test]
    fn every_content_encoding_line_is_reported() {
        assert_eq!(
            parse_head(
                b"HTTP/1.1 200 OK\r\nContent-Encoding: identity\r\ncontent-encoding: gzip\r\n\r\n"
            ),
            fin(Some("identity, gzip"))
        );
        // An empty value is reported as empty, not as absent.
        assert_eq!(
            parse_head(b"HTTP/1.1 200 OK\r\nContent-Encoding:\r\n\r\n"),
            fin(Some(""))
        );
    }

    #[test]
    fn an_interim_head_is_skipped_when_the_final_one_is_already_buffered() {
        assert_eq!(
            parse_head(
                b"HTTP/1.1 103 Early Hints\r\nLink: </a>\r\n\r\nHTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\n\r\n"
            ),
            fin(Some("gzip"))
        );
        assert_eq!(
            parse_head(b"HTTP/1.1 103 Early Hints\r\nLink: </a>\r\n\r\nHTTP/1.1 200 O"),
            Parsed::Interim
        );
        assert_eq!(
            parse_head(b"HTTP/1.1 100 Continue\r\n\r\n\r\nHTTP/1.1 200 OK\r\n\r\n"),
            fin(None)
        );
    }

    /// "Not seen" and "seen without an encoding" are different states.
    #[test]
    fn an_unobserved_head_is_not_an_absent_encoding() {
        clear();
        assert_eq!(take_head(), WireHead::Unobserved);
        HEAD.with(|h| {
            *h.borrow_mut() = WireHead::Observed {
                content_encoding: None,
            }
        });
        assert_eq!(
            take_head(),
            WireHead::Observed {
                content_encoding: None
            }
        );
        // Taking resets the observation.
        assert_eq!(take_head(), WireHead::Unobserved);
        assert_ne!(
            WireHead::Unobserved,
            WireHead::Observed {
                content_encoding: None
            }
        );
    }
}

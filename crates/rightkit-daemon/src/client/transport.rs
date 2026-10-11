//! One TLS connection per request over std sockets and rustls: HTTP/1.1,
//! `Connection: close`, `Content-Length` bodies. Only the roots the caller
//! supplied are trusted.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::ServerName;

use super::error::Error;

/// Largest header block accepted from the daemon.
const HEAD_MAX: usize = 64 * 1024;
/// Largest response body accepted (the daemon's largest reply is a small JSON object).
const BODY_MAX: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Reply {
    pub status: u16,
    /// Lower-case names.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Clone)]
pub(super) struct Transport {
    pub tls: Arc<rustls::ClientConfig>,
    pub addr: SocketAddr,
    pub server_name: ServerName<'static>,
    pub host_header: String,
    pub timeout: Duration,
}

fn tls_or_io(e: std::io::Error) -> Error {
    if e.get_ref()
        .is_some_and(|inner| inner.downcast_ref::<rustls::Error>().is_some())
    {
        Error::Tls(e.to_string())
    } else {
        Error::Io(e)
    }
}

fn clean(s: &str) -> bool {
    !s.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0)
}

impl Transport {
    pub fn exchange(
        &self,
        method: &str,
        path_and_query: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Result<Reply, Error> {
        if !path_and_query.starts_with('/') || !path_and_query.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err(Error::Config(format!(
                "request target must start with / and hold no spaces or control bytes: {path_and_query:?}"
            )));
        }
        if !method.bytes().all(|b| b.is_ascii_uppercase()) || method.is_empty() {
            return Err(Error::Config(format!("bad method {method:?}")));
        }
        if headers
            .iter()
            .any(|(k, v)| !clean(k) || !clean(v) || k.is_empty() || k.contains(':'))
        {
            return Err(Error::Config(
                "header name or value holds a line break".into(),
            ));
        }

        let mut sock = TcpStream::connect_timeout(&self.addr, self.timeout)?;
        sock.set_read_timeout(Some(self.timeout))?;
        sock.set_write_timeout(Some(self.timeout))?;
        sock.set_nodelay(true)?;
        let mut conn = rustls::ClientConnection::new(self.tls.clone(), self.server_name.clone())
            .map_err(|e| Error::Tls(e.to_string()))?;
        while conn.is_handshaking() {
            conn.complete_io(&mut sock).map_err(tls_or_io)?;
        }
        let mut stream = rustls::StreamOwned::new(conn, sock);

        let mut head = format!(
            "{method} {path_and_query} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nContent-Length: {}\r\n",
            self.host_header,
            body.len()
        );
        for (k, v) in headers {
            head.push_str(k);
            head.push_str(": ");
            head.push_str(v);
            head.push_str("\r\n");
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes()).map_err(tls_or_io)?;
        stream.write_all(body).map_err(tls_or_io)?;
        stream.flush().map_err(tls_or_io)?;
        read_reply(&mut stream)
    }
}

fn read_reply(stream: &mut impl Read) -> Result<Reply, Error> {
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        if buf.len() > HEAD_MAX {
            return Err(Error::Protocol("response header block too large".into()));
        }
        match stream.read(&mut chunk).map_err(tls_or_io)? {
            0 => {
                return Err(Error::Protocol(
                    "connection closed before the headers".into(),
                ))
            }
            n => buf.extend_from_slice(&chunk[..n]),
        }
    };
    let head = std::str::from_utf8(&buf[..head_end])
        .map_err(|_| Error::Protocol("response headers are not UTF-8".into()))?
        .to_string();
    let mut body: Vec<u8> = buf[head_end + 4..].to_vec();

    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or("");
    let mut parts = status_line.splitn(3, ' ');
    let status = match (
        parts.next(),
        parts.next().and_then(|s| s.parse::<u16>().ok()),
    ) {
        (Some(v), Some(s)) if v.starts_with("HTTP/1.") => s,
        _ => return Err(Error::Protocol(format!("bad status line {status_line:?}"))),
    };
    let mut headers = Vec::new();
    for line in lines {
        let Some((k, v)) = line.split_once(':') else {
            return Err(Error::Protocol(format!("bad header line {line:?}")));
        };
        headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
    }
    if headers.iter().any(|(k, _)| k == "transfer-encoding") {
        return Err(Error::Protocol(
            "chunked responses are not used by the daemon".into(),
        ));
    }
    let length = match headers.iter().find(|(k, _)| k == "content-length") {
        Some((_, v)) => Some(
            v.parse::<usize>()
                .map_err(|_| Error::Protocol(format!("bad content-length {v:?}")))?,
        ),
        None => None,
    };
    match length {
        Some(n) if n > BODY_MAX => return Err(Error::Protocol("response body too large".into())),
        Some(n) => {
            while body.len() < n {
                match stream.read(&mut chunk).map_err(tls_or_io)? {
                    0 => return Err(Error::Protocol("response body truncated".into())),
                    m => body.extend_from_slice(&chunk[..m]),
                }
            }
            body.truncate(n);
        }
        None if status == 204 || status == 304 || status < 200 => body.clear(),
        None => loop {
            if body.len() > BODY_MAX {
                return Err(Error::Protocol("response body too large".into()));
            }
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(m) => body.extend_from_slice(&chunk[..m]),
                // The peer closed without close_notify: the body ends here.
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(tls_or_io(e)),
            }
        },
    }
    Ok(Reply {
        status,
        headers,
        body,
    })
}

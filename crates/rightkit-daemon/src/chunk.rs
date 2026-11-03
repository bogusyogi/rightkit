//! Chunked calls: messages larger than one frame (issue #294).
//!
//! A frame is capped ([`DEFAULT_FRAME_LIMIT`](crate::DEFAULT_FRAME_LIMIT), 16 MiB) so one peer can
//! never make the other buffer an unbounded line. That cap stays. A message above it travels as
//! several frames, and the host assembles it under a second, explicit bound
//! ([`HostOptions::max_message`](crate::HostOptions::max_message)). Chunking is **off by default**
//! (`max_message` 0, issue #323): a service opts in with a size, and a host that has not refuses
//! `rk.upload`.
//!
//! **Request** ([`Client::call_large`](crate::Client::call_large)): the params are serialised to
//! JSON text and cut at character boundaries into pieces of `chunk_bytes` (the host advertises it
//! in `hello`). Each piece is an ordinary request to the reserved method [`METHOD_UPLOAD`]
//! (`{id, seq, last, text}`, and `method` on `seq` 0). The host appends the pieces in order (one
//! upload at a time per connection; a gap, a repeat or a changed id drops it), and on `last` parses
//! the assembled text and runs the real method through the normal pipeline: allowlist, handler,
//! events, response. The response to the final piece is the response to the call. A piece is
//! plain JSON text, so no encoding crate is needed; escaping at most doubles its wire size.
//!
//! **Response**: a client that opts in per request (`"chunked_ok": true`, which `call_large`
//! sets) gets a result that exceeds the frame limit as `{ok: true, result: null, chunked: {id,
//! bytes, count}}` instead of `response_too_large`; the client then reads the pieces with the
//! reserved method [`METHOD_FETCH`] (`{id, seq}` answered `{seq, text}`) and parses the join. The
//! host keeps one such result per connection (a new one replaces it; closing the connection drops
//! it). A client that does not opt in still gets the typed `response_too_large`.
//!
//! Events keep the frame limit: an event larger than it is dropped with
//! [`ServiceEvent::FrameTooLarge`](crate::ServiceEvent::FrameTooLarge) as before.
//!
//! The reserved methods are `rk.upload` and `rk.fetch`; no service method may start with `rk.`.
//! They need no allowlist entry (any app that passed `hello` may use them), because the call they
//! carry is authorised on its own method id: when the first piece arrives, before a byte is
//! buffered (an unauthorised peer is refused there with `not_allowed`), and again when the call is
//! dispatched. The assembled text is checked for nesting and value count before it is parsed.

use crate::error::{ServiceError, ERROR_BAD_CHUNK, ERROR_REQUEST_TOO_LARGE};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

/// Reserved method: one piece of a request too large for a frame.
pub const METHOD_UPLOAD: &str = "rk.upload";
/// Reserved method: one piece of a result too large for a frame.
pub const METHOD_FETCH: &str = "rk.fetch";
/// Prefix no service method may use.
pub const RESERVED_PREFIX: &str = "rk.";

/// Largest piece of text in one chunk frame (1 MiB; escaping at most doubles it on the wire).
pub const CHUNK_TEXT_BYTES: usize = 1024 * 1024;
/// Default for [`HostOptions::max_message`](crate::HostOptions::max_message): 0, chunking off
/// (it was 256 MiB earlier in the unpublished 0.2.7; see issue #323). A service that takes large calls opts in.
pub const DEFAULT_MAX_MESSAGE: usize = 0;
/// A reasonable opt-in value for a service that does take large calls: 64 MiB.
pub const SUGGESTED_MAX_MESSAGE: usize = 64 * 1024 * 1024;

/// The chunk size a host with `frame_limit` uses: a quarter of the frame (so a doubled escape
/// still fits), at most [`CHUNK_TEXT_BYTES`], at least 16 bytes.
pub(crate) fn chunk_bytes_for(frame_limit: usize) -> usize {
    (frame_limit / 4).clamp(16, CHUNK_TEXT_BYTES)
}

/// End offsets of consecutive pieces of `text`, each about `chunk` bytes and cut only at
/// character boundaries. Never empty: empty text is one empty piece.
pub(crate) fn boundaries(text: &str, chunk: usize) -> Vec<usize> {
    let chunk = chunk.max(4);
    let mut ends = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + chunk).min(text.len());
        // `text.len()` is always a boundary, so this ends.
        while !text.is_char_boundary(end) {
            end += 1;
        }
        ends.push(end);
        start = end;
    }
    if ends.is_empty() {
        ends.push(0);
    }
    ends
}

fn bad(message: impl Into<String>) -> ServiceError {
    ServiceError::new(ERROR_BAD_CHUNK, message)
}

/// Assemblies one connection may hold at once. A new first piece replaces the one in progress, so
/// the bytes a connection holds are at most one assembly, itself at most `max_message`.
pub const MAX_ASSEMBLIES_PER_CONNECTION: usize = 1;
/// An assembly that has not received a piece for this long is dropped (checked on every frame
/// the connection sends, and the whole state goes with the connection).
pub const UPLOAD_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// Deepest array/object nesting an assembled request may have (serde_json itself stops at 128).
pub const MAX_JSON_DEPTH: usize = 64;
/// Most values (elements, members and containers) an assembled request may contain. A parsed
/// `Value` costs 16 to 32 times the text of a dense array, so the count, not only the byte length,
/// bounds the memory a parse can take: 1 Mi values is about 32 to 64 MiB.
pub const MAX_JSON_NODES: usize = 1 << 20;

/// Refuse text whose nesting or value count is over the bounds, scanning it once without
/// allocating, so the cost of the real parse is bounded before it starts. Not a validator:
/// malformed text that passes here is rejected by the parse.
pub(crate) fn check_shape(text: &str) -> Result<(), ServiceError> {
    let (mut depth, mut nodes) = (0usize, 1usize);
    let (mut in_string, mut escaped) = (false, false);
    for &b in text.as_bytes() {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'[' | b'{' => {
                depth += 1;
                nodes += 1;
                if depth > MAX_JSON_DEPTH {
                    return Err(bad(format!(
                        "the assembled params nest deeper than {MAX_JSON_DEPTH} levels"
                    )));
                }
            }
            b']' | b'}' => depth = depth.saturating_sub(1),
            b',' => nodes += 1,
            _ => {}
        }
        if nodes > MAX_JSON_NODES {
            return Err(ServiceError::new(
                ERROR_REQUEST_TOO_LARGE,
                format!("the assembled params hold more than {MAX_JSON_NODES} values"),
            ));
        }
    }
    Ok(())
}

/// An upload in progress on one connection.
#[derive(Debug)]
struct Upload {
    id: String,
    method: String,
    next: u64,
    text: String,
    touched: Instant,
}

/// What one upload frame did.
#[derive(Debug, PartialEq)]
pub(crate) enum UploadStep {
    /// Stored; ask for the next piece.
    More { received: u64 },
    /// The last piece arrived: run `method` with `params`.
    Done { method: String, params: Value },
}

/// Per-connection upload state (at most one upload at a time).
#[derive(Debug, Default)]
pub(crate) struct Uploads {
    current: Option<Upload>,
}

impl Uploads {
    /// Drop an assembly that has been idle past [`UPLOAD_IDLE_TIMEOUT`].
    pub(crate) fn expire(&mut self) {
        if self
            .current
            .as_ref()
            .is_some_and(|u| u.touched.elapsed() > UPLOAD_IDLE_TIMEOUT)
        {
            self.current = None;
        }
    }

    /// Take one `rk.upload` frame's params. On any error the upload in progress is dropped, so a
    /// failed upload never leaves memory behind. `max` bounds the assembled text. `authorize`
    /// runs on the first piece, with the method it names, before anything is buffered: a refusal
    /// is the error returned, and no state is created.
    pub(crate) fn accept_with(
        &mut self,
        params: &Value,
        max: usize,
        authorize: &dyn Fn(&str) -> Result<(), ServiceError>,
    ) -> Result<UploadStep, ServiceError> {
        self.expire();
        let result = self.accept_inner(params, max, authorize);
        if result.is_err() {
            self.current = None;
        }
        result
    }

    fn accept_inner(
        &mut self,
        params: &Value,
        max: usize,
        authorize: &dyn Fn(&str) -> Result<(), ServiceError>,
    ) -> Result<UploadStep, ServiceError> {
        let id = params["id"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 64)
            .ok_or_else(|| bad("an upload piece needs an id of 1 to 64 characters"))?;
        let seq = params["seq"]
            .as_u64()
            .ok_or_else(|| bad("an upload piece needs a numeric seq"))?;
        let last = params["last"].as_bool().unwrap_or(false);
        let text = params["text"]
            .as_str()
            .ok_or_else(|| bad("an upload piece needs a text"))?;
        if seq == 0 {
            let method = params["method"]
                .as_str()
                .filter(|m| !m.is_empty() && !m.starts_with(RESERVED_PREFIX))
                .ok_or_else(|| bad("the first piece must name a method outside the rk. prefix"))?;
            // Authorise on the caller's own terms before a byte is kept; a replaced assembly is
            // freed either way.
            self.current = None;
            authorize(method)?;
            if text.len() > max {
                return Err(ServiceError::new(
                    ERROR_REQUEST_TOO_LARGE,
                    format!("the message is larger than the {max}-byte message limit"),
                ));
            }
            self.current = Some(Upload {
                id: id.to_string(),
                method: method.to_string(),
                next: 0,
                text: String::new(),
                touched: Instant::now(),
            });
        }
        let up = match self.current.as_mut() {
            Some(up) if up.id == id && up.next == seq => up,
            _ => {
                return Err(bad(
                    "upload piece out of order, repeated, or for another upload",
                ))
            }
        };
        if up.text.len().saturating_add(text.len()) > max {
            return Err(ServiceError::new(
                ERROR_REQUEST_TOO_LARGE,
                format!("the message is larger than the {max}-byte message limit"),
            ));
        }
        up.text.push_str(text);
        up.next += 1;
        up.touched = Instant::now();
        if !last {
            return Ok(UploadStep::More { received: seq });
        }
        let up = self.current.take().expect("the upload was just extended");
        // Bound the work before the parse: length is already under `max`; nesting and value
        // count are checked here, so no `Value` is built for a body that would amplify.
        check_shape(&up.text)?;
        let params = serde_json::from_str::<Value>(&up.text)
            .map_err(|e| bad(format!("the assembled params are not JSON: {e}")))?;
        Ok(UploadStep::Done {
            method: up.method,
            params,
        })
    }
}

/// A result held for the client to fetch in pieces.
#[derive(Debug)]
pub(crate) struct Download {
    id: String,
    text: String,
    ends: Vec<usize>,
}

impl Download {
    pub(crate) fn new(id: String, text: String, chunk: usize) -> Self {
        let ends = boundaries(&text, chunk);
        Self { id, text, ends }
    }

    /// The `chunked` field of the response that announces this result.
    pub(crate) fn descriptor(&self) -> Value {
        json!({"id": self.id, "bytes": self.text.len(), "count": self.ends.len()})
    }

    /// Answer one `rk.fetch` frame.
    pub(crate) fn fetch(&self, params: &Value) -> Result<Value, ServiceError> {
        if params["id"].as_str() != Some(self.id.as_str()) {
            return Err(bad(
                "no such result on this connection (it was replaced or released)",
            ));
        }
        let seq = params["seq"]
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n < self.ends.len())
            .ok_or_else(|| bad("fetch seq is out of range"))?;
        let start = if seq == 0 { 0 } else { self.ends[seq - 1] };
        Ok(json!({"seq": seq, "text": &self.text[start..self.ends[seq]]}))
    }
}

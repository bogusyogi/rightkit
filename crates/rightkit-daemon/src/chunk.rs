//! Chunked calls: messages larger than one frame (issue #294).
//!
//! A frame is capped ([`DEFAULT_FRAME_LIMIT`](crate::DEFAULT_FRAME_LIMIT), 16 MiB) so one peer can
//! never make the other buffer an unbounded line. That cap stays. A message above it travels as
//! several frames, and the host assembles it under a second, explicit bound
//! ([`HostOptions::max_message`](crate::HostOptions::max_message), 256 MiB by default).
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
//! carry is authorised on its own method id before any handler runs.

use crate::error::{ServiceError, ERROR_BAD_CHUNK, ERROR_REQUEST_TOO_LARGE};
use serde_json::{json, Value};

/// Reserved method: one piece of a request too large for a frame.
pub const METHOD_UPLOAD: &str = "rk.upload";
/// Reserved method: one piece of a result too large for a frame.
pub const METHOD_FETCH: &str = "rk.fetch";
/// Prefix no service method may use.
pub const RESERVED_PREFIX: &str = "rk.";

/// Largest piece of text in one chunk frame (1 MiB; escaping at most doubles it on the wire).
pub const CHUNK_TEXT_BYTES: usize = 1024 * 1024;
/// Default for [`HostOptions::max_message`](crate::HostOptions::max_message): 256 MiB.
pub const DEFAULT_MAX_MESSAGE: usize = 256 * 1024 * 1024;

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

/// An upload in progress on one connection.
#[derive(Debug)]
struct Upload {
    id: String,
    method: String,
    next: u64,
    text: String,
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
    /// Take one `rk.upload` frame's params. On any error the upload in progress is dropped, so a
    /// failed upload never leaves memory behind. `max` bounds the assembled text.
    pub(crate) fn accept(&mut self, params: &Value, max: usize) -> Result<UploadStep, ServiceError> {
        let result = self.accept_inner(params, max);
        if result.is_err() {
            self.current = None;
        }
        result
    }

    fn accept_inner(&mut self, params: &Value, max: usize) -> Result<UploadStep, ServiceError> {
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
            self.current = Some(Upload {
                id: id.to_string(),
                method: method.to_string(),
                next: 0,
                text: String::new(),
            });
        }
        let up = match self.current.as_mut() {
            Some(up) if up.id == id && up.next == seq => up,
            _ => return Err(bad("upload piece out of order, repeated, or for another upload")),
        };
        if up.text.len().saturating_add(text.len()) > max {
            return Err(ServiceError::new(
                ERROR_REQUEST_TOO_LARGE,
                format!("the message is larger than the {max}-byte message limit"),
            ));
        }
        up.text.push_str(text);
        up.next += 1;
        if !last {
            return Ok(UploadStep::More { received: seq });
        }
        let up = self.current.take().expect("the upload was just extended");
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
            return Err(bad("no such result on this connection (it was replaced or released)"));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn piece(id: &str, seq: u64, last: bool, text: &str, method: Option<&str>) -> Value {
        let mut p = json!({"id": id, "seq": seq, "last": last, "text": text});
        if let Some(m) = method {
            p["method"] = json!(m);
        }
        p
    }

    #[test]
    fn boundaries_cover_the_text_and_never_split_a_character() {
        let text = "aé€😀".repeat(50);
        for chunk in [4, 5, 7, 64, 1000] {
            let ends = boundaries(&text, chunk);
            assert_eq!(*ends.last().unwrap(), text.len());
            let mut start = 0;
            for end in &ends {
                assert!(text.is_char_boundary(*end));
                assert!(*end > start);
                start = *end;
            }
        }
        assert_eq!(boundaries("", 8), vec![0]);
    }

    #[test]
    fn an_upload_assembles_in_order_and_yields_the_params() {
        let whole = json!({"a": "x".repeat(100), "b": [1, 2, 3]});
        let text = serde_json::to_string(&whole).unwrap();
        let ends = boundaries(&text, 30);
        let mut up = Uploads::default();
        let mut start = 0;
        for (i, end) in ends.iter().enumerate() {
            let last = i + 1 == ends.len();
            let p = piece("u1", i as u64, last, &text[start..*end], (i == 0).then_some("m.big"));
            start = *end;
            match up.accept(&p, 1 << 20).unwrap() {
                UploadStep::More { received } => {
                    assert!(!last);
                    assert_eq!(received, i as u64);
                }
                UploadStep::Done { method, params } => {
                    assert!(last);
                    assert_eq!(method, "m.big");
                    assert_eq!(params, whole);
                }
            }
        }
    }

    #[test]
    fn a_gap_a_repeat_or_a_foreign_id_drops_the_upload() {
        let mut up = Uploads::default();
        up.accept(&piece("u1", 0, false, "{", Some("m")), 1000).unwrap();
        assert!(up.accept(&piece("u1", 2, false, "x", None), 1000).is_err());
        // Dropped: even the right next piece now fails.
        assert!(up.accept(&piece("u1", 1, false, "x", None), 1000).is_err());

        up.accept(&piece("u2", 0, false, "{", Some("m")), 1000).unwrap();
        assert!(up.accept(&piece("u2", 0, false, "{", None), 1000).is_err());
        up.accept(&piece("u3", 0, false, "{", Some("m")), 1000).unwrap();
        assert!(up.accept(&piece("other", 1, false, "x", None), 1000).is_err());
    }

    #[test]
    fn the_message_bound_and_the_reserved_prefix_are_enforced() {
        let mut up = Uploads::default();
        up.accept(&piece("u", 0, false, "12345", Some("m")), 8).unwrap();
        let err = up.accept(&piece("u", 1, false, "6789", None), 8).unwrap_err();
        assert_eq!(err.code, ERROR_REQUEST_TOO_LARGE);
        assert!(up.accept(&piece("v", 0, false, "{", Some("rk.fetch")), 8).is_err());
        assert!(up.accept(&piece("v", 0, false, "{", None), 8).is_err());
    }

    #[test]
    fn assembled_text_that_is_not_json_is_refused() {
        let mut up = Uploads::default();
        let err = up
            .accept(&piece("u", 0, true, "{not json", Some("m")), 1000)
            .unwrap_err();
        assert_eq!(err.code, ERROR_BAD_CHUNK);
    }

    #[test]
    fn a_download_serves_every_piece_and_rejects_strangers() {
        let text = "héllo wörld ".repeat(40);
        let dl = Download::new("d1".into(), text.clone(), 25);
        let desc = dl.descriptor();
        assert_eq!(desc["bytes"], text.len());
        let count = desc["count"].as_u64().unwrap();
        let mut joined = String::new();
        for seq in 0..count {
            let r = dl.fetch(&json!({"id": "d1", "seq": seq})).unwrap();
            joined.push_str(r["text"].as_str().unwrap());
        }
        assert_eq!(joined, text);
        assert!(dl.fetch(&json!({"id": "d1", "seq": count})).is_err());
        assert!(dl.fetch(&json!({"id": "d2", "seq": 0})).is_err());
    }

    #[test]
    fn the_chunk_size_scales_with_the_frame_limit() {
        assert_eq!(chunk_bytes_for(16 * 1024 * 1024), CHUNK_TEXT_BYTES);
        assert_eq!(chunk_bytes_for(4096), 1024);
        assert_eq!(chunk_bytes_for(10), 16);
    }
}

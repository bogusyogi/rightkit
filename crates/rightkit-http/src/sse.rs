//! Server-Sent Events decoder (WHATWG EventSource framing): LF, CR and CRLF line
//! endings, multi-line `data`, comments, `id`, `retry`, BOM, bounded line size.

use std::collections::VecDeque;
use std::io::Read;

use crate::error::HttpError;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SseEvent {
    /// Event type; "message" when the stream gave none.
    pub event: String,
    pub data: String,
    pub id: Option<String>,
    pub retry_ms: Option<u64>,
}

/// Push-based decoder: feed bytes, receive completed events.
#[derive(Debug)]
pub struct SseDecoder {
    line: Vec<u8>,
    prev_cr: bool,
    first: bool,
    event: String,
    data: Vec<String>,
    has_data: bool,
    id: Option<String>,
    retry_ms: Option<u64>,
    max_line: usize,
    max_event: usize,
    event_bytes: usize,
    /// Deliver each `data:` line as its own event when its newline arrives (see [`SseDecoder::eager`]).
    eager: bool,
}

impl Default for SseDecoder {
    fn default() -> Self {
        Self::new(1024 * 1024)
    }
}

impl SseDecoder {
    /// Decoder bounding each line; events are unbounded (use [`Self::with_limits`]).
    pub fn new(max_line_bytes: usize) -> Self {
        Self::with_limits(max_line_bytes, usize::MAX)
    }

    /// Decoder bounding each line and the accumulated bytes of one event
    /// (event name plus all data lines, one separator byte per line).
    pub fn with_limits(max_line_bytes: usize, max_event_bytes: usize) -> Self {
        Self {
            line: Vec::new(),
            prev_cr: false,
            first: true,
            event: String::new(),
            data: Vec::new(),
            has_data: false,
            id: None,
            retry_ms: None,
            max_line: max_line_bytes,
            max_event: max_event_bytes,
            event_bytes: 0,
            eager: false,
        }
    }

    /// Eager decoder with the default line bound. Every `data:` line becomes an event as soon
    /// as its newline arrives, carrying that line as its `data`, instead of waiting for the
    /// blank line that ends the frame. A provider whose daemon ends each frame with a newline
    /// rather than a blank line needs this; the WHATWG framing is the default.
    pub fn eager() -> Self {
        Self {
            eager: true,
            ..Self::default()
        }
    }

    /// Switch this decoder to eager mode (see [`SseDecoder::eager`]).
    pub fn with_eager(mut self) -> Self {
        self.eager = true;
        self
    }

    /// True when bytes or a frame are buffered that no event has delivered yet: a partial line,
    /// or `data` lines waiting for their blank line. A stream that ends while this is true was
    /// cut off mid-event.
    pub fn has_pending(&self) -> bool {
        !self.line.is_empty() || self.has_data
    }

    /// Flush at end of stream. A final line without its newline is processed, and a complete
    /// frame still waiting for its blank line is delivered, so a daemon that closes right after
    /// its last event does not lose it. Returns at most one event. Errors (invalid UTF-8 in the
    /// final line, for example) are reported, never dropped.
    pub fn finish(&mut self) -> Result<Option<SseEvent>, HttpError> {
        if !self.line.is_empty() {
            let line = std::mem::take(&mut self.line);
            if let Some(ev) = self.handle_line(&line)? {
                return Ok(Some(ev));
            }
        }
        Ok(self.dispatch())
    }

    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<SseEvent>, HttpError> {
        let mut out = Vec::new();
        for &b in bytes {
            if self.prev_cr {
                self.prev_cr = false;
                if b == b'\n' {
                    continue; // second half of CRLF
                }
            }
            match b {
                b'\n' | b'\r' => {
                    self.prev_cr = b == b'\r';
                    let line = std::mem::take(&mut self.line);
                    if let Some(ev) = self.handle_line(&line)? {
                        out.push(ev);
                    }
                }
                _ => {
                    self.line.push(b);
                    if self.line.len() > self.max_line {
                        return Err(HttpError::Stream("SSE line exceeds limit".into()));
                    }
                }
            }
        }
        Ok(out)
    }

    fn handle_line(&mut self, raw: &[u8]) -> Result<Option<SseEvent>, HttpError> {
        let mut line = raw;
        if self.first {
            self.first = false;
            if line.starts_with(&[0xEF, 0xBB, 0xBF]) {
                line = &line[3..];
            }
        }
        if line.is_empty() {
            return Ok(self.dispatch());
        }
        let text = std::str::from_utf8(line)
            .map_err(|_| HttpError::Stream("SSE line is not valid UTF-8".into()))?;
        if text.starts_with(':') {
            return Ok(None);
        }
        let (field, value) = match text.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (text, ""),
        };
        if matches!(field, "event" | "data") {
            self.event_bytes = self.event_bytes.saturating_add(value.len() + 1);
            if self.event_bytes > self.max_event {
                return Err(HttpError::Stream("SSE event exceeds limit".into()));
            }
        }
        match field {
            "event" => self.event = value.to_string(),
            "data" if self.eager => {
                self.event_bytes = 0;
                let event = if self.event.is_empty() {
                    "message".to_string()
                } else {
                    self.event.clone()
                };
                return Ok(Some(SseEvent {
                    event,
                    data: value.to_string(),
                    id: self.id.clone(),
                    retry_ms: self.retry_ms.take(),
                }));
            }
            "data" => {
                self.data.push(value.to_string());
                self.has_data = true;
            }
            "id" if !value.contains('\0') => self.id = Some(value.to_string()),
            "retry" => {
                if let Ok(ms) = value.parse::<u64>() {
                    self.retry_ms = Some(ms);
                }
            }
            _ => {}
        }
        Ok(None)
    }

    fn dispatch(&mut self) -> Option<SseEvent> {
        let had = self.has_data;
        self.event_bytes = 0;
        let event = std::mem::take(&mut self.event);
        let data = std::mem::take(&mut self.data);
        self.has_data = false;
        let retry_ms = self.retry_ms.take();
        if !had {
            return None;
        }
        Some(SseEvent {
            event: if event.is_empty() {
                "message".into()
            } else {
                event
            },
            data: data.join("\n"),
            id: self.id.clone(),
            retry_ms,
        })
    }
}

/// What a reader does with an event cut off by end of stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EofPolicy {
    /// Drop it, per the EventSource spec (the default).
    #[default]
    Drop,
    /// Deliver a complete final frame that lacks its blank line ([`SseDecoder::finish`]).
    Deliver,
    /// Report an error when the stream ends inside an event ([`SseDecoder::has_pending`]),
    /// so a truncated stream fails closed.
    Fail,
}

/// Blocking iterator of events over any reader. By default an event cut off by EOF is
/// dropped, per the EventSource spec; see [`SseReader::eof_policy`].
pub struct SseReader<R: Read> {
    reader: R,
    decoder: SseDecoder,
    queue: VecDeque<SseEvent>,
    done: bool,
    eof: EofPolicy,
}

impl<R: Read> SseReader<R> {
    pub fn new(reader: R) -> Self {
        Self::with_decoder(reader, SseDecoder::default())
    }

    /// Read with a caller-built decoder (for example [`SseDecoder::eager`]).
    pub fn with_decoder(reader: R, decoder: SseDecoder) -> Self {
        Self {
            reader,
            decoder,
            queue: VecDeque::new(),
            done: false,
            eof: EofPolicy::Drop,
        }
    }

    /// Choose what happens to an event cut off by end of stream.
    pub fn eof_policy(mut self, policy: EofPolicy) -> Self {
        self.eof = policy;
        self
    }
}

impl<R: Read> Iterator for SseReader<R> {
    type Item = Result<SseEvent, HttpError>;
    fn next(&mut self) -> Option<Self::Item> {
        let mut buf = [0u8; 8192];
        loop {
            if let Some(ev) = self.queue.pop_front() {
                return Some(Ok(ev));
            }
            if self.done {
                return None;
            }
            match self.reader.read(&mut buf) {
                Ok(0) => {
                    self.done = true;
                    match self.eof {
                        EofPolicy::Drop => {}
                        EofPolicy::Deliver => match self.decoder.finish() {
                            Ok(Some(ev)) => self.queue.push_back(ev),
                            Ok(None) => {}
                            Err(e) => return Some(Err(e)),
                        },
                        EofPolicy::Fail => {
                            if self.decoder.has_pending() {
                                return Some(Err(HttpError::Stream(
                                    "SSE stream ended inside an event".into(),
                                )));
                            }
                        }
                    }
                }
                Ok(n) => match self.decoder.push(&buf[..n]) {
                    Ok(events) => self.queue.extend(events),
                    Err(e) => {
                        self.done = true;
                        return Some(Err(e));
                    }
                },
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => {
                    self.done = true;
                    return Some(Err(match e.kind() {
                        std::io::ErrorKind::TimedOut => {
                            HttpError::timeout(e.to_string(), crate::error::Phase::Body)
                        }
                        _ => HttpError::transport(e.to_string(), crate::error::Phase::Body),
                    }));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eager_delivers_each_data_line_and_finish_flushes_the_tail() {
        let mut d = SseDecoder::eager();
        let evs = d.push(b"event: tick\ndata: one\ndata: two\n").unwrap();
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].event, "tick");
        assert_eq!(evs[0].data, "one");
        assert_eq!(evs[1].data, "two");
        assert!(!d.has_pending());
        assert!(d.push(b"data: tail").unwrap().is_empty());
        assert!(d.has_pending());
        let last = d.finish().unwrap().unwrap();
        assert_eq!(last.data, "tail");
        assert!(!d.has_pending());
    }

    #[test]
    fn default_mode_finish_delivers_a_frame_missing_its_blank_line() {
        let mut d = SseDecoder::new(1024);
        assert!(d.push(b"data: a\n").unwrap().is_empty());
        assert!(d.has_pending());
        let ev = d.finish().unwrap().unwrap();
        assert_eq!(ev.data, "a");
        assert_eq!(ev.event, "message");
        assert!(!d.has_pending());
    }

    #[test]
    fn reader_eof_policies_drop_deliver_or_fail() {
        let input: &[u8] = b"data: a\n\ndata: b";
        let dropped: Vec<String> = SseReader::new(input).map(|r| r.unwrap().data).collect();
        assert_eq!(dropped, vec!["a"]);
        let delivered: Vec<String> = SseReader::new(input)
            .eof_policy(EofPolicy::Deliver)
            .map(|r| r.unwrap().data)
            .collect();
        assert_eq!(delivered, vec!["a", "b"]);
        let failed: Vec<Result<SseEvent, HttpError>> =
            SseReader::new(input).eof_policy(EofPolicy::Fail).collect();
        assert!(failed.last().unwrap().is_err());
    }
}

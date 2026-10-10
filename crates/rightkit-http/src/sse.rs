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
        }
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

/// Blocking iterator of events over any reader. An event cut off by EOF is
/// dropped, per the EventSource spec.
pub struct SseReader<R: Read> {
    reader: R,
    decoder: SseDecoder,
    queue: VecDeque<SseEvent>,
    done: bool,
}

impl<R: Read> SseReader<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            decoder: SseDecoder::default(),
            queue: VecDeque::new(),
            done: false,
        }
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

//! Event streams that resume from the last sequence seen (rightkit#1120): the host side over
//! [`EventLog`](crate::event_log::EventLog) and the client [`ReplayStream`].
//!
//! The contract both halves keep:
//!
//! * every event frame carries `id: <seq>`, `event: <topic>` and `data: <json>`; the sequence
//!   numbers are those of the [`EventLog`], unique and increasing, and the first one of a process
//!   is its start time, so a restarted host never reuses a number (that is the epoch);
//! * a client resumes with `Last-Event-ID: <seq>` (the `after` query value is the same thing for a
//!   client that cannot set headers) and receives exactly the events after it: no duplicate, no
//!   gap;
//! * a cursor the log cannot continue from (an older epoch, or older than the oldest kept event) is
//!   answered with one `resync` frame, **without an `id`**, so it does not move the cursor: the
//!   client re-reads its state, then continues from the `next` in the frame;
//! * the host sends a comment line every [`KEEPALIVE`] so a dead peer is noticed.
//!
//! The client reconnects with bounded exponential backoff from its own cursor, admits an event only
//! when its number is above the cursor (a replayed or duplicated frame is dropped), and reports a
//! resync without advancing the cursor. A Swift counterpart (an SSE parser and a replay client over
//! the same frames) belongs to RightKitSwift and is not in this crate.

use super::client::open;
use super::http::{Request, Response, StreamSink};
use crate::event_log::EventLog;
use crate::stop::StopToken;
use serde_json::{json, Value};
use std::io::{self, BufRead, Read};
use std::sync::Arc;
use std::time::Duration;

/// How often the host sends a comment line on a quiet stream.
pub const KEEPALIVE: Duration = Duration::from_secs(15);
/// How long the host waits for an event before looking at the sink again.
const POLL_WAIT: Duration = Duration::from_secs(2);

/// The cursor a request resumes from: `Last-Event-ID`, else the `after` query value; `None` for a
/// client that sent neither (it wants only what happens from now on).
fn request_cursor(request: &Request) -> Option<u64> {
    request
        .header("last-event-id")
        .and_then(|v| v.trim().parse::<u64>().ok())
        .or_else(|| {
            request
                .query_pairs()
                .into_iter()
                .find(|(k, _)| k == "after")
                .and_then(|(_, v)| v.trim().parse::<u64>().ok())
        })
}

/// The `GET` response for an event stream over `log`: replays after the request's cursor, then
/// follows. `topics` limits the stream (empty is every topic). Register the route as a
/// declared stream route ([`RouteClass`](super::RouteClass)); the response never ends by itself.
pub fn replay_response(log: Arc<EventLog>, request: &Request, topics: Vec<String>) -> Response {
    let cursor = request_cursor(request);
    Response::event_stream(Box::new(move |sink: &mut dyn StreamSink| {
        let mut after = match cursor {
            Some(after) => after,
            // Live only: start at the newest number.
            None => log.poll(u64::MAX, &topics, Duration::ZERO).next,
        };
        let mut quiet = Duration::ZERO;
        loop {
            if sink.stopping() {
                return;
            }
            let polled = log.poll(after, &topics, POLL_WAIT);
            if polled.dropped {
                let frame = format!(
                    "event: resync\ndata: {}\n\n",
                    json!({ "next": polled.next })
                );
                if sink.send(frame.as_bytes()).is_err() {
                    return;
                }
                after = polled.next;
                continue;
            }
            if polled.events.is_empty() {
                after = polled.next.max(after);
                quiet += POLL_WAIT;
                if quiet >= KEEPALIVE {
                    quiet = Duration::ZERO;
                    if sink.send(b": keepalive\n\n").is_err() {
                        return;
                    }
                }
                continue;
            }
            quiet = Duration::ZERO;
            for event in &polled.events {
                let data = serde_json::to_string(&*event.payload).unwrap_or_else(|_| "null".into());
                let frame = format!(
                    "id: {}\nevent: {}\ndata: {}\n\n",
                    event.seq, event.topic, data
                );
                if sink.send(frame.as_bytes()).is_err() {
                    return;
                }
            }
            after = polled.next;
        }
    }))
}

/// What a [`ReplayStream`] reports.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ReplayEvent {
    /// The stream is open (also after each reconnect).
    Connected,
    /// One event, admitted: its number is above the cursor, which now equals it.
    Event {
        seq: u64,
        topic: String,
        data: Value,
    },
    /// The host could not continue from the cursor (a restart, or events dropped): re-read the
    /// state. The cursor did not move; `next` is where the stream continues.
    Resync { next: Option<u64> },
    /// The connection ended; a reconnect follows after `retry_in`.
    Disconnected { error: String, retry_in: Duration },
}

/// How a [`ReplayStream`] connects and backs off.
#[derive(Debug, Clone)]
pub struct ReplayConfig {
    /// The daemon's loopback port.
    pub port: u16,
    /// The bearer token (a stream route may also take it as the `access_token` query value).
    pub token: String,
    /// The stream path, for example `/sessions/42/events`.
    pub path: String,
    /// The cursor to resume from; `None` starts live.
    pub cursor: Option<u64>,
    /// First reconnect delay; doubles per consecutive failure.
    pub backoff_initial: Duration,
    /// The reconnect delay never exceeds this.
    pub backoff_max: Duration,
    /// A connected stream that is silent this long (the host sends a keepalive every 15 s) is
    /// treated as dead.
    pub idle_timeout: Duration,
}

impl ReplayConfig {
    pub fn new(port: u16, token: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            port,
            token: token.into(),
            path: path.into(),
            cursor: None,
            backoff_initial: Duration::from_millis(250),
            backoff_max: Duration::from_secs(10),
            idle_timeout: Duration::from_secs(45),
        }
    }

    #[must_use]
    pub fn cursor(mut self, cursor: Option<u64>) -> Self {
        self.cursor = cursor;
        self
    }
}

/// The client cursor rule: an event is admitted only above the cursor, and then it is the cursor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReplayCursor {
    last: Option<u64>,
}

impl ReplayCursor {
    pub fn new(last: Option<u64>) -> Self {
        Self { last }
    }

    /// The last admitted number.
    pub fn last(&self) -> Option<u64> {
        self.last
    }

    /// Admit `seq` (true, the cursor moves) or drop it as a duplicate or replay (false).
    pub fn admit(&mut self, seq: u64) -> bool {
        if self.last.is_some_and(|last| seq <= last) {
            return false;
        }
        self.last = Some(seq);
        true
    }
}

/// Run a replay stream until `stop`: connect, resume from the cursor, deliver events to
/// `on_event`, reconnect with bounded exponential backoff, never deliver a duplicate. Blocks;
/// run it on its own thread. Returns the final cursor, for a caller that persists it.
pub fn run_replay_stream(
    config: &ReplayConfig,
    stop: &StopToken,
    mut on_event: impl FnMut(ReplayEvent),
) -> Option<u64> {
    let mut cursor = ReplayCursor::new(config.cursor);
    let mut failures = 0u32;
    while !stop.is_stopped() {
        let outcome = stream_once(config, stop, &mut cursor, &mut on_event, &mut failures);
        if stop.is_stopped() {
            break;
        }
        let error = match outcome {
            Ok(()) => "the stream ended".to_string(),
            Err(e) => e.to_string(),
        };
        let delay = config
            .backoff_initial
            .saturating_mul(1u32 << failures.min(10))
            .min(config.backoff_max)
            .max(Duration::from_millis(1));
        failures = failures.saturating_add(1);
        on_event(ReplayEvent::Disconnected {
            error,
            retry_in: delay,
        });
        if stop.wait(delay).is_some() {
            break;
        }
    }
    cursor.last()
}

fn stream_once(
    config: &ReplayConfig,
    stop: &StopToken,
    cursor: &mut ReplayCursor,
    on_event: &mut impl FnMut(ReplayEvent),
    failures: &mut u32,
) -> io::Result<()> {
    let mut extra: Vec<(&str, String)> = vec![("Accept", "text/event-stream".to_string())];
    if let Some(last) = cursor.last() {
        extra.push(("Last-Event-ID", last.to_string()));
    }
    let mut response = open(
        config.port,
        "GET",
        &config.path,
        Some(&config.token),
        &extra,
        None,
        config.idle_timeout,
    )?;
    if response.status != 200 {
        return Err(io::Error::other(format!(
            "the daemon answered {}",
            response.status
        )));
    }
    *failures = 0;
    on_event(ReplayEvent::Connected);
    let chunked = response.chunked();
    let mut decoder = Frame::default();
    let mut pending = String::new();
    loop {
        if stop.is_stopped() {
            return Ok(());
        }
        let line = if chunked {
            match next_chunk_line(&mut response.reader, &mut pending)? {
                Some(line) => line,
                None => return Ok(()),
            }
        } else {
            let mut raw = String::new();
            if response.reader.read_line(&mut raw)? == 0 {
                return Ok(());
            }
            raw.trim_end_matches(['\r', '\n']).to_string()
        };
        if let Some(frame) = decoder.push_line(&line) {
            deliver(frame, cursor, on_event);
        }
    }
}

/// One decoded server-sent-event frame.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Done {
    id: Option<u64>,
    event: String,
    data: String,
}

#[derive(Default)]
struct Frame {
    current: Done,
    has_data: bool,
}

impl Frame {
    /// Feed one line (no terminator); a blank line ends a frame.
    fn push_line(&mut self, line: &str) -> Option<Done> {
        if line.is_empty() {
            let frame = std::mem::take(&mut self.current);
            let had = std::mem::replace(&mut self.has_data, false);
            return had.then_some(frame);
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "id" => self.current.id = value.trim().parse::<u64>().ok(),
            "event" => self.current.event = value.to_string(),
            "data" => {
                if self.has_data {
                    self.current.data.push('\n');
                }
                self.current.data.push_str(value);
                self.has_data = true;
            }
            _ => {}
        }
        None
    }
}

fn deliver(frame: Done, cursor: &mut ReplayCursor, on_event: &mut impl FnMut(ReplayEvent)) {
    let data: Value = serde_json::from_str(&frame.data).unwrap_or(Value::String(frame.data));
    if frame.event == "resync" {
        // A resync carries no id and never moves the cursor.
        on_event(ReplayEvent::Resync {
            next: data.get("next").and_then(Value::as_u64),
        });
        return;
    }
    let Some(seq) = frame.id else {
        return;
    };
    if cursor.admit(seq) {
        on_event(ReplayEvent::Event {
            seq,
            topic: frame.event,
            data,
        });
    }
}

/// The next text line out of a chunked body (chunks may split a line, and a chunk may hold
/// several). `pending` carries a partial line across calls. `None` at the end of the body.
fn next_chunk_line(
    reader: &mut std::io::BufReader<std::net::TcpStream>,
    pending: &mut String,
) -> io::Result<Option<String>> {
    loop {
        if let Some(i) = pending.find('\n') {
            let line: String = pending.drain(..=i).collect();
            return Ok(Some(line.trim_end_matches(['\r', '\n']).to_string()));
        }
        let mut size_line = String::new();
        if reader.read_line(&mut size_line)? == 0 {
            return Ok(None);
        }
        let size_text = size_line.trim().split(';').next().unwrap_or("");
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad chunk size"))?;
        if size == 0 {
            return Ok(None);
        }
        if size > 8 * 1024 * 1024 || pending.len() + size > 8 * 1024 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "stream chunk too large",
            ));
        }
        let mut data = vec![0u8; size];
        reader.read_exact(&mut data)?;
        let mut crlf = [0u8; 2];
        reader.read_exact(&mut crlf)?;
        pending.push_str(
            &String::from_utf8(data)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "stream is not UTF-8"))?,
        );
    }
}

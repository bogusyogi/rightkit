//! Event streams that resume from the last sequence seen (rightkit#1120): the host side over
//! [`EventLog`] and the client replay stream ([`run_replay_stream`]).
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
//!
//! The pure types here ([`EpochCursor`], [`CursorHandle`], [`ReconnectBackoff`], [`ReplayWire`]) serve
//! an async or non-loopback client that streams over its own transport (https, many sessions): they
//! do no I/O. `run_replay_stream` and `run_replay_stream_with` are loopback-only plain TCP.

use super::client::open;
use super::http::{Request, Response, StreamSink};
use crate::event_log::EventLog;
use crate::stop::StopToken;
use serde_json::{json, Value};
use std::io::{self, BufRead, Read};
use std::sync::{Arc, Mutex, MutexGuard};
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

/// What a replay stream ([`run_replay_stream`]) reports.
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
    /// An event whose epoch differs from the cursor's: the cursor was reset to it and the `Event`
    /// that follows is admitted even when its number is lower than the old cursor. Emitted only by
    /// [`run_replay_stream_with`], immediately before that `Event`.
    EpochReset { epoch: String, seq: u64 },
    /// A pass that delivered no frame ended (a refused connect, a non-200 answer, a dropped stream).
    /// `http_status` is the status of a non-200 answer, `None` for a connect or read error.
    /// `consecutive_failures` counts the passes in a row that delivered nothing, this one included.
    /// Emitted immediately before the `Disconnected` for that pass.
    Failed {
        consecutive_failures: u32,
        http_status: Option<u16>,
    },
}

/// How a replay stream ([`run_replay_stream`]) connects and backs off.
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

/// The client-side cursor with an epoch: the last admitted number and the epoch it belongs to. A
/// daemon restart changes the epoch (the first number of a process is its start time), so an event
/// from a new epoch resets the cursor even when its number is lower than the old one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EpochCursor {
    last: Option<u64>,
    epoch: Option<String>,
}

impl EpochCursor {
    pub fn new(last: Option<u64>, epoch: Option<String>) -> Self {
        Self { last, epoch }
    }

    /// The last admitted number.
    pub fn last(&self) -> Option<u64> {
        self.last
    }

    /// The epoch of the last admitted event, if one was seen.
    pub fn epoch(&self) -> Option<&str> {
        self.epoch.as_deref()
    }

    /// Raise the cursor to `seq` when it is unset or lower; never lowers it. For events paged out of
    /// band (for example from `/events/replay`). True when the cursor moved.
    pub fn advance(&mut self, seq: u64) -> bool {
        if self.last.is_none_or(|last| seq > last) {
            self.last = Some(seq);
            return true;
        }
        false
    }

    /// Decide one event. An epoch that is present, non-empty and different from the cursor's (an
    /// unset cursor epoch counts as different) resets the cursor to `seq` and is admitted, even when
    /// `seq` is below the old cursor. Otherwise a number at or below the cursor is a duplicate, and a
    /// higher one is admitted and becomes the cursor.
    pub fn admit(&mut self, epoch: Option<&str>, seq: u64) -> Admission {
        if let Some(new) = epoch.filter(|e| !e.is_empty()) {
            if self.epoch.as_deref() != Some(new) {
                self.epoch = Some(new.to_string());
                self.last = Some(seq);
                return Admission::EpochReset;
            }
        }
        if self.last.is_some_and(|last| seq <= last) {
            return Admission::Duplicate;
        }
        self.last = Some(seq);
        Admission::Admitted
    }
}

/// The outcome of [`EpochCursor::admit`] (and [`CursorHandle::admit`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Admission {
    /// Above the cursor: deliver it; the cursor is now its number.
    Admitted,
    /// A new epoch: the cursor was reset to it; deliver it.
    EpochReset,
    /// At or below the cursor within the same epoch: drop it.
    Duplicate,
}

/// A cursor shared by the stream that owns it and any code that pages `/events/replay`. Clones share
/// one cursor; the lock is held only for the call.
#[derive(Debug, Clone, Default)]
pub struct CursorHandle(Arc<Mutex<EpochCursor>>);

impl CursorHandle {
    pub fn new(cursor: EpochCursor) -> Self {
        Self(Arc::new(Mutex::new(cursor)))
    }

    fn state(&self) -> MutexGuard<'_, EpochCursor> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A copy of the cursor now.
    pub fn snapshot(&self) -> EpochCursor {
        self.state().clone()
    }

    /// The last admitted number.
    pub fn last(&self) -> Option<u64> {
        self.state().last()
    }

    /// See [`EpochCursor::advance`].
    pub fn advance(&self, seq: u64) -> bool {
        self.state().advance(seq)
    }

    /// See [`EpochCursor::admit`].
    pub fn admit(&self, epoch: Option<&str>, seq: u64) -> Admission {
        self.state().admit(epoch, seq)
    }
}

/// The reconnect delay rule: the first delay is `initial`, the delay doubles per pass that delivered
/// no frame and never exceeds `max`, and a pass that delivered a frame resets it to `initial`, even
/// when that pass then failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconnectBackoff {
    initial: Duration,
    max: Duration,
    failures: u32,
}

impl ReconnectBackoff {
    pub fn new(initial: Duration, max: Duration) -> Self {
        Self {
            initial,
            max,
            failures: 0,
        }
    }

    /// The delay before the next pass, given how many frames the pass that just ended delivered.
    pub fn next_delay(&mut self, frames_delivered: u64) -> Duration {
        if frames_delivered > 0 {
            self.failures = 0;
        }
        let delay = self
            .initial
            .saturating_mul(1u32 << self.failures.min(10))
            .min(self.max)
            .max(Duration::from_millis(1));
        self.failures = self.failures.saturating_add(1);
        delay
    }
}

/// How a stream names its resume position: the `Last-Event-ID` header, or a query pair on the path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeVia {
    /// Send `Last-Event-ID: <last>` (no header when nothing was admitted yet).
    LastEventId,
    /// Append `<name>=<last>` to the path, with `?` or `&` as the path needs. No `Last-Event-ID`.
    AfterQuery(String),
}

/// Where the sequence number of a frame is read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeqFrom {
    /// The SSE `id:` value.
    EventId,
    /// The first listed field of the JSON data object that is a non-negative integer.
    JsonFields(Vec<String>),
}

/// The frame that tells the client to re-read its state: an SSE event name, or a JSON data field
/// with a given value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResyncMarker {
    /// A frame whose SSE event name equals this string.
    SseEvent(String),
    /// A frame whose JSON data object has `field == value`.
    JsonKind { field: String, value: String },
}

/// The wire profile of a stream: how it resumes, where its sequence and epoch come from, and what
/// marks a resync. The default is CodeRight's wire: `Last-Event-ID`, the SSE `id`, no epoch, and an
/// `event: resync` frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayWire {
    resume: ResumeVia,
    seq: SeqFrom,
    epoch_field: Option<String>,
    resync: ResyncMarker,
    end_pass_on_resync: bool,
}

impl Default for ReplayWire {
    fn default() -> Self {
        Self {
            resume: ResumeVia::LastEventId,
            seq: SeqFrom::EventId,
            epoch_field: None,
            resync: ResyncMarker::SseEvent("resync".into()),
            end_pass_on_resync: false,
        }
    }
}

impl ReplayWire {
    #[must_use]
    pub fn resume(mut self, resume: ResumeVia) -> Self {
        self.resume = resume;
        self
    }

    #[must_use]
    pub fn seq(mut self, seq: SeqFrom) -> Self {
        self.seq = seq;
        self
    }

    /// A string field of the data object that carries the epoch.
    #[must_use]
    pub fn epoch_field(mut self, field: impl Into<String>) -> Self {
        self.epoch_field = Some(field.into());
        self
    }

    #[must_use]
    pub fn resync(mut self, resync: ResyncMarker) -> Self {
        self.resync = resync;
        self
    }

    /// End the pass after a resync, so the caller's out-of-band paging runs before the next connect.
    #[must_use]
    pub fn end_pass_on_resync(mut self, on: bool) -> Self {
        self.end_pass_on_resync = on;
        self
    }

    /// Classify one decoded frame. The data is parsed as JSON, or kept as a string when it is not.
    /// A resync is checked first and never becomes an `Event`. A frame with no usable sequence is
    /// `Ignore`.
    pub fn classify(&self, event: &str, id: Option<&str>, data: &str) -> Classified {
        let value: Value =
            serde_json::from_str(data).unwrap_or_else(|_| Value::String(data.to_string()));
        let is_resync = match &self.resync {
            ResyncMarker::SseEvent(name) => event == name.as_str(),
            ResyncMarker::JsonKind { field, value: kind } => {
                value.get(field.as_str()).and_then(Value::as_str) == Some(kind.as_str())
            }
        };
        if is_resync {
            return Classified::Resync {
                next: value.get("next").and_then(Value::as_u64),
            };
        }
        let seq = match &self.seq {
            SeqFrom::EventId => id.and_then(|text| text.trim().parse::<u64>().ok()),
            SeqFrom::JsonFields(fields) => fields
                .iter()
                .find_map(|field| value.get(field.as_str()).and_then(Value::as_u64)),
        };
        let Some(seq) = seq else {
            return Classified::Ignore;
        };
        let epoch = self
            .epoch_field
            .as_deref()
            .and_then(|field| value.get(field))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        Classified::Event {
            seq,
            epoch,
            topic: event.to_string(),
            data: value,
        }
    }
}

/// One frame, classified by [`ReplayWire::classify`].
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Classified {
    /// An event with its sequence, its epoch (when the wire carries one) and its topic.
    Event {
        seq: u64,
        epoch: Option<String>,
        topic: String,
        data: Value,
    },
    /// A resync. `next` is the `next` field of the data, when it has one. Never moves a cursor.
    Resync { next: Option<u64> },
    /// Not an event and not a resync (no usable sequence).
    Ignore,
}

/// Run a replay stream until `stop`: connect, resume from the cursor, deliver events to
/// `on_event`, reconnect with bounded exponential backoff, never deliver a duplicate. Blocks;
/// run it on its own thread. Returns the final cursor, for a caller that persists it.
///
/// This is [`run_replay_stream_with`] on CodeRight's wire ([`ReplayWire::default`]) with a cursor
/// that has no epoch.
pub fn run_replay_stream(
    config: &ReplayConfig,
    stop: &StopToken,
    on_event: impl FnMut(ReplayEvent),
) -> Option<u64> {
    run_replay_stream_with(
        config,
        &ReplayWire::default(),
        &CursorHandle::new(EpochCursor::new(config.cursor, None)),
        stop,
        on_event,
    )
}

/// [`run_replay_stream`] over a [`ReplayWire`] and a shared [`CursorHandle`]. The resume position is
/// read from the handle before each connect, so a caller that pages `/events/replay` and calls
/// [`CursorHandle::advance`] between passes resumes after it. Returns the cursor at the end.
pub fn run_replay_stream_with(
    config: &ReplayConfig,
    wire: &ReplayWire,
    handle: &CursorHandle,
    stop: &StopToken,
    mut on_event: impl FnMut(ReplayEvent),
) -> Option<u64> {
    let mut backoff = ReconnectBackoff::new(config.backoff_initial, config.backoff_max);
    while !stop.is_stopped() {
        let mut frames = 0u64;
        let outcome = stream_once(config, wire, stop, handle, &mut on_event, &mut frames);
        if stop.is_stopped() {
            break;
        }
        let (error, http_status) = match outcome {
            Ok(()) => ("the stream ended".to_string(), None),
            Err(e) => (e.message, e.status),
        };
        let delay = backoff.next_delay(frames);
        if frames == 0 {
            on_event(ReplayEvent::Failed {
                consecutive_failures: backoff.failures,
                http_status,
            });
        }
        on_event(ReplayEvent::Disconnected {
            error,
            retry_in: delay,
        });
        if stop.wait(delay).is_some() {
            break;
        }
    }
    handle.last()
}

/// Why a pass ended with an error. `status` is the HTTP status when the daemon answered something
/// other than 200; `None` for a connect or read error.
#[derive(Debug)]
struct PassError {
    status: Option<u16>,
    message: String,
}

impl From<io::Error> for PassError {
    fn from(e: io::Error) -> Self {
        Self {
            status: None,
            message: e.to_string(),
        }
    }
}

fn stream_once(
    config: &ReplayConfig,
    wire: &ReplayWire,
    stop: &StopToken,
    handle: &CursorHandle,
    on_event: &mut impl FnMut(ReplayEvent),
    frames: &mut u64,
) -> Result<(), PassError> {
    // The resume position is read here, before this connect.
    let mut extra: Vec<(&str, String)> = vec![("Accept", "text/event-stream".to_string())];
    let mut path = config.path.clone();
    match &wire.resume {
        ResumeVia::LastEventId => {
            if let Some(last) = handle.last() {
                extra.push(("Last-Event-ID", last.to_string()));
            }
        }
        ResumeVia::AfterQuery(name) => {
            if let Some(last) = handle.last() {
                let sep = if path.contains('?') { '&' } else { '?' };
                path = format!("{path}{sep}{name}={last}");
            }
        }
    }
    let mut response = open(
        config.port,
        "GET",
        &path,
        Some(&config.token),
        &extra,
        None,
        config.idle_timeout,
    )?;
    if response.status != 200 {
        return Err(PassError {
            status: Some(response.status),
            message: format!("the daemon answered {}", response.status),
        });
    }
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
            *frames += 1;
            if deliver(frame, wire, handle, on_event) && wire.end_pass_on_resync {
                return Ok(());
            }
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

/// Classify one decoded frame and deliver what it means. Returns true when the frame was a resync
/// (the caller ends the pass when the wire asks for it). A resync never moves the cursor.
fn deliver(
    frame: Done,
    wire: &ReplayWire,
    handle: &CursorHandle,
    on_event: &mut impl FnMut(ReplayEvent),
) -> bool {
    let id = frame.id.map(|n| n.to_string());
    match wire.classify(&frame.event, id.as_deref(), &frame.data) {
        Classified::Event {
            seq,
            epoch,
            topic,
            data,
        } => {
            match handle.admit(epoch.as_deref(), seq) {
                Admission::Duplicate => {}
                Admission::EpochReset => {
                    on_event(ReplayEvent::EpochReset {
                        epoch: epoch.unwrap_or_default(),
                        seq,
                    });
                    on_event(ReplayEvent::Event { seq, topic, data });
                }
                Admission::Admitted => on_event(ReplayEvent::Event { seq, topic, data }),
            }
            false
        }
        Classified::Resync { next } => {
            on_event(ReplayEvent::Resync { next });
            true
        }
        Classified::Ignore => false,
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

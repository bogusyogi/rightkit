//! Periodic work and a heartbeat record for a daemon host (issue #376).
//!
//! [`spawn_ticker`] runs a closure every interval on its own thread until a [`StopToken`] is
//! triggered. [`spawn_heartbeat`] is a ticker that writes a small JSON record (`pid`, `updated_ms`
//! and the caller's `state`) atomically to a path, so a supervisor or a client can read it. Both
//! end on the first tick after the stop and do no work after it. A write failure is reported to
//! the caller's `on_error` and the heartbeat keeps going.

use crate::record::{write_atomic, write_atomic_with};
use crate::stop::StopToken;
use serde_json::{json, Value};
use std::io;
use std::path::PathBuf;
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Run `tick` every `interval` until `stop` is triggered. The first tick comes one interval after
/// the spawn. The thread is named `rk-ticker`.
pub fn spawn_ticker(
    interval: Duration,
    stop: StopToken,
    mut tick: impl FnMut() + Send + 'static,
) -> io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("rk-ticker".into())
        .spawn(move || loop {
            if stop.wait(interval).is_some() {
                break;
            }
            tick();
        })
}

/// The heartbeat record: `pid`, `updated_ms` (milliseconds since the Unix epoch) and `state`.
pub fn heartbeat_record(state: Value) -> Value {
    let updated_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    json!({"pid": std::process::id(), "updated_ms": updated_ms, "state": state})
}

/// Write a heartbeat record to `path` now and then every `interval`, until `stop` is triggered.
/// `state` is called for each write. Writes go through [`write_atomic`], so a reader never sees a
/// partial record. A failed write is passed to `on_error`; the next interval tries again.
pub fn spawn_heartbeat(
    path: PathBuf,
    interval: Duration,
    stop: StopToken,
    state: impl Fn() -> Value + Send + 'static,
    mut on_error: impl FnMut(&io::Error) + Send + 'static,
) -> io::Result<JoinHandle<()>> {
    let write = move |path: &PathBuf, state: &dyn Fn() -> Value| {
        let body = serde_json::to_vec(&heartbeat_record(state()))
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        write_atomic(path, &body)
    };
    std::thread::Builder::new()
        .name("rk-heartbeat".into())
        .spawn(move || loop {
            if let Err(e) = write(&path, &state) {
                on_error(&e);
            }
            if stop.wait(interval).is_some() {
                break;
            }
        })
}

/// How [`spawn_heartbeat_with`] paces its writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeartbeatOptions {
    /// How often an unchanged state is rewritten (the liveness beat): without `fsync`.
    pub interval: Duration,
    /// How often the state is looked at to see whether it changed. A change is written at once
    /// (durably) at the next look, so this bounds how late a change reaches the record. Clamped to
    /// `1 ms..=interval`.
    pub poll: Duration,
}

impl Default for HeartbeatOptions {
    /// A 5 s liveness beat and a 1 s change poll, the cadence of Pulse's resident engine.
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(5),
            poll: Duration::from_secs(1),
        }
    }
}

/// [`spawn_heartbeat`] that does not wear the disk (rightkit#1035). `state` is read every
/// [`poll`](HeartbeatOptions::poll). When it differs from the state last written, the record is
/// written durably at once ([`write_atomic`]: a reader and a crash both see the change). When it is
/// unchanged the record is only rewritten, without `fsync`
/// ([`write_atomic_with`] with `durable` false), once per [`interval`](HeartbeatOptions::interval),
/// so `updated_ms` still advances and a reader can tell a live host from a dead one. The first
/// write is durable and immediate. A failed write is passed to `on_error` and the next look tries
/// again (a failed change is still a change, so it is retried durably).
///
/// `state` must be cheap: it runs once per poll. The thread is named `rk-heartbeat` and ends on the
/// first look after `stop`.
pub fn spawn_heartbeat_with(
    path: PathBuf,
    options: HeartbeatOptions,
    stop: StopToken,
    state: impl Fn() -> Value + Send + 'static,
    mut on_error: impl FnMut(&io::Error) + Send + 'static,
) -> io::Result<JoinHandle<()>> {
    let poll = options
        .poll
        .max(Duration::from_millis(1))
        .min(options.interval.max(Duration::from_millis(1)));
    let interval = options.interval;
    std::thread::Builder::new()
        .name("rk-heartbeat".into())
        .spawn(move || {
            let mut last: Option<(Value, std::time::Instant)> = None;
            loop {
                let now_state = state();
                let changed = last.as_ref().is_none_or(|(v, _)| *v != now_state);
                let due = last.as_ref().is_none_or(|(_, at)| at.elapsed() >= interval);
                if changed || due {
                    let written = serde_json::to_vec(&heartbeat_record(now_state.clone()))
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
                        .and_then(|body| write_atomic_with(&path, &body, changed));
                    match written {
                        Ok(()) => last = Some((now_state, std::time::Instant::now())),
                        Err(e) => on_error(&e),
                    }
                }
                if stop.wait(poll).is_some() {
                    break;
                }
            }
        })
}

/// Beat the `heartbeat_ms` and `state` fields of this process's own `service.json`
/// ([`ServiceRecord`](crate::ServiceRecord), rightkit#404) with the same two rules as
/// [`spawn_heartbeat_with`]: a change of `state` is written durably at once, an unchanged state is
/// rewritten without `fsync` once per interval. The record must exist and be this process's (its
/// `pid` equals [`std::process::id`]); a record that is missing, unreadable or another process's
/// is reported to `on_error` and never overwritten, so a host that lost its election does not
/// clobber the winner. Only `heartbeat_ms` and `state` change.
pub fn spawn_record_heartbeat(
    suite: crate::Suite,
    service: String,
    options: HeartbeatOptions,
    stop: StopToken,
    state: impl Fn() -> String + Send + 'static,
    mut on_error: impl FnMut(&io::Error) + Send + 'static,
) -> io::Result<JoinHandle<()>> {
    let path = crate::record_path(&suite, &service);
    let poll = options
        .poll
        .max(Duration::from_millis(1))
        .min(options.interval.max(Duration::from_millis(1)));
    let interval = options.interval;
    std::thread::Builder::new()
        .name("rk-record-heartbeat".into())
        .spawn(move || {
            let mut last: Option<(String, std::time::Instant)> = None;
            loop {
                let now_state = state();
                let changed = last.as_ref().is_none_or(|(v, _)| *v != now_state);
                let due = last.as_ref().is_none_or(|(_, at)| at.elapsed() >= interval);
                if changed || due {
                    let written = (|| -> io::Result<()> {
                        let mut record =
                            crate::try_read_record(&suite, &service)?.ok_or_else(|| {
                                io::Error::new(io::ErrorKind::NotFound, "no service record")
                            })?;
                        if record.pid != std::process::id() {
                            return Err(io::Error::new(
                                io::ErrorKind::PermissionDenied,
                                "the service record belongs to another process",
                            ));
                        }
                        record.heartbeat_ms = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map(|d| d.as_millis() as u64)
                            .unwrap_or(0);
                        record.state = now_state.clone();
                        let body = serde_json::to_vec_pretty(&record)
                            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                        write_atomic_with(&path, &body, changed)
                    })();
                    match written {
                        Ok(()) => last = Some((now_state, std::time::Instant::now())),
                        Err(e) => on_error(&e),
                    }
                }
                if stop.wait(poll).is_some() {
                    break;
                }
            }
        })
}

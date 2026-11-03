//! Periodic work and a heartbeat record for a daemon host (issue #376).
//!
//! [`spawn_ticker`] runs a closure every interval on its own thread until a [`StopToken`] is
//! triggered. [`spawn_heartbeat`] is a ticker that writes a small JSON record (`pid`, `updated_ms`
//! and the caller's `state`) atomically to a path, so a supervisor or a client can read it. Both
//! end on the first tick after the stop and do no work after it. A write failure is reported to
//! the caller's `on_error` and the heartbeat keeps going.

use crate::record::write_atomic;
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

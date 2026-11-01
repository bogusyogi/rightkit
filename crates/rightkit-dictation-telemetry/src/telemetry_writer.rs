use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TrySendError};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

const MAX_EVENTS_JSONL_BYTES: u64 = 10 * 1024 * 1024;
const MAX_EVENT_JSON_BYTES: usize = 64 * 1024;
const MAX_QUEUE_EVENTS: usize = 256;
const MAINTENANCE_TIMEOUT: Duration = Duration::from_secs(5);

static WRITER: OnceLock<Option<WriterHandle>> = OnceLock::new();
static DROPPED_EVENTS: AtomicU64 = AtomicU64::new(0);

struct WriterHandle {
    sender: SyncSender<Command>,
}

impl WriterHandle {
    fn try_send(&self, command: Command) -> Result<(), TrySendError<Command>> {
        self.sender.try_send(command)
    }

    fn enqueue_append(&self, path: &Path, value: Value, dropped: &AtomicU64) {
        if self
            .try_send(Command::Append {
                path: path.to_path_buf(),
                value,
            })
            .is_err()
        {
            dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

enum Command {
    Append {
        path: PathBuf,
        value: Value,
    },
    /// An `events.jsonl` line whose path the writer resolves on its own thread, so a caller
    /// (often the UI thread) never waits on the app-data folder lookup.
    AppendEvents {
        value: Value,
    },
    Flush {
        reply: Sender<Result<(), String>>,
        deadline: Instant,
    },
    Snapshot {
        destination: PathBuf,
        paths: Vec<PathBuf>,
        reply: Sender<Result<Vec<String>, String>>,
        deadline: Instant,
    },
    Clear {
        paths: Vec<PathBuf>,
        reply: Sender<Result<(), String>>,
        deadline: Instant,
    },
}

fn rotated_log_path(path: &Path) -> PathBuf {
    let filename = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!("{filename}.1"))
}

fn spawn_writer() -> Option<WriterHandle> {
    let (sender, receiver) = mpsc::sync_channel(MAX_QUEUE_EVENTS);
    let worker_sender = sender.clone();
    thread::Builder::new()
        .name("rightkit-telemetry-writer".to_string())
        .spawn(move || run_writer(receiver))
        .ok()?;
    Some(WriterHandle {
        sender: worker_sender,
    })
}

fn writer() -> Option<&'static WriterHandle> {
    WRITER.get_or_init(spawn_writer).as_ref()
}

fn enqueue(command: Command) -> Result<(), TrySendError<Command>> {
    let Some(handle) = writer() else {
        return Err(TrySendError::Disconnected(command));
    };
    handle.try_send(command)
}

pub(crate) fn append(path: &Path, value: Value) {
    let value = bound_event(value);
    if let Some(handle) = writer() {
        handle.enqueue_append(path, value, &DROPPED_EVENTS);
    } else {
        DROPPED_EVENTS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Queue an `events.jsonl` line without resolving its path on the caller's thread.
pub(crate) fn append_events(value: Value) {
    let value = bound_event(value);
    let sent =
        writer().is_some_and(|handle| handle.try_send(Command::AppendEvents { value }).is_ok());
    if !sent {
        DROPPED_EVENTS.fetch_add(1, Ordering::Relaxed);
    }
}

fn bound_event(value: Value) -> Value {
    match serde_json::to_vec(&value) {
        Ok(bytes) if bytes.len() <= MAX_EVENT_JSON_BYTES => value,
        Ok(bytes) => json!({
            "schema_version": 1,
            "event": "telemetry_event_dropped",
            "reason": "event_too_large",
            "bytes": bytes.len(),
        }),
        Err(_) => json!({
            "schema_version": 1,
            "event": "telemetry_event_dropped",
            "reason": "event_not_serializable",
        }),
    }
}

fn maintenance_send(command: Command, deadline: Instant) -> Result<(), String> {
    let mut command = command;
    loop {
        match enqueue(command) {
            Ok(()) => return Ok(()),
            Err(TrySendError::Disconnected(returned)) => {
                drop(returned);
                return Err("telemetry writer is unavailable".into());
            }
            Err(TrySendError::Full(returned)) => {
                command = returned;
                if Instant::now() >= deadline {
                    return Err("telemetry writer queue did not accept maintenance command".into());
                }
                thread::sleep(Duration::from_millis(1));
            }
        }
    }
}

pub(crate) fn flush_pending_logs() -> Result<(), String> {
    let (reply, receiver) = mpsc::channel();
    let deadline = Instant::now() + MAINTENANCE_TIMEOUT;
    maintenance_send(Command::Flush { reply, deadline }, deadline)?;
    receiver
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|_| "telemetry writer flush timed out".to_string())?
}

pub(crate) fn snapshot_log_files(
    destination: &Path,
    paths: &[PathBuf],
) -> Result<Vec<String>, String> {
    let (reply, receiver) = mpsc::channel();
    let deadline = Instant::now() + MAINTENANCE_TIMEOUT;
    maintenance_send(
        Command::Snapshot {
            destination: destination.to_path_buf(),
            paths: paths.to_vec(),
            reply,
            deadline,
        },
        deadline,
    )?;
    receiver
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|_| "telemetry snapshot timed out".to_string())?
}

pub(crate) fn clear_local_logs(paths: &[PathBuf]) -> Result<(), String> {
    let (reply, receiver) = mpsc::channel();
    let deadline = Instant::now() + MAINTENANCE_TIMEOUT;
    maintenance_send(
        Command::Clear {
            paths: paths.to_vec(),
            reply,
            deadline,
        },
        deadline,
    )?;
    receiver
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|_| "telemetry clear timed out".to_string())?
}

/// Append one line, first recording how many lines the bounded queue dropped since the last one.
fn append_with_overflow(
    sinks: &mut HashMap<PathBuf, rightkit_logs::JsonlStore>,
    path: &Path,
    value: &Value,
) {
    let dropped = DROPPED_EVENTS.swap(0, Ordering::AcqRel);
    if dropped > 0 {
        append_to_sink(
            sinks,
            path,
            &json!({
                "schema_version": 1,
                "event": "telemetry_queue_overflow",
                "scope": "process",
                "dropped_events": dropped,
            }),
        );
    }
    append_to_sink(sinks, path, value);
}

fn run_writer(receiver: Receiver<Command>) {
    let mut sinks = HashMap::<PathBuf, rightkit_logs::JsonlStore>::new();
    while let Ok(command) = receiver.recv() {
        match command {
            Command::Append { path, value } => append_with_overflow(&mut sinks, &path, &value),
            Command::AppendEvents { value } => {
                append_with_overflow(&mut sinks, &crate::telemetry::log_path(), &value)
            }
            Command::Flush { reply, deadline } => {
                let result = if Instant::now() <= deadline {
                    Ok(())
                } else {
                    Err("telemetry writer flush deadline expired".to_string())
                };
                let _ = reply.send(result);
            }
            Command::Snapshot {
                destination,
                paths,
                reply,
                deadline,
            } => {
                let result = if Instant::now() <= deadline {
                    snapshot_files(&destination, &paths)
                } else {
                    Err("telemetry snapshot deadline expired".to_string())
                };
                let _ = reply.send(result);
            }
            Command::Clear {
                paths,
                reply,
                deadline,
            } => {
                let result = if Instant::now() <= deadline {
                    sinks.clear();
                    DROPPED_EVENTS.store(0, Ordering::Release);
                    paths.into_iter().try_for_each(|path| {
                        rightkit_logs::JsonlStore::new(&path, MAX_EVENTS_JSONL_BYTES)
                            .clear()
                            .map_err(|err| format!("delete {}: {err}", path.display()))
                    })
                } else {
                    Err("telemetry clear deadline expired".to_string())
                };
                let _ = reply.send(result);
            }
        }
    }
}

fn append_to_sink(
    sinks: &mut HashMap<PathBuf, rightkit_logs::JsonlStore>,
    path: &Path,
    value: &Value,
) {
    let sink = sinks
        .entry(path.to_path_buf())
        .or_insert_with(|| rightkit_logs::JsonlStore::new(path, MAX_EVENTS_JSONL_BYTES));
    if let Err(error) = sink.append_value(value) {
        sinks.remove(path);
        eprintln!("[telemetry] append to {} failed ({error})", path.display());
    }
}

fn snapshot_files(destination: &Path, paths: &[PathBuf]) -> Result<Vec<String>, String> {
    fs::create_dir_all(destination)
        .map_err(|error| format!("create diagnostics staging directory: {error}"))?;
    let mut copied = Vec::new();
    for path in paths {
        for source in [path.clone(), rotated_log_path(path)] {
            if !source.is_file() {
                continue;
            }
            let name = source
                .file_name()
                .ok_or_else(|| format!("diagnostics log has no file name: {}", source.display()))?
                .to_string_lossy()
                .into_owned();
            fs::copy(&source, destination.join(&name))
                .map_err(|error| format!("copy {}: {error}", source.display()))?;
            copied.push(name);
        }
    }
    Ok(copied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "rightkit-telemetry-writer-{label}-{}-{nonce}.jsonl",
            std::process::id()
        ))
    }

    #[test]
    fn oversized_events_are_replaced_with_bounded_marker() {
        let value = bound_event(json!({ "message": "x".repeat(MAX_EVENT_JSON_BYTES) }));
        assert_eq!(value["event"], "telemetry_event_dropped");
        assert_eq!(value["reason"], "event_too_large");
    }

    #[test]
    fn full_queue_drops_append_without_touching_global_writer() {
        let (sender, _receiver) = mpsc::sync_channel(MAX_QUEUE_EVENTS);
        let handle = WriterHandle { sender };
        let dropped = AtomicU64::new(0);
        let path = PathBuf::from("isolated-queue-test.jsonl");

        for index in 0..MAX_QUEUE_EVENTS {
            handle.enqueue_append(&path, json!({ "index": index }), &dropped);
        }
        assert_eq!(dropped.load(Ordering::Relaxed), 0);

        let started = Instant::now();
        handle.enqueue_append(&path, json!({ "index": MAX_QUEUE_EVENTS }), &dropped);
        assert!(started.elapsed() < Duration::from_millis(100));
        assert_eq!(dropped.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn flush_barrier_preserves_enqueue_order() {
        let path = temp_path("order");
        for index in 0..3 {
            append(&path, json!({ "event": "ordered", "index": index }));
        }
        flush_pending_logs().expect("ordered events flush");

        let lines = fs::read_to_string(&path).expect("ordered log");
        let events = lines
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).expect("json event"))
            .collect::<Vec<_>>();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0]["index"], 0);
        assert_eq!(events[1]["index"], 1);
        assert_eq!(events[2]["index"], 2);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn clear_barrier_runs_after_prior_appends() {
        let path = temp_path("clear");
        append(&path, json!({ "event": "cleared" }));
        clear_local_logs(std::slice::from_ref(&path)).expect("clear queued log");
        assert!(!path.exists(), "clear must follow prior append");
    }
}

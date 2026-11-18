//! One worker thread. The recording path only enqueues; the worker processes commands strictly
//! in order.

use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, OnceLock};

use super::backend::{Backend, Lifecycle};

enum Command {
    Begin(String),
    Restore(String, String),
    RestoreAny(String),
}

struct Worker {
    tx: Mutex<Sender<Command>>,
}

impl Worker {
    fn spawn<B: Backend + Send + 'static>(backend: B) -> Worker {
        let (tx, rx) = mpsc::channel::<Command>();
        let result = std::thread::Builder::new()
            .name("media-control".into())
            .spawn(move || {
                let lifecycle = Lifecycle::new(backend);
                for command in rx {
                    match command {
                        Command::Begin(session) => lifecycle.begin(&session),
                        Command::Restore(session, reason) => {
                            crate::event("media_restore_for_session", &reason);
                            lifecycle.restore_for_session(&session);
                        }
                        Command::RestoreAny(reason) => {
                            crate::event("media_restore_any", &reason);
                            lifecycle.restore_any();
                        }
                    }
                }
            });
        if let Err(error) = result {
            // A failed spawn must degrade media control to a no-op, never take the process down.
            // `rx` was dropped with the unspawned closure, so `send` below discards every command.
            crate::event("media_worker_spawn_failed", &format!("{error}"));
        }
        Worker { tx: Mutex::new(tx) }
    }

    fn send(&self, command: Command) {
        if let Ok(tx) = self.tx.lock() {
            let _ = tx.send(command);
        }
    }
}

static WORKER: OnceLock<Worker> = OnceLock::new();

/// Install a host-supplied backend. Call once, before the first recording. Returns false when a
/// backend (this one or the built-in one) is already running; the given backend is then dropped.
pub fn install_backend<B: Backend + Send + 'static>(backend: B) -> bool {
    let mut slot = Some(backend);
    WORKER.get_or_init(|| Worker::spawn(slot.take().expect("backend present on first init")));
    slot.is_none()
}

/// The running worker: a host-installed backend, else the built-in one where this crate has one
/// (Windows). `None` means no backend, so every call is a no-op.
fn worker() -> Option<&'static Worker> {
    #[cfg(target_os = "windows")]
    {
        Some(WORKER.get_or_init(|| Worker::spawn(super::win::WinBackend::default())))
    }
    #[cfg(not(target_os = "windows"))]
    {
        WORKER.get()
    }
}

/// Pause every player observed playing and record it against this recording session.
pub fn begin_for_session(session_id: &str) {
    if session_id.is_empty() {
        return;
    }
    if let Some(worker) = worker() {
        worker.send(Command::Begin(session_id.to_string()));
    }
}

/// Resume media owned by one recording session. `reason` is an outcome code.
pub fn restore_for_session(session_id: &str, reason: &str) {
    if session_id.is_empty() {
        return;
    }
    if let Some(worker) = worker() {
        worker.send(Command::Restore(session_id.to_string(), reason.to_string()));
    }
}

/// Lifecycle cleanup for shutdown and watchdog paths: resume everything any session paused. Never
/// persists ownership for replay after restart.
pub fn restore_any(reason: &str) {
    if let Some(worker) = worker() {
        worker.send(Command::RestoreAny(reason.to_string()));
    }
}

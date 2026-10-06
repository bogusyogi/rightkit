//! Engine sidecar wiring over `rightkit-jobs::EngineClient` and `rightkit-process`.
//!
//! The shell owns only the window-side seam: start the engine off the UI thread, expose typed
//! calls to the webview (`rk_shell_engine_call`), stream engine events to the webview, restart on
//! demand, and shut the engine down in the ordered exit. Framing, multiplexing and the bounded
//! shutdown are `EngineClient`; child ownership is `rightkit-process`. Donor: HeardRight
//! `engine.rs` / `engine_events.rs` (event bridge) and the template's `engine_supervisor`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rightkit_jobs::{EngineClient, JobError};
use rightkit_process::OwnedCommand;
use serde_json::Value;
use tauri::{AppHandle, Emitter, Wry};

use crate::EVENT_ENGINE;

pub type CommandFactory = Arc<dyn Fn() -> OwnedCommand + Send + Sync>;

#[derive(Clone)]
pub struct EngineSpec {
    pub command: CommandFactory,
    pub hello_timeout: Duration,
    pub shutdown_grace: Duration,
    /// Per-call deadline applied by the webview command (None = engine decides).
    pub call_timeout: Option<Duration>,
}

impl EngineSpec {
    pub fn new(command: impl Fn() -> OwnedCommand + Send + Sync + 'static) -> Self {
        Self { command: Arc::new(command), hello_timeout: Duration::from_secs(30), shutdown_grace: Duration::from_secs(3), call_timeout: Some(Duration::from_secs(60)) }
    }
}

pub struct EngineHost {
    spec: EngineSpec,
    client: Mutex<Option<Arc<EngineClient>>>,
    closing: std::sync::atomic::AtomicBool,
    lifecycle: Mutex<Value>,
}

impl EngineHost {
    pub fn new(spec: EngineSpec) -> Self {
        Self { spec, client: Mutex::new(None), closing: Default::default(), lifecycle: Mutex::new(serde_json::json!({"state": "starting"})) }
    }

    /// Spawn the engine (blocking until hello). Replaces a previous client.
    pub fn start(&self) -> Result<Value, JobError> {
        let client = Arc::new(EngineClient::spawn_with((self.spec.command)(), self.spec.hello_timeout)?);
        let hello = client.hello().clone();
        *self.client.lock().unwrap_or_else(|e| e.into_inner()) = Some(client);
        Ok(hello)
    }

    /// Last start outcome (`{state: starting|ready|error, ...}`). The start event can fire before the
    /// webview subscribes, so `rk_shell_ready` and `rk_shell_status` replay this.
    pub fn lifecycle(&self) -> Value {
        self.lifecycle.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub(crate) fn set_lifecycle(&self, v: Value) {
        *self.lifecycle.lock().unwrap_or_else(|e| e.into_inner()) = v;
    }

    pub fn hello(&self) -> Option<Value> {
        self.client.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|c| c.hello().clone())
    }

    fn client(&self) -> Result<Arc<EngineClient>, JobError> {
        if self.closing.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(JobError::new("shutting_down", "the app is exiting"));
        }
        self.client.lock().unwrap_or_else(|e| e.into_inner()).clone().ok_or_else(|| JobError::new("engine_not_running", "engine has not started"))
    }

    /// One call; engine events stream to the webview as `rightkit-shell://engine`
    /// `{call, event}` while it runs. A dead engine is restarted and the call retried
    /// only when the write itself failed (the request provably never reached the engine); a call lost mid-flight is reported, not replayed.
    pub fn call(&self, app: &AppHandle<Wry>, method: &str, params: Value) -> Result<Value, JobError> {
        let attempt = |client: Arc<EngineClient>| {
            let app = app.clone();
            let name = method.to_string();
            let on_event: Box<dyn Fn(&Value) + Send> = Box::new(move |ev| {
                let _ = app.emit(EVENT_ENGINE, serde_json::json!({"call": name, "event": ev}));
            });
            match self.spec.call_timeout {
                Some(t) => client.call_timeout(method, params.clone(), Some(on_event), t),
                None => client.call_with_events(method, params.clone(), Some(on_event)),
            }
        };
        let closing = || self.closing.load(std::sync::atomic::Ordering::SeqCst);
        match attempt(self.client()?) {
            // The request never left this process (write to a dead pipe): safe to retry once on a
            // fresh engine.
            Err(e) if matches!(e.code.as_str(), "io" | "engine_write") && !closing() => {
                self.start()?;
                attempt(self.client()?)
            }
            // The engine died while the call was in flight: it may have partly run, so the call is
            // NOT replayed. Restart so the next call works, and report the failure.
            Err(e) if e.code == "engine_exited" && !closing() => {
                let _ = self.start();
                Err(e)
            }
            other => other,
        }
    }

    /// Ordered-exit step: refuse new calls, then ask the engine to stop (bounded).
    pub fn shutdown(&self) {
        self.closing.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(c) = self.client.lock().unwrap_or_else(|e| e.into_inner()).take() {
            c.shutdown(self.spec.shutdown_grace);
        }
    }
}

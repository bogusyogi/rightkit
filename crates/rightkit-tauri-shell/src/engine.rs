//! Engine sidecar wiring over `rightkit-job-queue::EngineClient` and `rightkit-child-process`.
//!
//! The shell owns only the window-side seam: start the engine off the UI thread, expose typed
//! calls to the webview (`rk_shell_engine_call`), stream engine events to the webview, restart on
//! demand, and shut the engine down in the ordered exit. Framing, multiplexing and the bounded
//! shutdown are `EngineClient`; child ownership is `rightkit-child-process`. Donor: HeardRight
//! `engine.rs` / `engine_events.rs` (event bridge) and the template's `engine_supervisor`.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rightkit_child_process::OwnedCommand;
pub use rightkit_child_process::{RestartMode, RestartPolicy};
use rightkit_job_queue::{
    CallOptions, EngineClient, EngineSupervisor, JobError, Lifecycle, SupervisorConfig, Terminal,
};
pub use rightkit_job_queue::{EventFrame, LifecycleEvent};
use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, Wry};

use crate::EVENT_ENGINE;

pub type CommandFactory = Arc<dyn Fn() -> OwnedCommand + Send + Sync>;

#[derive(Clone)]
pub struct EngineSpec {
    pub command: CommandFactory,
    pub hello_timeout: Duration,
    pub shutdown_grace: Duration,
    /// Per-call deadline. `None` is unbounded in legacy mode; supervised mode uses
    /// the configured supervisor RPC timeout (120 s unless configured otherwise).
    pub call_timeout: Option<Duration>,
}

impl EngineSpec {
    pub fn new(command: impl Fn() -> OwnedCommand + Send + Sync + 'static) -> Self {
        Self {
            command: Arc::new(command),
            hello_timeout: Duration::from_secs(30),
            shutdown_grace: Duration::from_secs(3),
            call_timeout: Some(Duration::from_secs(60)),
        }
    }
}

/// Shell engine policy. The default denies every method: list the methods the webview may
/// call with [`EnginePolicy::allow_methods`], or opt into unrestricted calls explicitly with
/// [`EnginePolicy::allow_any_method`].
#[derive(Clone, Default)]
pub struct EnginePolicy {
    /// Methods the webview may call. `None` (the default) denies every method unless
    /// [`EnginePolicy::allow_any`] is set.
    pub allowed_methods: Option<HashSet<String>>,
    /// Explicit opt-in to unrestricted calls; ignored when `allowed_methods` is set.
    pub allow_any: bool,
    /// Exact method names; missing entries use `EngineSpec::call_timeout`.
    pub method_timeouts: HashMap<String, Duration>,
    /// Opt into job-queue's bounded supervisor (automatic, on-demand or disabled).
    pub restart: Option<RestartPolicy>,
    /// Supervised calls abandon only their wait on timeout instead of killing the child.
    pub keep_alive_on_timeout: bool,
    /// Optional contract, stderr, readiness & handshake configuration. Also enables supervision.
    pub configure_supervisor:
        Option<Arc<dyn Fn(SupervisorConfig) -> SupervisorConfig + Send + Sync>>,
}

impl EnginePolicy {
    pub fn allow_methods(mut self, methods: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.allowed_methods = Some(methods.into_iter().map(Into::into).collect());
        self
    }

    /// Explicit opt-in: the webview may call every engine method. Prefer
    /// [`EnginePolicy::allow_methods`]; this exists for engines whose whole surface is app-safe.
    pub fn allow_any_method(mut self) -> Self {
        self.allow_any = true;
        self
    }

    pub fn method_timeout(mut self, method: impl Into<String>, timeout: Duration) -> Self {
        self.method_timeouts.insert(method.into(), timeout);
        self
    }

    pub fn restart_policy(mut self, policy: RestartPolicy) -> Self {
        self.restart = Some(policy);
        self
    }

    pub fn supervisor_config(
        mut self,
        configure: impl Fn(SupervisorConfig) -> SupervisorConfig + Send + Sync + 'static,
    ) -> Self {
        self.configure_supervisor = Some(Arc::new(configure));
        self
    }

    /// Validate at the native boundary, before obtaining or starting a client.
    pub fn check_method(&self, method: &str) -> Result<(), JobError> {
        let allowed = match &self.allowed_methods {
            Some(methods) => methods.contains(method),
            None => self.allow_any,
        };
        if !allowed {
            return Err(JobError::supervisor(
                "method_not_allowed",
                format!("method {method:?} is not available to the app"),
            ));
        }
        Ok(())
    }

    pub fn timeout_for(&self, method: &str, fallback: Option<Duration>) -> Option<Duration> {
        self.method_timeouts.get(method).copied().or(fallback)
    }
}

/// Additive fields retain the existing `{call, event}` event contract.
#[derive(Debug, Clone, Serialize)]
pub struct EngineEvent {
    pub call: String,
    pub id: Option<u64>,
    pub tag: Option<String>,
    pub event: Value,
}

/// Typed error returned by the tagged IPC command. Legacy IPC keeps string errors.
#[derive(Debug, Clone, Serialize)]
pub struct EngineCallError {
    pub code: String,
    pub message: String,
    /// Distinguishes pre-execution readiness errors from engine replies with matching codes.
    pub origin: &'static str,
    /// Structured M4 error details carried through from the job-queue error, when present.
    /// Absent details stay absent on the wire (never serialised as `null`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl From<JobError> for EngineCallError {
    fn from(e: JobError) -> Self {
        Self {
            details: e.details().cloned(),
            code: e.code,
            message: e.message,
            origin: if e.origin == rightkit_job_queue::ErrorOrigin::Supervisor {
                "supervisor"
            } else {
                "engine"
            },
        }
    }
}

pub type EngineEventSink = Arc<dyn Fn(EngineEvent) + Send + Sync>;

pub struct EngineHost {
    spec: EngineSpec,
    policy: EnginePolicy,
    supervisor: Option<EngineSupervisor>,
    client: Mutex<Option<Arc<EngineClient>>>,
    closing: std::sync::atomic::AtomicBool,
    lifecycle: Mutex<Value>,
}

impl EngineHost {
    /// A host with the default policy, which denies every method; use [`EngineHost::with_policy`]
    /// to permit calls.
    pub fn new(spec: EngineSpec) -> Self {
        Self::with_policy(spec, EnginePolicy::default())
    }

    pub fn with_policy(spec: EngineSpec, policy: EnginePolicy) -> Self {
        let supervisor = if policy.restart.is_some() || policy.configure_supervisor.is_some() {
            let command = spec.command.clone();
            let mut config = SupervisorConfig::new("rk-shell-engine", move || Ok(command()))
                .with_hello_timeout(spec.hello_timeout)
                .with_shutdown_grace(spec.shutdown_grace);
            if let Some(t) = spec.call_timeout {
                config = config.with_rpc_timeout(t);
            }
            if let Some(restart) = policy.restart {
                config = config.with_restart(restart);
            }
            if let Some(configure) = &policy.configure_supervisor {
                config = configure(config);
            }
            Some(EngineSupervisor::new(config))
        } else {
            None
        };
        Self {
            spec,
            policy,
            supervisor,
            client: Mutex::new(None),
            closing: Default::default(),
            lifecycle: Mutex::new(serde_json::json!({"state": "starting"})),
        }
    }

    /// Spawn the engine (blocking until hello). Replaces a previous client.
    pub fn start(&self) -> Result<Value, JobError> {
        if self.closing.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(JobError::supervisor("shutting_down", "the app is exiting"));
        }
        if let Some(supervisor) = &self.supervisor {
            supervisor.start()?;
            return supervisor
                .status()
                .hello
                .ok_or_else(|| JobError::supervisor("engine_not_running", "engine has no hello"));
        }
        let client = Arc::new(EngineClient::spawn_with(
            (self.spec.command)(),
            self.spec.hello_timeout,
        )?);
        let hello = client.hello().clone();
        *self.client.lock().unwrap_or_else(|e| e.into_inner()) = Some(client);
        Ok(hello)
    }

    /// Last start outcome (`{state: starting|ready|error, ...}`). The start event can fire before the
    /// webview subscribes, so `rk_shell_ready` & `rk_shell_engine_status` replay this.
    pub fn lifecycle(&self) -> Value {
        if let Some(s) = &self.supervisor {
            let status = s.status();
            let state = match status.lifecycle {
                Lifecycle::Booting => "starting",
                Lifecycle::Ready => "ready",
                Lifecycle::Recovering => "restarting",
                Lifecycle::Down => match &status.terminal {
                    Some(Terminal::Stopped) => "stopped",
                    Some(Terminal::Failed { .. }) => "failed",
                    None if status.generation == 0 => "starting",
                    None => "restarting",
                },
            };
            return serde_json::json!({"state": state, "hello": status.hello,
            "restarts": status.restarts, "last_error": match status.terminal {
                Some(Terminal::Failed { reason }) => Some(reason),
                _ if status.lifecycle == Lifecycle::Ready => None,
                _ => status.last_exit_reason,
            }});
        }
        self.lifecycle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub(crate) fn set_lifecycle(&self, v: Value) {
        *self.lifecycle.lock().unwrap_or_else(|e| e.into_inner()) = v;
    }

    pub fn hello(&self) -> Option<Value> {
        if let Some(s) = &self.supervisor {
            return s.status().hello;
        }
        self.client
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|c| c.hello().clone())
    }

    fn client(&self) -> Result<Arc<EngineClient>, JobError> {
        if self.closing.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(JobError::supervisor("shutting_down", "the app is exiting"));
        }
        self.client
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| JobError::supervisor("engine_not_running", "engine has not started"))
    }

    /// One call; engine events stream to the webview as `rightkit-shell://engine`
    /// `{call, event}` while it runs. A dead engine is restarted and the call retried
    /// only when the write itself failed (the request provably never reached the engine); a call lost mid-flight is reported, not replayed.
    pub fn call(
        &self,
        app: &AppHandle<Wry>,
        method: &str,
        params: Value,
    ) -> Result<Value, JobError> {
        let app = app.clone();
        self.call_tagged(
            method,
            params,
            None,
            Arc::new(move |event| {
                let _ = app.emit(EVENT_ENGINE, event);
            }),
        )
    }

    /// Native/IPC entry point with caller correlation & request ids. In-flight failures are never replayed.
    pub fn call_tagged(
        &self,
        method: &str,
        params: Value,
        tag: Option<String>,
        sink: EngineEventSink,
    ) -> Result<Value, JobError> {
        self.policy.check_method(method)?;
        if self.closing.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(JobError::supervisor("shutting_down", "the app is exiting"));
        }
        let timeout = self.policy.timeout_for(method, self.spec.call_timeout);
        let on_frame = || {
            let (call, tag, sink) = (method.to_string(), tag.clone(), sink.clone());
            Box::new(move |frame: &EventFrame<'_>| {
                sink(EngineEvent {
                    call: call.clone(),
                    id: Some(frame.id),
                    tag: tag.clone(),
                    event: frame.event.clone(),
                })
            }) as rightkit_job_queue::OnEventFrame
        };
        if let Some(supervisor) = &self.supervisor {
            return supervisor.call_with(
                method,
                params,
                CallOptions {
                    timeout,
                    on_frame: Some(on_frame()),
                    keep_alive_on_timeout: self.policy.keep_alive_on_timeout,
                    ..Default::default()
                },
            );
        }
        let attempt = |client: Arc<EngineClient>| match timeout {
            Some(t) => client.call_timeout_frames(method, params.clone(), Some(on_frame()), t),
            None => client.call_with_frames(method, params.clone(), Some(on_frame())),
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

    /// Subscribe before `start()` to observe recovery even when no call is active.
    pub fn subscribe_lifecycle(&self) -> Option<std::sync::mpsc::Receiver<LifecycleEvent>> {
        self.supervisor.as_ref().map(|s| s.subscribe_lifecycle())
    }

    pub fn is_shutting_down(&self) -> bool {
        self.closing.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Unsolicited engine frames, with no request id. Supervised mode only.
    pub fn subscribe_events(&self) -> Option<std::sync::mpsc::Receiver<Value>> {
        self.supervisor.as_ref().map(|s| s.subscribe_events())
    }

    /// Ordered-exit step: refuse new calls, then ask the engine to stop (bounded).
    pub fn shutdown(&self) {
        self.closing
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(supervisor) = &self.supervisor {
            supervisor.shutdown();
        }
        if let Some(c) = self.client.lock().unwrap_or_else(|e| e.into_inner()).take() {
            c.shutdown(self.spec.shutdown_grace);
        }
    }
}

#[cfg(test)]
mod policy_tests {
    use super::*;

    #[test]
    fn deny_all_default_and_exact_allowlist_with_empty_deny_all() {
        assert_eq!(
            EnginePolicy::default()
                .check_method("anything")
                .unwrap_err()
                .code,
            "method_not_allowed"
        );
        let policy = EnginePolicy::default().allow_methods(["project.show"]);
        assert!(policy.check_method("project.show").is_ok());
        for method in ["project", "project.show.extra", "PROJECT.SHOW"] {
            assert_eq!(
                policy.check_method(method).unwrap_err().code,
                "method_not_allowed"
            );
        }
        assert!(EnginePolicy::default()
            .allow_methods(Vec::<String>::new())
            .check_method("project.show")
            .is_err());
    }

    #[test]
    fn method_deadlines_override_default_only_for_exact_names() {
        let policy = EnginePolicy::default().method_timeout("render", Duration::from_secs(300));
        assert_eq!(
            policy.timeout_for("render", Some(Duration::from_secs(10))),
            Some(Duration::from_secs(300))
        );
        assert_eq!(
            policy.timeout_for("render.cancel", Some(Duration::from_secs(10))),
            Some(Duration::from_secs(10))
        );
        assert_eq!(policy.timeout_for("other", None), None);
    }

    #[test]
    fn denial_precedes_client_acquisition_and_spawn() {
        let host = EngineHost::with_policy(
            EngineSpec::new(|| panic!("denied method spawned engine")),
            EnginePolicy::default().allow_methods(["ok"]),
        );
        assert_eq!(
            host.call_tagged("bad", Value::Null, None, Arc::new(|_| panic!("event")))
                .unwrap_err()
                .code,
            "method_not_allowed"
        );
    }

    #[test]
    fn correlated_event_and_error_wire_shapes() {
        let event = EngineEvent {
            call: "render".into(),
            id: Some(42),
            tag: Some("ui-2".into()),
            event: serde_json::json!({"progress": 1}),
        };
        assert_eq!(
            serde_json::to_value(event).unwrap(),
            serde_json::json!({"call": "render", "id": 42, "tag": "ui-2", "event": {"progress": 1}})
        );
        let reply = EngineCallError::from(JobError::new("not_ready", "engine reply"));
        let readiness = EngineCallError::from(JobError::supervisor("not_ready", "starting"));
        assert_eq!(reply.origin, "engine");
        assert_eq!(readiness.origin, "supervisor");
        let error = EngineCallError::from(JobError::supervisor("method_not_allowed", "denied"));
        assert_eq!(
            serde_json::to_value(error).unwrap(),
            serde_json::json!({"code": "method_not_allowed", "message": "denied", "origin": "supervisor"})
        );
        // M4 details pass through untouched; absent details stay off the wire (above).
        let detailed = EngineCallError::from(
            JobError::new("needs_auth", "sign in")
                .with_details(serde_json::json!({"provider": "x", "nested": {"n": 1}})),
        );
        assert_eq!(
            serde_json::to_value(detailed).unwrap(),
            serde_json::json!({"code": "needs_auth", "message": "sign in", "origin": "engine", "details": {"provider": "x", "nested": {"n": 1}}})
        );
    }
}

//! Sidecar mode: the engine side of the window/engine contract, `rightkit-job-queue`'s
//! `Dispatcher` served over stdio.
//!
//! stdout carries protocol frames only (hello, responses, unsolicited events). The supervisor
//! replays [`INIT_METHOD`] as the first request of every engine process. Requests the registry
//! marks long run on their own thread, so a slow command never holds back a later one; at most
//! [`SidecarOptions::max_in_flight`] requests run at once and the next is refused with `busy`.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rightkit_job_queue::{CommandSet, Dispatcher, JobError, JobResult, Sink};
use serde_json::{json, Value};

use crate::registry::CommandRegistry;
use crate::wire::{EngineError, HEALTH_METHOD, INIT_METHOD};
use crate::EngineSpec;

/// Handles [`INIT_METHOD`]: `(params, first)` where `first` is true only for the very first
/// request of the session (a per-launch secret should be accepted only then).
pub type InitFn = Arc<dyn Fn(&Value, bool) -> Result<Value, EngineError> + Send + Sync>;
/// Extra fields merged into the [`HEALTH_METHOD`] answer.
pub type HealthFn = Arc<dyn Fn() -> Value + Send + Sync>;
/// Called once when the session ends (end of input or `shutdown`).
pub type ShutdownFn = Arc<dyn Fn() + Send + Sync>;

/// Sidecar tuning and hooks.
#[derive(Clone)]
pub struct SidecarOptions {
    /// Requests handled at once; past it a request is refused with `busy`.
    pub max_in_flight: usize,
    /// How long the dispatcher waits for running requests at shutdown.
    pub drain_timeout: Duration,
    /// Frame limit; `None` keeps the dispatcher default.
    pub frame_limit: Option<usize>,
    /// Init hook; without one, init answers `{"ok": true}`.
    pub on_init: Option<InitFn>,
    /// Health hook.
    pub health: Option<HealthFn>,
    /// Shutdown hook.
    pub on_shutdown: Option<ShutdownFn>,
}

impl Default for SidecarOptions {
    fn default() -> Self {
        Self {
            max_in_flight: 64,
            drain_timeout: Duration::from_secs(5),
            frame_limit: None,
            on_init: None,
            health: None,
            on_shutdown: None,
        }
    }
}

impl std::fmt::Debug for SidecarOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SidecarOptions")
            .field("max_in_flight", &self.max_in_flight)
            .field("drain_timeout", &self.drain_timeout)
            .field("frame_limit", &self.frame_limit)
            .finish_non_exhaustive()
    }
}

/// The command set a sidecar serves: the engine's own methods plus every registered command.
pub struct SidecarCommands {
    spec: EngineSpec,
    registry: CommandRegistry,
    options: SidecarOptions,
    in_flight: AtomicUsize,
    requests_seen: AtomicU64,
}

impl SidecarCommands {
    /// A command set over `registry`.
    pub fn new(spec: EngineSpec, registry: CommandRegistry, options: SidecarOptions) -> Self {
        Self {
            spec,
            registry,
            options,
            in_flight: AtomicUsize::new(0),
            requests_seen: AtomicU64::new(0),
        }
    }

    /// Requests running now.
    pub fn in_flight(&self) -> usize {
        self.in_flight.load(Ordering::SeqCst)
    }

    fn health(&self) -> Value {
        let mut out = json!({
            "ok": true,
            "engine": self.spec.name,
            "version": self.spec.version,
            "contract_version": self.spec.contract_version,
            "in_flight": self.in_flight(),
        });
        if let (Some(hook), Some(map)) = (&self.options.health, out.as_object_mut()) {
            if let Value::Object(extra) = hook() {
                map.extend(extra);
            }
        }
        out
    }
}

struct Slot<'a>(&'a AtomicUsize);

impl<'a> Slot<'a> {
    fn take(counter: &'a AtomicUsize, max: usize) -> Option<Self> {
        if counter.fetch_add(1, Ordering::SeqCst) >= max {
            counter.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(Self(counter))
    }
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl CommandSet for SidecarCommands {
    fn methods(&self) -> Vec<String> {
        let mut methods = vec![INIT_METHOD.to_string(), HEALTH_METHOD.to_string()];
        methods.extend(self.registry.names());
        methods
    }

    fn is_long(&self, method: &str, _params: &Value) -> bool {
        self.registry.get(method).is_some_and(|s| s.long)
    }

    fn call(&self, method: &str, params: &Value, dry_run: bool, _emit: &Sink) -> JobResult<Value> {
        let first = self.requests_seen.fetch_add(1, Ordering::SeqCst) == 0;
        let Some(_slot) = Slot::take(&self.in_flight, self.options.max_in_flight) else {
            return Err(JobError::new(
                "busy",
                format!(
                    "{} requests are already running; try again",
                    self.options.max_in_flight
                ),
            ));
        };
        let result = match method {
            INIT_METHOD => match &self.options.on_init {
                Some(hook) => hook(params, first),
                None => Ok(json!({ "ok": true })),
            },
            HEALTH_METHOD => Ok(self.health()),
            name => self.registry.dispatch(name, params, dry_run),
        };
        result.map_err(JobError::from)
    }

    fn shutdown(&self) {
        if let Some(hook) = &self.options.on_shutdown {
            hook();
        }
    }
}

/// The dispatcher a sidecar serves. Take `event_emitter()` from it before [`run`] to send
/// unsolicited events.
pub fn dispatcher(
    spec: &EngineSpec,
    registry: CommandRegistry,
    options: SidecarOptions,
) -> Dispatcher {
    let mut dispatcher = Dispatcher::new(spec.name.clone(), spec.version.clone())
        .with_drain_timeout(options.drain_timeout)
        .with_error_retryable(false);
    if let Some(limit) = options.frame_limit {
        dispatcher = dispatcher.with_frame_limit(limit);
    }
    dispatcher.with(Arc::new(SidecarCommands::new(
        spec.clone(),
        registry,
        options,
    )))
}

/// Serve `dispatcher` over the process's stdin and stdout until end of input or `shutdown`.
pub fn run(dispatcher: Dispatcher) -> std::io::Result<()> {
    rightkit_job_queue::serve(
        Arc::new(dispatcher),
        std::io::stdin().lock(),
        std::io::stdout(),
        false,
    )
}

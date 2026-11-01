// Local image generation service — see docs/plans/local-image-generation.md

pub mod artifacts;
pub mod commands;
pub mod host;
pub mod http;
pub mod machine;
pub mod memory;
pub mod model_store;
pub mod paths;
pub mod profiles;
pub mod provider;
pub mod roots;
pub mod runtime;
pub mod settings;
pub mod store;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use artifacts::ArtifactContext;
pub use profiles::InstallProgress;
use profiles::ProfileSpec;
use provider::{ImgGenRequest, ProviderStatus, SdCppClient};
use rightkit_job_queue::{JobLock, JobLocks};
use runtime::{SdServer, SdServerConfig};
use serde::Serialize;
use serde_json::json;
use store::{JobRecord, JobState, JobUpdate, Store};

pub const JOB_EVENT: &str = "local-image://job";
pub const INSTALL_EVENT: &str = "local-image://install";
const READY_TIMEOUT: Duration = Duration::from_secs(600);
const POLL_INTERVAL: Duration = Duration::from_millis(750);
const MAX_POLL_TRANSPORT_ERRORS: u32 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeState {
    Stopped,
    Starting,
    Ready,
    Degraded,
    MissingBinary,
}

/// Filesystem locations shared by the desktop shell and headless engine.
/// Producers discover these in their host and pass the resolved value here.
#[derive(Debug, Clone)]
pub struct LocalImagePaths {
    pub app_data_dir: std::path::PathBuf,
    pub database_path: std::path::PathBuf,
    pub log_dir: std::path::PathBuf,
    pub staging_dir: std::path::PathBuf,
    pub reference_dir: std::path::PathBuf,
    pub model_root: std::path::PathBuf,
    pub lora_root: std::path::PathBuf,
    pub output_root: std::path::PathBuf,
    pub backend_binary: std::path::PathBuf,
}

pub type EventCallback = std::sync::Arc<dyn Fn(&str, serde_json::Value) + Send + Sync + 'static>;
pub type EventSink = EventCallback;

#[derive(Clone)]
pub struct ServiceOptions {
    pub events: EventCallback,
    pub idle_unload_seconds: std::sync::Arc<dyn Fn() -> u64 + Send + Sync + 'static>,
    /// Restrict worker claims to one engine attempt; `None` owns desktop queue.
    pub worker_request_prefix: Option<String>,
    /// Host cancellation source, keyed by the persisted submission request id.
    /// Closes the engine submit/provider-id-save race without starting another backend.
    pub request_cancelled: Arc<dyn Fn(&str) -> bool + Send + Sync + 'static>,
    /// Headless only (`worker_request_prefix` set): keep the server loaded after a job that
    /// SUCCEEDED, for the next job of the same caller. The caller owns the service, so the server
    /// ends with it (drop / `shutdown`, which is immediate), an idle holder still yields at once
    /// to a waiter (`yield_to_waiter`), and the idle-unload timer remains the backstop. A job that
    /// failed or was cancelled always stops the server.
    pub keep_warm_between_jobs: bool,
    /// Saved `memory_mode` setting (`auto`, `performance`, `balanced`, `low`); `None` = `auto`.
    /// `<PREFIX>_LOCAL_IMAGE_MEMORY_MODE` overrides it. See `memory.rs`.
    pub memory_mode: std::sync::Arc<dyn Fn() -> Option<String> + Send + Sync + 'static>,
}

impl Default for ServiceOptions {
    fn default() -> Self {
        Self {
            events: std::sync::Arc::new(|_, _| {}),
            idle_unload_seconds: std::sync::Arc::new(|| DEFAULT_IDLE_UNLOAD_SECONDS),
            worker_request_prefix: None,
            request_cancelled: Arc::new(|_| false),
            keep_warm_between_jobs: false,
            memory_mode: std::sync::Arc::new(|| None),
        }
    }
}

#[derive(Default)]
struct RuntimeSlot {
    server: Option<SdServer>,
    client: Option<Arc<SdCppClient>>,
    profile_id: Option<String>,
    /// Memory mode the running server was started with (part of the warm-reuse identity).
    memory: Option<memory::Selection>,
    capabilities: Option<serde_json::Value>,
    binary_sha256: Option<String>,
    last_active: Option<Instant>,
    last_error: Option<String>,
    state: RuntimeState,
}

impl Default for RuntimeState {
    fn default() -> Self {
        RuntimeState::Stopped
    }
}

/// Control flags of a profile install in flight.
#[derive(Default)]
struct InstallFlags {
    pause: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
}

struct Inner {
    paths: LocalImagePaths,
    store: Mutex<Store>,
    options: ServiceOptions,
    owner_locks: JobLocks,
    owner: Mutex<Option<JobLock>>,
    has_waited_for_owner: AtomicBool,
    wake: (Mutex<bool>, Condvar),
    cancel: Mutex<HashSet<String>>,
    runtime: Mutex<RuntimeSlot>,
    /// Further request prefixes this worker may claim (a caller running several jobs through one
    /// service, see `keep_warm_between_jobs`). The first is `options.worker_request_prefix`.
    extra_prefixes: Mutex<Vec<String>>,
    /// Profile installs in flight, each with its pause and cancel flags.
    installs: Mutex<HashMap<String, InstallFlags>>,
    shutdown: AtomicBool,
    /// Set by `request_unload`; the worker consumes it on its next idle tick.
    unload_requested: AtomicBool,
    worker: Mutex<Option<JoinHandle<()>>>,
}

#[derive(Clone)]
pub struct LocalImageService {
    inner: Arc<Inner>,
}

#[derive(Debug, Serialize)]
pub struct ConversationSnapshot {
    pub conversation: store::ConversationRecord,
    pub messages: Vec<store::MessageRecord>,
    pub jobs: Vec<JobRecord>,
    pub artifacts: HashMap<String, Vec<store::ArtifactRecord>>,
}

#[derive(Debug, Serialize)]
pub struct JobSnapshot {
    pub job: JobRecord,
    pub events: Vec<store::JobEventRecord>,
    pub artifacts: Vec<store::ArtifactRecord>,
    pub message: Option<store::MessageRecord>,
}

#[derive(Debug, Serialize)]
pub struct RuntimeStatus {
    pub state: RuntimeState,
    pub pid: Option<u32>,
    pub session_id: Option<String>,
    pub profile_id: Option<String>,
    pub port: Option<u16>,
    pub capabilities: Option<serde_json::Value>,
    pub last_error: Option<String>,
    pub backend_binary: String,
    pub backend_present: bool,
    /// sd-server supervisor is loaded and resident.
    pub loaded: bool,
    /// Resident memory of the backend process group, MB.
    pub memory_mb: Option<f64>,
    /// Seconds of idleness after which the backend unloads (0 = keep loaded).
    pub idle_unload_seconds: u64,
    /// Memory mode: what is configured, what it resolves to on this machine, the exact options,
    /// and the mode of the running server (`running`, null when none). Numbers are estimates.
    pub memory: serde_json::Value,
}

/// `RuntimeStatus.memory`. A bad configured value is reported, not hidden.
pub fn memory_status(
    inner_mode: &(dyn Fn() -> Option<String> + Send + Sync),
    running: Option<&memory::Selection>,
) -> serde_json::Value {
    match memory::current(inner_mode().as_deref()) {
        Ok(selection) => json!({
            "requested": selection.requested,
            "source": selection.source,
            "resolved": selection.mode.as_str(),
            "total_gb": selection.total_gb,
            "flags": selection.flags,
            "estimated_peak_gb": selection.estimated_peak_gb,
            "estimate": true,
            "running": running.map(|r| r.mode.as_str()),
            "thresholds_gb": {"low_max": memory::AUTO_LOW_MAX_GB, "balanced_max": memory::AUTO_BALANCED_MAX_GB},
            "estimated_peaks_gb": memory::Mode::ALL.iter().map(|m| (m.as_str().to_string(), m.estimated_peak_gb())).collect::<std::collections::BTreeMap<_, _>>(),
        }),
        Err(error) => json!({"error": error}),
    }
}

/// Idle-unload limit: persisted setting `idle_unload_seconds`, with the
/// `<PREFIX>_LOCAL_IMAGE_IDLE_SECONDS` env override for tests/harnesses.
/// Default 300s; 0 keeps the backend loaded.
pub const DEFAULT_IDLE_UNLOAD_SECONDS: u64 = 300;

pub fn idle_unload_seconds(_paths: &LocalImagePaths) -> u64 {
    if let Some(value) = crate::host::env_var("LOCAL_IMAGE_IDLE_SECONDS") {
        if let Ok(seconds) = value.trim().parse::<u64>() {
            return seconds;
        }
    }
    DEFAULT_IDLE_UNLOAD_SECONDS
}

fn configured_idle_seconds(inner: &Inner) -> u64 {
    crate::host::env_var("LOCAL_IMAGE_IDLE_SECONDS")
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or_else(|| (inner.options.idle_unload_seconds)())
}

impl LocalImageService {
    pub fn start(paths: LocalImagePaths, events: EventCallback) -> Result<Self, String> {
        Self::start_with_options(
            paths,
            ServiceOptions {
                events,
                ..ServiceOptions::default()
            },
        )
    }

    pub fn start_with_options(
        paths: LocalImagePaths,
        options: ServiceOptions,
    ) -> Result<Self, String> {
        for dir in [
            &paths.app_data_dir,
            &paths.log_dir,
            &paths.staging_dir,
            &paths.reference_dir,
            &paths.model_root,
            &paths.output_root,
        ] {
            std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        }
        let store = Store::open(&paths.database_path)?;
        let owner_locks = JobLocks::new(paths.log_dir.join("service-locks"));
        let inner = Arc::new(Inner {
            paths,
            store: Mutex::new(store),
            options,
            owner_locks,
            owner: Mutex::new(None),
            has_waited_for_owner: AtomicBool::new(false),
            wake: (Mutex::new(false), Condvar::new()),
            cancel: Mutex::new(HashSet::new()),
            runtime: Mutex::new(RuntimeSlot::default()),
            extra_prefixes: Mutex::new(Vec::new()),
            installs: Mutex::new(HashMap::new()),
            shutdown: AtomicBool::new(false),
            unload_requested: AtomicBool::new(false),
            worker: Mutex::new(None),
        });
        // Establish ownership and reconcile before callers can submit work.
        // Passive clients return immediately and retry takeover in worker_loop.
        if !ensure_owner(&inner) {
            inner.has_waited_for_owner.store(true, Ordering::SeqCst);
        }
        let worker_inner = inner.clone();
        let handle = thread::Builder::new()
            .name("rightkit-local-gen-worker".to_string())
            .spawn(move || worker_loop(worker_inner))
            .map_err(|e| format!("spawn local image worker: {e}"))?;
        *inner.worker.lock().unwrap() = Some(handle);
        Ok(Self { inner })
    }

    /// Let this headless worker also claim jobs whose request id starts with `prefix`.
    pub fn allow_request_prefix(&self, prefix: String) {
        {
            let mut extra = self.inner.extra_prefixes.lock().unwrap();
            if self.inner.options.worker_request_prefix.as_deref() == Some(prefix.as_str())
                || extra.contains(&prefix)
            {
                return;
            }
            extra.push(prefix);
        }
        self.notify();
    }

    /// Ask the worker to unload the model server now (`local_image.unload`). The worker does it on
    /// its next idle tick (it wakes at once), never under a running job; a queued job defers it.
    /// Waits up to `timeout` for the server to be gone and returns whether it is. A server that is
    /// still busy stays loaded and the request is consumed by the next idle tick.
    pub fn request_unload(&self, timeout: Duration) -> bool {
        self.inner.unload_requested.store(true, Ordering::SeqCst);
        self.notify();
        let deadline = Instant::now() + timeout;
        loop {
            if self.inner.runtime.lock().unwrap().server.is_none() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(25));
        }
    }

    pub fn shutdown(&self) {
        self.inner.shutdown.store(true, Ordering::SeqCst);
        self.notify();
        if let Some(handle) = self.inner.worker.lock().unwrap().take() {
            let _ = handle.join();
        }
        let owned = self.inner.owner.lock().unwrap().is_some();
        if owned {
            let mut runtime = self.inner.runtime.lock().unwrap();
            if let Some(mut server) = runtime.server.take() {
                if let Err(error) = server.stop(&self.inner.paths.log_dir) {
                    rightkit_logs::JsonlStore::new(
                        (&self.inner.paths.log_dir).join(runtime::LOCAL_IMAGE_LOG_FILE),
                        runtime::LOCAL_IMAGE_LOG_MAX_BYTES,
                    )
                    .append_best_effort(
                        &rightkit_logs::EventRecord::new(
                            runtime::log_now_ms(),
                            rightkit_logs::Level::Info,
                            "shutdown_failed",
                        )
                        .with_field("error", serde_json::json!(error)),
                    );
                }
            }
            runtime.client = None;
            runtime.state = RuntimeState::Stopped;
            if self.inner.options.worker_request_prefix.is_none() {
                if let Err(error) = self.inner.store.lock().unwrap().reconcile_shutdown() {
                    rightkit_logs::JsonlStore::new(
                        (&self.inner.paths.log_dir).join(runtime::LOCAL_IMAGE_LOG_FILE),
                        runtime::LOCAL_IMAGE_LOG_MAX_BYTES,
                    )
                    .append_best_effort(
                        &rightkit_logs::EventRecord::new(
                            runtime::log_now_ms(),
                            rightkit_logs::Level::Info,
                            "shutdown_reconcile_failed",
                        )
                        .with_field("error", serde_json::json!(error)),
                    );
                }
            }
        }
        self.inner.owner.lock().unwrap().take();
    }

    pub fn notify(&self) {
        let (lock, cvar) = &self.inner.wake;
        *lock.lock().unwrap() = true;
        cvar.notify_all();
    }

    pub fn paths(&self) -> &LocalImagePaths {
        &self.inner.paths
    }

    pub fn store(&self) -> &Mutex<Store> {
        &self.inner.store
    }

    pub fn runtime_status(&self) -> RuntimeStatus {
        if self.inner.owner.lock().unwrap().is_none() {
            if let Some(instance) = runtime::active_instance(&self.inner.paths.log_dir) {
                let jobs = self
                    .inner
                    .store
                    .lock()
                    .unwrap()
                    .list_jobs(None, 200)
                    .unwrap_or_default();
                let active = jobs.iter().find(|job| {
                    matches!(
                        job.state,
                        JobState::Loading
                            | JobState::Submitting
                            | JobState::Running
                            | JobState::Cancelling
                            | JobState::Finalizing
                    )
                });
                let session_job = jobs
                    .iter()
                    .find(|job| job.runtime_session_id.as_deref() == Some(&instance.session_id));
                return RuntimeStatus {
                    state: if session_job.is_some() {
                        RuntimeState::Ready
                    } else {
                        RuntimeState::Starting
                    },
                    pid: Some(instance.pid),
                    session_id: Some(instance.session_id.clone()),
                    profile_id: active.or(session_job).map(|job| job.profile_id.clone()),
                    port: instance.port,
                    capabilities: None,
                    last_error: None,
                    backend_binary: instance.binary.clone(),
                    backend_present: std::path::Path::new(&instance.binary).is_file(),
                    loaded: true,
                    memory_mb: runtime::group_rss_mb(instance.pid),
                    idle_unload_seconds: configured_idle_seconds(&self.inner),
                    memory: memory_status(&*self.inner.options.memory_mode, None),
                };
            }
        }
        let mut slot = match self.inner.runtime.try_lock() {
            Ok(slot) => slot,
            Err(_) => {
                return RuntimeStatus {
                    state: RuntimeState::Starting,
                    pid: None,
                    session_id: None,
                    profile_id: None,
                    port: None,
                    capabilities: None,
                    last_error: None,
                    backend_binary: artifacts::normalize_fs_path(&self.inner.paths.backend_binary),
                    backend_present: self.inner.paths.backend_binary.is_file(),
                    loaded: false,
                    memory_mb: None,
                    idle_unload_seconds: configured_idle_seconds(&self.inner),
                    memory: memory_status(&*self.inner.options.memory_mode, None),
                };
            }
        };
        if let Some(server) = slot.server.as_mut() {
            if !server.alive() {
                slot.server = None;
                slot.client = None;
                if slot.state == RuntimeState::Ready {
                    slot.state = RuntimeState::Stopped;
                }
            }
        }
        let pid = slot.server.as_ref().map(|s| s.pid());
        RuntimeStatus {
            state: if !self.inner.paths.backend_binary.is_file()
                && slot.state != RuntimeState::Ready
            {
                RuntimeState::MissingBinary
            } else {
                slot.state
            },
            pid,
            session_id: slot.server.as_ref().map(|s| s.session_id().to_string()),
            profile_id: slot.profile_id.clone(),
            port: slot.server.as_ref().map(|s| s.port()),
            capabilities: slot.capabilities.clone(),
            last_error: slot.last_error.clone(),
            backend_binary: artifacts::normalize_fs_path(&self.inner.paths.backend_binary),
            backend_present: self.inner.paths.backend_binary.is_file(),
            loaded: pid.is_some(),
            memory_mb: pid.and_then(runtime::group_rss_mb),
            idle_unload_seconds: configured_idle_seconds(&self.inner),
            memory: memory_status(&*self.inner.options.memory_mode, slot.memory.as_ref()),
        }
    }

    pub fn emit_job(&self, job_id: &str) {
        let store = self.inner.store.lock().unwrap();
        let payload = store
            .job_events(job_id)
            .ok()
            .and_then(|events| events.last().cloned())
            .map(|event| {
                json!({
                    "job_id": event.job_id,
                    "revision": event.revision,
                    "state": event.state,
                    "stage": event.stage,
                    "progress": event.progress,
                    "runtime_session_id": event.runtime_session_id,
                    "error_code": event.error_code,
                })
            });
        drop(store);
        if let Some(payload) = payload {
            (self.inner.options.events)(JOB_EVENT, payload);
        }
    }

    pub fn set_state(&self, job_id: &str, update: &JobUpdate) -> Result<JobRecord, String> {
        let record = {
            let store = self.inner.store.lock().unwrap();
            store.set_state(job_id, update)?
        };
        self.emit_job(job_id);
        Ok(record)
    }

    pub fn request_cancel_flag(&self, job_id: &str) {
        self.inner.cancel.lock().unwrap().insert(job_id.to_string());
    }

    /// Pause a running profile install: partial downloads are kept and
    /// installing again resumes them. Returns whether an install was running.
    pub fn pause_install(&self, profile_id: &str) -> bool {
        match self.inner.installs.lock().unwrap().get(profile_id) {
            Some(flags) => {
                flags.pause.store(true, Ordering::SeqCst);
                true
            }
            None => false,
        }
    }

    /// Cancel a running profile install and discard its partial downloads.
    /// Returns whether an install was running.
    pub fn cancel_install(&self, profile_id: &str) -> bool {
        match self.inner.installs.lock().unwrap().get(profile_id) {
            Some(flags) => {
                flags.cancel.store(true, Ordering::SeqCst);
                true
            }
            None => false,
        }
    }

    pub fn install_profile(&self, profile_id: &str) -> Result<(), String> {
        let spec = profiles::profile(profile_id)?
            .ok_or_else(|| format!("unknown_profile: {profile_id}"))?;
        if commands::profile_unavailable_on_current_backend(spec)? {
            return Err(format!(
                "profile_backend_unavailable: {} is not supported by this backend",
                spec.id
            ));
        }
        let flags = InstallFlags::default();
        let (pause, cancel) = (Arc::clone(&flags.pause), Arc::clone(&flags.cancel));
        {
            let mut installs = self.inner.installs.lock().unwrap();
            if installs.contains_key(profile_id) {
                return Err(format!("install_in_progress: {profile_id}"));
            }
            installs.insert(profile_id.to_string(), flags);
        }
        let inner = self.inner.clone();
        let spec: &'static ProfileSpec = spec;
        thread::Builder::new()
            .name(format!("rightkit-local-gen-install-{profile_id}"))
            .spawn(move || {
                let records = {
                    let store = inner.store.lock().unwrap();
                    store
                        .components_for_profile(&spec.id, spec.revision)
                        .unwrap_or_default()
                };
                let events = inner.options.events.clone();
                let emit = |progress: &InstallProgress| {
                    if let Ok(payload) = serde_json::to_value(progress) {
                        events(INSTALL_EVENT, payload);
                    }
                };
                let persist = |record: &store::ComponentRecord| {
                    let store = inner.store.lock().unwrap();
                    store.upsert_component(record)
                };
                let result = profiles::install_profile(
                    &inner.paths,
                    spec,
                    &records,
                    persist,
                    emit,
                    pause,
                    cancel,
                );
                if let Err(error) = &result {
                    let state = if error.starts_with("cancelled:") {
                        "cancelled"
                    } else if error.starts_with("paused:") {
                        "paused"
                    } else {
                        "failed"
                    };
                    (inner.options.events)(
                        INSTALL_EVENT,
                        serde_json::to_value(InstallProgress {
                            profile_id: spec.id.clone(),
                            component: String::new(),
                            file: String::new(),
                            bytes_done: 0,
                            bytes_total: 0,
                            state: state.to_string(),
                            error: Some(error.clone()),
                        })
                        .unwrap_or_else(|_| serde_json::json!({"state": state})),
                    );
                }
                inner.installs.lock().unwrap().remove(&spec.id);
                result
            })
            .map_err(|e| {
                self.inner.installs.lock().unwrap().remove(profile_id);
                format!("spawn installer: {e}")
            })?;
        Ok(())
    }
}

/// Acquire the process-wide local-image service owner. Passive clients keep
/// their worker alive and retry, so a dead owner is taken over automatically.
fn ensure_owner(inner: &Arc<Inner>) -> bool {
    if inner.owner.lock().unwrap().is_some() {
        return true;
    }
    let lock = match inner.owner_locks.try_lock("local-image-service") {
        Ok(Some(lock)) => lock,
        Ok(None) => return false,
        Err(error) => {
            rightkit_logs::JsonlStore::new(
                (&inner.paths.log_dir).join(runtime::LOCAL_IMAGE_LOG_FILE),
                runtime::LOCAL_IMAGE_LOG_MAX_BYTES,
            )
            .append_best_effort(
                &rightkit_logs::EventRecord::new(
                    runtime::log_now_ms(),
                    rightkit_logs::Level::Info,
                    "service_lock_failed",
                )
                .with_field("error", serde_json::json!(error.to_string())),
            );
            return false;
        }
    };
    // An older installed desktop has a backend identity lock but no service
    // owner lock. Leave its live store/session untouched during upgrade overlap.
    if runtime::active_instance(&inner.paths.log_dir)
        .is_some_and(|instance| instance.owner_pid != std::process::id())
    {
        return false;
    }
    let mut owner = inner.owner.lock().unwrap();
    if owner.is_some() {
        return true;
    }
    *owner = Some(lock);
    drop(owner);

    runtime::reap_orphaned_backend(&inner.paths.log_dir, &inner.paths.backend_binary, false);
    let preserve_queue = inner.options.worker_request_prefix.is_some()
        || inner.has_waited_for_owner.load(Ordering::SeqCst);
    let reconcile = if preserve_queue {
        inner
            .store
            .lock()
            .unwrap()
            .reconcile_startup_preserving_queue()
    } else {
        inner.store.lock().unwrap().reconcile_startup()
    };
    let recovered = match inner.store.lock() {
        Ok(store) => artifacts::recover_finalized(&store, &inner.paths),
        Err(error) => Err(format!("store lock: {error}")),
    };
    rightkit_logs::JsonlStore::new(
        (&inner.paths.log_dir).join(runtime::LOCAL_IMAGE_LOG_FILE),
        runtime::LOCAL_IMAGE_LOG_MAX_BYTES,
    )
    .append_best_effort(
        &rightkit_logs::EventRecord::new(
            runtime::log_now_ms(),
            rightkit_logs::Level::Info,
            "recovery",
        )
        .with_field(
            "interrupted",
            serde_json::json!(reconcile.as_ref().map(|r| r.interrupted).unwrap_or(0)),
        )
        .with_field(
            "cancelled",
            serde_json::json!(reconcile.as_ref().map(|r| r.cancelled).unwrap_or(0)),
        )
        .with_field(
            "finalizing_left",
            serde_json::json!(reconcile.as_ref().map(|r| r.finalizing_left).unwrap_or(0)),
        )
        .with_field(
            "recovered",
            serde_json::json!(recovered.as_ref().ok().copied().unwrap_or(0)),
        )
        .with_field(
            "error",
            serde_json::json!(reconcile
                .err()
                .map(|e| e.to_string())
                .or_else(|| recovered.err())),
        ),
    );
    true
}

fn worker_loop(inner: Arc<Inner>) {
    loop {
        if inner.shutdown.load(Ordering::SeqCst) {
            return;
        }
        if !ensure_owner(&inner) {
            inner.has_waited_for_owner.store(true, Ordering::SeqCst);
            let (lock, cvar) = &inner.wake;
            let signaled = lock.lock().unwrap();
            let _ = cvar.wait_timeout(signaled, Duration::from_millis(500));
            continue;
        }
        let job = {
            let store = inner.store.lock().unwrap();
            let mut claimed =
                store.claim_next_queued_for(inner.options.worker_request_prefix.as_deref());
            if matches!(claimed, Ok(None)) {
                let extra = inner.extra_prefixes.lock().unwrap().clone();
                for prefix in extra {
                    claimed = store.claim_next_queued_for(Some(&prefix));
                    if !matches!(claimed, Ok(None)) {
                        break;
                    }
                }
            }
            claimed
        };
        match job {
            Ok(Some(job)) => {
                process_job(&inner, job);
                let mut slot = inner.runtime.lock().unwrap();
                slot.last_active = Some(Instant::now());
                // The job is over; a warm server is idle. A job waiting for the machine-wide
                // server gets it now, before this worker claims anything else.
                if let Some(machine) = slot.server.as_ref().and_then(|server| server.machine()) {
                    machine.set_state(machine::HolderState::Idle);
                }
                yield_to_waiter(&inner, &mut slot);
            }
            Ok(None) => {
                let idle_seconds = configured_idle_seconds(&inner);
                let mut slot = inner.runtime.lock().unwrap();
                // An idle server yields to a waiting job at once (checked every worker tick,
                // 500 ms); the idle timer below only applies when nobody is waiting.
                yield_to_waiter(&inner, &mut slot);
                let last_active = slot.last_active;
                let forced = inner.unload_requested.swap(false, Ordering::SeqCst);
                let unloaded = if forced {
                    runtime::unload_now(&mut slot.server, &inner.paths.log_dir)
                } else {
                    runtime::unload_if_idle(
                        &mut slot.server,
                        last_active,
                        Duration::from_secs(idle_seconds),
                        &inner.paths.log_dir,
                    )
                };
                match unloaded {
                    Ok(true) => {
                        slot.client = None;
                        slot.capabilities = None;
                        slot.state = RuntimeState::Stopped;
                        slot.last_active = None;
                    }
                    Ok(false) => {}
                    Err(error) => {
                        slot.state = RuntimeState::Degraded;
                        slot.last_error = Some(error);
                        slot.last_active = None;
                    }
                }
                drop(slot);
                let (lock, cvar) = &inner.wake;
                let mut signaled = lock.lock().unwrap();
                *signaled = false;
                let _ = cvar.wait_timeout(signaled, Duration::from_millis(500));
            }
            Err(error) => {
                rightkit_logs::JsonlStore::new(
                    (&inner.paths.log_dir).join(runtime::LOCAL_IMAGE_LOG_FILE),
                    runtime::LOCAL_IMAGE_LOG_MAX_BYTES,
                )
                .append_best_effort(
                    &rightkit_logs::EventRecord::new(
                        runtime::log_now_ms(),
                        rightkit_logs::Level::Info,
                        "job_terminal",
                    )
                    .with_field("error", serde_json::json!(format!("next_queued: {error}"))),
                );
                thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

fn update(inner: &Inner, job_id: &str, state: JobState, stage: &str) -> Result<JobRecord, String> {
    let record = {
        let store = inner.store.lock().unwrap();
        store.set_state(job_id, &JobUpdate::transition(state, stage))?
    };
    emit_job_event(inner, job_id);
    Ok(record)
}

fn emit_job_event(inner: &Inner, job_id: &str) {
    let payload = {
        let store = inner.store.lock().unwrap();
        store
            .job_events(job_id)
            .ok()
            .and_then(|events| events.last().cloned())
            .map(|event| {
                json!({
                    "job_id": event.job_id,
                    "revision": event.revision,
                    "state": event.state,
                    "stage": event.stage,
                    "progress": event.progress,
                    "runtime_session_id": event.runtime_session_id,
                    "error_code": event.error_code,
                })
            })
    };
    if let Some(payload) = payload {
        (inner.options.events)(JOB_EVENT, payload);
    }
}

fn mark_terminal(
    inner: &Inner,
    job: &JobRecord,
    state: JobState,
    stage: &str,
    code: Option<&str>,
    message: Option<&str>,
) {
    let mut upd = JobUpdate::transition(state, stage);
    if code.is_some() || message.is_some() {
        upd.error_code = Some(code.map(ToString::to_string));
        upd.error_message = Some(message.map(ToString::to_string));
    }
    let result = {
        let store = inner.store.lock().unwrap();
        store.set_state(&job.id, &upd)
    };
    if let Err(error) = result {
        rightkit_logs::JsonlStore::new(
            (&inner.paths.log_dir).join(runtime::LOCAL_IMAGE_LOG_FILE),
            runtime::LOCAL_IMAGE_LOG_MAX_BYTES,
        )
        .append_best_effort(
            &rightkit_logs::EventRecord::new(
                runtime::log_now_ms(),
                rightkit_logs::Level::Info,
                "transition_rejected",
            )
            .with_field("job_id", serde_json::json!(job.id))
            .with_field("requested", serde_json::json!(state.as_str()))
            .with_field("error", serde_json::json!(error)),
        );
        return;
    }
    inner.cancel.lock().unwrap().remove(&job.id);
    emit_job_event(inner, &job.id);
    rightkit_logs::JsonlStore::new(
        (&inner.paths.log_dir).join(runtime::LOCAL_IMAGE_LOG_FILE),
        runtime::LOCAL_IMAGE_LOG_MAX_BYTES,
    )
    .append_best_effort(
        &rightkit_logs::EventRecord::new(
            runtime::log_now_ms(),
            rightkit_logs::Level::Info,
            "job_terminal",
        )
        .with_field("job_id", serde_json::json!(job.id))
        .with_field("state", serde_json::json!(state.as_str()))
        .with_field("error_code", serde_json::json!(code))
        .with_field("error_message", serde_json::json!(message)),
    );
}

fn cancel_requested(inner: &Inner, job_id: &str) -> bool {
    if inner.cancel.lock().unwrap().contains(job_id) {
        return true;
    }
    inner
        .store
        .lock()
        .ok()
        .and_then(|store| store.job(job_id).ok().flatten())
        .is_some_and(|job| {
            matches!(job.state, JobState::Cancelling | JobState::Cancelled)
                || (inner.options.request_cancelled)(&job.request_id)
        })
}

enum StartOutcome {
    Ready,
    Cancelled,
    Shutdown,
}

fn ensure_runtime(
    inner: &Arc<Inner>,
    spec: &ProfileSpec,
    job_id: &str,
) -> Result<StartOutcome, String> {
    if commands::profile_unavailable_on_current_backend(spec)? {
        return Err(format!(
            "profile_backend_unavailable: {} is not supported by this backend",
            spec.id
        ));
    }
    // The memory mode is part of the warm-reuse identity, next to the profile: a server started in
    // another mode is stopped before this job, never reused.
    let selection = memory::current((inner.options.memory_mode)().as_deref())?;
    let (binary, args) = {
        let mut slot = inner.runtime.lock().unwrap();
        let same_profile = slot.profile_id.as_deref() == Some(spec.id.as_str())
            && slot.memory.as_ref().map(|m| m.mode) == Some(selection.mode);
        if let Some(server) = slot.server.as_mut() {
            if server.alive() && same_profile {
                // A warm server is about to run this job: the machine-wide holder is busy again.
                if let Some(machine) = server.machine() {
                    machine.set_state(machine::HolderState::Busy);
                }
                return Ok(StartOutcome::Ready);
            }
        }
        if let Some(mut server) = slot.server.take() {
            if slot.memory.as_ref().map(|m| m.mode) != Some(selection.mode) {
                rightkit_logs::JsonlStore::new(
                    (&inner.paths.log_dir).join(runtime::LOCAL_IMAGE_LOG_FILE),
                    runtime::LOCAL_IMAGE_LOG_MAX_BYTES,
                )
                .append_best_effort(
                    &rightkit_logs::EventRecord::new(
                        runtime::log_now_ms(),
                        rightkit_logs::Level::Info,
                        "memory_mode_restart",
                    )
                    .with_field(
                        "from",
                        serde_json::json!(slot.memory.as_ref().map(|m| m.mode.as_str())),
                    )
                    .with_field("to", serde_json::json!(selection.mode.as_str())),
                );
            }
            server.stop(&inner.paths.log_dir)?;
        }
        slot.memory = None;
        slot.client = None;
        slot.capabilities = None;
        slot.binary_sha256 = None;

        if !inner.paths.backend_binary.is_file() {
            slot.state = RuntimeState::MissingBinary;
            slot.last_error = Some(format!(
                "sd-server binary not found at {}",
                inner.paths.backend_binary.display()
            ));
            return Err(format!(
                "backend_missing: {}",
                inner.paths.backend_binary.display()
            ));
        }

        let mut args = profiles::launch_args(spec, &inner.paths)?;
        let actual_hash = store::sha256_file(&inner.paths.backend_binary)?;
        let manifest = profiles::runtime_manifest()?;
        let platform = if cfg!(windows) {
            "windows-x86_64"
        } else if cfg!(target_arch = "aarch64") {
            "macos-aarch64"
        } else {
            "macos-x86_64"
        };
        let entry = &manifest.binaries[platform];
        let approval_hash = if cfg!(target_os = "macos") {
            if entry["approval_hash_algorithm"] != "macho-codesign-strip-linkedit-v1" {
                return Err("backend_unapproved: missing signature-aware runtime identity".into());
            }
            runtime::approval_sha256(&inner.paths.backend_binary)?
        } else {
            actual_hash.clone()
        };
        let approved = entry
            .get(if cfg!(target_os = "macos") {
                "approval_sha256"
            } else {
                "sha256"
            })
            .and_then(|hash| hash.as_str());
        if approved != Some(approval_hash.as_str()) {
            slot.state = RuntimeState::Degraded;
            slot.last_error =
                Some("Backend integrity check failed; install an approved native runtime".into());
            return Err(
                "backend_unapproved: executable differs from packaged runtime manifest".into(),
            );
        }
        // Capability check: a memory mode is offered only when the (already approved) server
        // lists every option it needs. No fallback to another mode.
        if selection.mode != memory::Mode::Performance {
            let supported = memory::supported_flags(&inner.paths.backend_binary).map_err(|e| {
                format!(
                    "runtime_start_failed: memory mode '{}': {e}",
                    selection.mode.as_str()
                )
            })?;
            if let Some(flag) = memory::first_unsupported(selection.mode, &supported) {
                let error = memory::unsupported_error(selection.mode, &flag);
                slot.last_error = Some(error.clone());
                return Err(error);
            }
        }
        args.extend(selection.flags.iter().cloned());
        slot.binary_sha256 = Some(actual_hash);
        slot.state = RuntimeState::Starting;
        slot.last_error = None;
        (inner.paths.backend_binary.clone(), args)
    };

    let port = runtime::pick_loopback_port()?;
    let token = runtime::new_session_token()?;
    let session_id = runtime::new_session_id();
    let cfg = SdServerConfig {
        binary,
        args,
        log_dir: inner.paths.log_dir.clone(),
        memory: json!({
            "requested": selection.requested,
            "source": selection.source,
            "mode": selection.mode.as_str(),
            "total_gb": selection.total_gb,
            "flags": selection.flags,
            "estimated_peak_gb": selection.estimated_peak_gb,
            "estimate": true,
        }),
    };
    if inner.shutdown.load(Ordering::SeqCst) {
        return Ok(StartOutcome::Shutdown);
    }
    if cancel_requested(inner, job_id) {
        return Ok(StartOutcome::Cancelled);
    }
    // One model server per MACHINE (not per data folder): two ~10 GB servers exhausted memory.
    // Wait a bounded time for a neighbouring job to finish and unload, then fail typed.
    let machine = {
        let data_dir = inner
            .paths
            .app_data_dir
            .parent()
            .unwrap_or(&inner.paths.app_data_dir)
            .to_path_buf();
        let mut announced = false;
        let acquired = machine::MachineLock::acquire(
            &spec.id,
            &data_dir,
            Some(job_id),
            machine::wait_budget(),
            |holder| {
                let mut waiting = JobUpdate::transition(
                    JobState::Loading,
                    "waiting for the local image model server (in use by another job)",
                );
                waiting.progress = None;
                let _ = inner.store.lock().unwrap().set_state(job_id, &waiting);
                emit_job_event(inner, job_id);
                rightkit_logs::JsonlStore::new(
                    (&inner.paths.log_dir).join(runtime::LOCAL_IMAGE_LOG_FILE),
                    runtime::LOCAL_IMAGE_LOG_MAX_BYTES,
                )
                .append_best_effort(
                    &rightkit_logs::EventRecord::new(
                        runtime::log_now_ms(),
                        rightkit_logs::Level::Info,
                        "model_server_wait",
                    )
                    .with_field("job_id", serde_json::json!(job_id))
                    .with_field("holder", serde_json::json!(holder.to_json()))
                    .with_field("first", serde_json::json!(!announced)),
                );
                announced = true;
            },
            || inner.shutdown.load(Ordering::SeqCst) || cancel_requested(inner, job_id),
        );
        match acquired {
            Ok(lock) => lock,
            Err(machine::AcquireError::Cancelled) => {
                return Ok(if inner.shutdown.load(Ordering::SeqCst) {
                    StartOutcome::Shutdown
                } else {
                    StartOutcome::Cancelled
                });
            }
            Err(error) => {
                let message = error.message();
                rightkit_logs::JsonlStore::new(
                    (&inner.paths.log_dir).join(runtime::LOCAL_IMAGE_LOG_FILE),
                    runtime::LOCAL_IMAGE_LOG_MAX_BYTES,
                )
                .append_best_effort(
                    &rightkit_logs::EventRecord::new(
                        runtime::log_now_ms(),
                        rightkit_logs::Level::Info,
                        "model_server_busy",
                    )
                    .with_field("job_id", serde_json::json!(job_id))
                    .with_field("error", serde_json::json!(message)),
                );
                return Err(message);
            }
        }
    };
    if let Some(old) = machine.took_over.as_ref() {
        rightkit_logs::JsonlStore::new(
            (&inner.paths.log_dir).join(runtime::LOCAL_IMAGE_LOG_FILE),
            runtime::LOCAL_IMAGE_LOG_MAX_BYTES,
        )
        .append_best_effort(
            &rightkit_logs::EventRecord::new(
                runtime::log_now_ms(),
                rightkit_logs::Level::Info,
                "machine_lock_takeover",
            )
            .with_field("previous_holder", serde_json::json!(old.to_json()))
            .with_field(
                "stale_server",
                serde_json::json!(machine.stale_server.as_ref().map(|s| s.to_json())),
            ),
        );
    }
    let mut server = SdServer::start(&cfg, port, &token, &session_id, Some(machine))?;
    // Publish ownership while loading; readiness is not required to cancel.
    let mut loading = JobUpdate::transition(JobState::Loading, "backend_loading");
    loading.runtime_session_id = Some(Some(session_id.clone()));
    let published = inner.store.lock().unwrap().set_state(job_id, &loading);
    if published.is_err() && cancel_requested(inner, job_id) {
        server.stop(&inner.paths.log_dir)?;
        return Ok(StartOutcome::Cancelled);
    }
    published?;
    emit_job_event(inner, job_id);
    rightkit_logs::JsonlStore::new(
        (&inner.paths.log_dir).join(runtime::LOCAL_IMAGE_LOG_FILE),
        runtime::LOCAL_IMAGE_LOG_MAX_BYTES,
    )
    .append_best_effort(
        &rightkit_logs::EventRecord::new(
            runtime::log_now_ms(),
            rightkit_logs::Level::Info,
            "model_load",
        )
        .with_field("session", serde_json::json!(session_id))
        .with_field("profile", serde_json::json!(spec.id))
        .with_field("memory_mode", serde_json::json!(selection.mode.as_str())),
    );
    let client = SdCppClient::new(port, &token);
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        if inner.shutdown.load(Ordering::SeqCst) {
            server.stop(&inner.paths.log_dir)?;
            return Ok(StartOutcome::Shutdown);
        }
        if cancel_requested(inner, job_id) {
            server.stop(&inner.paths.log_dir)?;
            return Ok(StartOutcome::Cancelled);
        }
        let slice = Duration::from_secs(5).min(deadline.saturating_duration_since(Instant::now()));
        if slice.is_zero() {
            server.stop(&inner.paths.log_dir)?;
            let mut slot = inner.runtime.lock().unwrap();
            slot.state = RuntimeState::Degraded;
            slot.last_error = Some("sd-server not ready before deadline".to_string());
            return Err("runtime_start_failed: sd-server not ready before deadline".to_string());
        }
        match server.wait_ready(&client, slice, &inner.paths.log_dir, || {
            inner.shutdown.load(Ordering::SeqCst) || cancel_requested(inner, job_id)
        }) {
            Ok(validated) => {
                let mut slot = inner.runtime.lock().unwrap();
                slot.capabilities = Some(validated.capabilities);
                slot.binary_sha256 = Some(server.binary_sha256().to_string());
                slot.client = Some(Arc::new(client));
                slot.server = Some(server);
                slot.profile_id = Some(spec.id.clone());
                slot.memory = Some(selection.clone());
                slot.last_active = Some(Instant::now());
                slot.state = RuntimeState::Ready;
                return Ok(StartOutcome::Ready);
            }
            Err(error) => {
                if inner.shutdown.load(Ordering::SeqCst) {
                    server.stop(&inner.paths.log_dir)?;
                    return Ok(StartOutcome::Shutdown);
                }
                if cancel_requested(inner, job_id) {
                    server.stop(&inner.paths.log_dir)?;
                    return Ok(StartOutcome::Cancelled);
                }
                if error.starts_with("sd-server exited before ready")
                    || error.starts_with("sd-server session verification failed")
                {
                    server.stop(&inner.paths.log_dir)?;
                    let mut slot = inner.runtime.lock().unwrap();
                    slot.state = RuntimeState::Degraded;
                    slot.last_error = Some(error.clone());
                    // A server that died naming one of the mode's options rejected it: typed
                    // error naming the option, no fallback.
                    if let Some(flag) = memory::rejected_flag(selection.mode, &error) {
                        return Err(memory::unsupported_error(selection.mode, &flag));
                    }
                    return Err(format!("runtime_start_failed: {error}"));
                }
            }
        }
    }
}

fn process_job(inner: &Arc<Inner>, job: JobRecord) {
    // Headless workers own a job-scoped backend. Desktop workers alone may
    // retain a warm backend under their live owner's idle-unload policy. A headless caller that
    // asked for `keep_warm_between_jobs` keeps it only after a job that succeeded.
    struct JobRuntime<'a>(&'a Inner, String);
    impl Drop for JobRuntime<'_> {
        fn drop(&mut self) {
            if self.0.options.worker_request_prefix.is_none() {
                return;
            }
            if self.0.options.keep_warm_between_jobs {
                let succeeded = self
                    .0
                    .store
                    .lock()
                    .ok()
                    .and_then(|store| store.job(&self.1).ok().flatten())
                    .is_some_and(|job| job.state == JobState::Succeeded);
                if succeeded && !self.0.shutdown.load(Ordering::SeqCst) {
                    return;
                }
            }
            if let Err(error) = stop_runtime(self.0) {
                rightkit_logs::JsonlStore::new(
                    (&self.0.paths.log_dir).join(runtime::LOCAL_IMAGE_LOG_FILE),
                    runtime::LOCAL_IMAGE_LOG_MAX_BYTES,
                )
                .append_best_effort(
                    &rightkit_logs::EventRecord::new(
                        runtime::log_now_ms(),
                        rightkit_logs::Level::Info,
                        "job_cleanup_failed",
                    )
                    .with_field("error", serde_json::json!(error)),
                );
            }
        }
    }
    let _runtime = JobRuntime(inner, job.id.clone());
    process_job_inner(inner, job);
}

fn process_job_inner(inner: &Arc<Inner>, job: JobRecord) {
    let Some(spec) = ({
        let inner = inner.as_ref();
        match profiles::profile(&job.profile_id) {
            Ok(spec) => spec,
            Err(error) => {
                mark_terminal(
                    inner,
                    &job,
                    JobState::Failed,
                    "failed",
                    Some("catalogue_error"),
                    Some(&error),
                );
                return;
            }
        }
    }) else {
        mark_terminal(
            inner,
            &job,
            JobState::Failed,
            "failed",
            Some("unknown_profile"),
            Some(&format!("no such profile: {}", job.profile_id)),
        );
        return;
    };

    emit_job_event(inner, &job.id);
    if spec.revision != job.profile_revision {
        mark_terminal(
            inner,
            &job,
            JobState::Interrupted,
            "interrupted",
            Some("profile_changed"),
            Some("Profile revision changed; submit a new job"),
        );
        return;
    }
    if let Some(frozen) = job.overrides.get("_component_hashes") {
        let hashes = profiles::component_hashes_cancellable(spec, &inner.paths, || {
            inner.shutdown.load(Ordering::SeqCst) || cancel_requested(inner, &job.id)
        });
        let hashes = match hashes {
            Ok(hashes) => hashes,
            Err(_) if inner.shutdown.load(Ordering::SeqCst) => return,
            Err(_) if cancel_requested(inner, &job.id) => {
                finish_cancel(inner, &job);
                return;
            }
            Err(error) => {
                mark_terminal(
                    inner,
                    &job,
                    JobState::Failed,
                    "failed",
                    Some("component_changed"),
                    Some(&error),
                );
                return;
            }
        };
        let actual: HashMap<String, String> = hashes.into_iter().collect();
        if serde_json::to_value(actual).ok().as_ref() != Some(frozen) {
            mark_terminal(
                inner,
                &job,
                JobState::Failed,
                "failed",
                Some("component_changed"),
                Some("Installed model files differ from submitted job"),
            );
            return;
        }
    }
    if cancel_requested(inner, &job.id) {
        finish_cancel(inner, &job);
        return;
    }
    match ensure_runtime(inner, spec, &job.id) {
        Ok(StartOutcome::Ready) => {}
        Ok(StartOutcome::Cancelled) => {
            finish_cancel(inner, &job);
            return;
        }
        Ok(StartOutcome::Shutdown) => return,
        Err(error) => {
            let code = if error.starts_with("profile_backend_unavailable") {
                "profile_backend_unavailable"
            } else if error.starts_with("model_not_installed") {
                "model_not_installed"
            } else if error.starts_with("backend_missing") {
                "backend_missing"
            } else if error.starts_with("memory_mode_unsupported") {
                "memory_mode_unsupported"
            } else if error.starts_with(machine::BUSY_CODE) {
                machine::BUSY_CODE
            } else {
                "runtime_start_failed"
            };
            mark_terminal(
                inner,
                &job,
                JobState::Failed,
                "failed",
                Some(code),
                Some(&error),
            );
            return;
        }
    }

    let (client, session_id) = {
        let slot = inner.runtime.lock().unwrap();
        (
            slot.client
                .clone()
                .unwrap_or_else(|| Arc::new(SdCppClient::new(0, ""))),
            slot.server.as_ref().map(|s| s.session_id().to_string()),
        )
    };

    if cancel_requested(inner, &job.id) {
        finish_cancel(inner, &job);
        return;
    }

    let request = match build_img_request(inner, &job, spec) {
        Ok(request) => request,
        Err(error) => {
            mark_terminal(
                inner,
                &job,
                JobState::Failed,
                "failed",
                Some("request_invalid"),
                Some(&error),
            );
            return;
        }
    };
    let dimension_check = {
        let slot = inner.runtime.lock().unwrap();
        slot.capabilities
            .as_ref()
            .ok_or_else(|| "capabilities_missing".to_string())
            .and_then(|caps| validate_dimensions(&job, caps))
    };
    if let Err(error) = dimension_check {
        mark_terminal(
            inner,
            &job,
            JobState::Failed,
            "failed",
            Some("invalid_dimensions"),
            Some(&error),
        );
        return;
    }

    if update(inner, &job.id, JobState::Submitting, "submitting").is_err() {
        return;
    }
    rightkit_logs::JsonlStore::new(
        (&inner.paths.log_dir).join(runtime::LOCAL_IMAGE_LOG_FILE),
        runtime::LOCAL_IMAGE_LOG_MAX_BYTES,
    )
    .append_best_effort(
        &rightkit_logs::EventRecord::new(
            runtime::log_now_ms(),
            rightkit_logs::Level::Info,
            "job_submit",
        )
        .with_field("job_id", serde_json::json!(job.id))
        .with_field("profile", serde_json::json!(job.profile_id))
        .with_field("session", serde_json::json!(session_id)),
    );
    let provider_job_id = match client.submit_img_gen(&request) {
        Ok(id) => id,
        Err(error) => {
            match error.code {
                "transport" | "http" => mark_terminal(
                    inner,
                    &job,
                    JobState::Interrupted,
                    "interrupted",
                    Some("submit_ambiguous"),
                    Some(&format!("submission outcome unknown: {}", error.message)),
                ),
                _ => mark_terminal(
                    inner,
                    &job,
                    JobState::Failed,
                    "failed",
                    Some(error.code),
                    Some(&error.message),
                ),
            }
            return;
        }
    };

    {
        let mut upd = JobUpdate::transition(JobState::Running, "generating");
        upd.provider_job_id = Some(Some(provider_job_id.clone()));
        upd.runtime_session_id = session_id.clone().map(Some);
        let store = inner.store.lock().unwrap();
        let _ = store.set_state(&job.id, &upd);
    }
    emit_job_event(inner, &job.id);

    let started = Instant::now();
    let mut transport_errors = 0u32;
    loop {
        if inner.shutdown.load(Ordering::SeqCst) {
            return;
        }
        if cancel_requested(inner, &job.id) {
            cancel_active(inner, &job, &provider_job_id, &client);
            return;
        }
        if let Some(server) = inner.runtime.lock().unwrap().server.as_mut() {
            if !server.alive() {
                mark_terminal(
                    inner,
                    &job,
                    JobState::Interrupted,
                    "interrupted",
                    Some("backend_exit"),
                    Some("sd-server exited during generation"),
                );
                return;
            }
            // Busy: keep the lock, but keep the holder record's `waiters` current for readers.
            if let Some(machine) = server.machine() {
                machine.refresh();
            }
        }
        match client.job(&provider_job_id) {
            Ok(provider_job) => {
                transport_errors = 0;
                match provider_job.status {
                    ProviderStatus::Queued => {
                        let _ = update(inner, &job.id, JobState::Running, "backend_queued");
                    }
                    ProviderStatus::Generating => {
                        let _ = update(inner, &job.id, JobState::Running, "generating");
                    }
                    ProviderStatus::Completed { images, .. } => {
                        finalize(
                            inner,
                            &job,
                            spec,
                            &provider_job_id,
                            session_id.as_deref(),
                            started,
                            images,
                        );
                        return;
                    }
                    ProviderStatus::Failed { code, message } => {
                        mark_terminal(
                            inner,
                            &job,
                            JobState::Failed,
                            "failed",
                            Some(&format!("backend_{code}")),
                            Some(&message),
                        );
                        return;
                    }
                    ProviderStatus::Cancelled => {
                        mark_terminal(inner, &job, JobState::Cancelled, "cancelled", None, None);
                        return;
                    }
                }
            }
            Err(error) => match error.code {
                "gone" | "not_found" => {
                    mark_terminal(
                        inner,
                        &job,
                        JobState::Interrupted,
                        "interrupted",
                        Some("backend_job_lost"),
                        Some("provider job expired or missing"),
                    );
                    return;
                }
                "auth" => {
                    mark_terminal(
                        inner,
                        &job,
                        JobState::Interrupted,
                        "interrupted",
                        Some("backend_auth"),
                        Some("session token rejected"),
                    );
                    return;
                }
                _ => {
                    transport_errors += 1;
                    if transport_errors > MAX_POLL_TRANSPORT_ERRORS {
                        mark_terminal(
                            inner,
                            &job,
                            JobState::Interrupted,
                            "interrupted",
                            Some("backend_unreachable"),
                            Some(&error.message),
                        );
                        return;
                    }
                }
            },
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn validate_dimensions(job: &JobRecord, caps: &serde_json::Value) -> Result<(), String> {
    let limits = &caps["limits"];
    for (dimension, size) in [("width", job.width), ("height", job.height)] {
        let min = limits
            .get(format!("min_{dimension}"))
            .and_then(|v| v.as_i64())
            .ok_or("capabilities_missing: dimension minimum")?;
        let max = limits
            .get(format!("max_{dimension}"))
            .and_then(|v| v.as_i64())
            .ok_or("capabilities_missing: dimension maximum")?;
        if !(min..=max).contains(&size) {
            return Err(format!("{dimension} must be within {min}..{max}"));
        }
    }
    Ok(())
}

fn build_img_request(
    inner: &Arc<Inner>,
    job: &JobRecord,
    spec: &ProfileSpec,
) -> Result<ImgGenRequest, String> {
    let overrides = &job.overrides;
    let num = |key: &str| overrides.get(key).and_then(|v| v.as_f64());
    let text = |key: &str| {
        overrides
            .get(key)
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };
    let defaults = &spec.defaults;
    let references = {
        let store = inner.store.lock().unwrap();
        store.references_by_ids(&job.reference_ids)?
    };
    if references.len() != job.reference_ids.len() {
        return Err("reference_missing: edit reference unavailable".into());
    }
    let refs: Vec<String> = references
        .into_iter()
        .map(|reference| {
            let bytes = std::fs::read(&reference.stored_path)
                .map_err(|e| format!("reference_missing: {e}"))?;
            if bytes.len() as i64 != reference.bytes
                || store::sha256_hex(&bytes) != reference.sha256
            {
                return Err("reference_corrupt: stored edit reference changed".to_string());
            }
            Ok(provider::encode_ref_image(&bytes, &reference.mime))
        })
        .collect::<Result<_, String>>()?;
    Ok(ImgGenRequest {
        prompt: job.prompt.clone(),
        negative_prompt: text("negative_prompt").unwrap_or_else(|| job.negative_prompt.clone()),
        width: job.width as u32,
        height: job.height as u32,
        seed: job.seed,
        ref_images: refs,
        sample_steps: num("sample_steps").unwrap_or(defaults.sample_steps as f64) as u32,
        sample_method: text("sample_method").unwrap_or_else(|| defaults.sample_method.clone()),
        scheduler: if overrides.get("scheduler").is_some() {
            text("scheduler")
        } else {
            defaults.scheduler.clone()
        },
        txt_cfg: num("txt_cfg").unwrap_or(defaults.txt_cfg),
    })
}

/// The machine-wide yield rule: when the server is idle and a live job is waiting for the
/// lock, unload the server and release the lock now. Returns true when it did. A busy server
/// is never touched here (its job has not ended), and with nobody waiting nothing changes.
fn yield_to_waiter(inner: &Inner, slot: &mut RuntimeSlot) -> bool {
    let due = slot
        .server
        .as_ref()
        .and_then(|server| server.machine())
        .is_some_and(|machine| machine.yield_due());
    if !due {
        return false;
    }
    if let Some(mut server) = slot.server.take() {
        // `stop` drops the machine lock last; on error the Drop of `server` still releases it.
        match server.stop(&inner.paths.log_dir) {
            Ok(()) => {
                rightkit_logs::JsonlStore::new(
                    (&inner.paths.log_dir).join(runtime::LOCAL_IMAGE_LOG_FILE),
                    runtime::LOCAL_IMAGE_LOG_MAX_BYTES,
                )
                .append_best_effort(
                    &rightkit_logs::EventRecord::new(
                        runtime::log_now_ms(),
                        rightkit_logs::Level::Info,
                        "runtime_yield",
                    )
                    .with_field(
                        "reason",
                        serde_json::json!("job waiting for the machine-wide model server"),
                    ),
                );
                slot.state = RuntimeState::Stopped;
            }
            Err(error) => {
                slot.state = RuntimeState::Degraded;
                slot.last_error = Some(error);
            }
        }
    }
    slot.client = None;
    slot.capabilities = None;
    slot.last_active = None;
    true
}

fn stop_runtime(inner: &Inner) -> Result<(), String> {
    let mut slot = inner.runtime.lock().unwrap();
    let stopped = slot
        .server
        .take()
        .map(|mut server| server.stop(&inner.paths.log_dir))
        .transpose();
    slot.client = None;
    slot.capabilities = None;
    slot.profile_id = None;
    slot.last_active = None;
    slot.state = RuntimeState::Stopped;
    if let Err(error) = stopped {
        slot.state = RuntimeState::Degraded;
        slot.last_error = Some(error.clone());
        return Err(error);
    }
    Ok(())
}

fn cancel_active(
    inner: &Arc<Inner>,
    job: &JobRecord,
    provider_job_id: &str,
    _client: &Arc<SdCppClient>,
) {
    rightkit_logs::JsonlStore::new(
        (&inner.paths.log_dir).join(runtime::LOCAL_IMAGE_LOG_FILE),
        runtime::LOCAL_IMAGE_LOG_MAX_BYTES,
    )
    .append_best_effort(
        &rightkit_logs::EventRecord::new(
            runtime::log_now_ms(),
            rightkit_logs::Level::Info,
            "cancel_requested",
        )
        .with_field("job_id", serde_json::json!(job.id))
        .with_field("provider_job_id", serde_json::json!(provider_job_id)),
    );
    // Cancellation ends the owned process tree even when HTTP cancellation
    // would succeed: a cancelled job must never keep a resident backend.
    finish_cancel(inner, job);
}

fn finish_cancel(inner: &Arc<Inner>, job: &JobRecord) {
    if let Err(error) = stop_runtime(inner) {
        mark_terminal(
            inner,
            job,
            JobState::Interrupted,
            "interrupted",
            Some("backend_stop_failed"),
            Some(&error),
        );
        return;
    }
    inner.cancel.lock().unwrap().remove(&job.id);
    mark_terminal(inner, job, JobState::Cancelled, "cancelled", None, None);
}

fn finalize(
    inner: &Arc<Inner>,
    job: &JobRecord,
    spec: &ProfileSpec,
    provider_job_id: &str,
    session_id: Option<&str>,
    started: Instant,
    images_b64: Vec<String>,
) {
    if update(inner, &job.id, JobState::Finalizing, "finalizing").is_err() {
        return;
    }
    let decode = |b64: &String| -> Result<Vec<u8>, String> {
        {
            let mut t: String = b64
                .chars()
                .filter(|c| !c.is_ascii_whitespace() && *c != '=')
                .collect();
            while t.len() % 4 != 0 {
                t.push('=');
            }
            rightkit_crypto::base64_decode(&t).map_err(|e| format!("decode b64: {e}"))
        }
    };
    let images: Vec<Vec<u8>> = match images_b64.iter().map(decode).collect() {
        Ok(images) => images,
        Err(error) => {
            mark_terminal(
                inner,
                job,
                JobState::Failed,
                "failed",
                Some("decode"),
                Some(&error),
            );
            return;
        }
    };
    let manifest = profiles::runtime_manifest().ok();
    let (backend_binary_sha256, capabilities) = {
        let slot = inner.runtime.lock().unwrap();
        (
            slot.binary_sha256.clone().unwrap_or_default(),
            slot.capabilities.clone().unwrap_or(serde_json::Value::Null),
        )
    };
    let ctx = ArtifactContext {
        profile_id: spec.id.clone(),
        profile_revision: spec.revision,
        component_hashes: job
            .overrides
            .get("_component_hashes")
            .and_then(|v| serde_json::from_value::<HashMap<String, String>>(v.clone()).ok())
            .map(|map| map.into_iter().collect())
            .unwrap_or_else(|| profiles::component_hashes(spec, &inner.paths)),
        backend_pin: manifest
            .map(|m| m.upstream.tag.clone())
            .unwrap_or_else(|| "unknown".to_string()),
        backend_commit: manifest
            .map(|m| m.upstream.commit.clone())
            .unwrap_or_else(|| "unknown".to_string()),
        backend_binary_sha256,
        capabilities,
        provider_job_id: Some(provider_job_id.to_string()),
        runtime_session_id: session_id.map(str::to_string),
        steps: job
            .overrides
            .get("sample_steps")
            .and_then(|v| v.as_u64())
            .unwrap_or(spec.defaults.sample_steps as u64) as u32,
        sample_method: job
            .overrides
            .get("sample_method")
            .and_then(|v| v.as_str())
            .unwrap_or(&spec.defaults.sample_method)
            .to_string(),
        scheduler: if job.overrides.get("scheduler").is_some() {
            job.overrides
                .get("scheduler")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        } else {
            job.overrides
                .get("scheduler")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .or_else(|| spec.defaults.scheduler.clone())
        },
        txt_cfg: job
            .overrides
            .get("txt_cfg")
            .and_then(|v| v.as_f64())
            .unwrap_or(spec.defaults.txt_cfg),
        negative_prompt: job
            .overrides
            .get("negative_prompt")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| Some(job.negative_prompt.clone())),
        queued_at: job.created_at.clone(),
        started_at: job.updated_at.clone(),
        finished_at: store::now_iso(),
        generate_ms: started.elapsed().as_millis() as u64,
    };
    match artifacts::prepare_images(&inner.paths, job, &ctx, &images) {
        Ok(prepared) => {
            let committed = inner
                .store
                .lock()
                .unwrap()
                .complete_finalization(&job.id, &prepared.records);
            if let Err(error) = committed {
                // Published files remain recoverable until their database commit succeeds.
                let mut update = JobUpdate::transition(JobState::Finalizing, "recovery_required");
                update.error_code = Some(Some("finalize_commit".into()));
                update.error_message = Some(Some(error.clone()));
                let _ = inner.store.lock().unwrap().set_state(&job.id, &update);
                inner.cancel.lock().unwrap().remove(&job.id);
                rightkit_logs::JsonlStore::new(
                    (&inner.paths.log_dir).join(runtime::LOCAL_IMAGE_LOG_FILE),
                    runtime::LOCAL_IMAGE_LOG_MAX_BYTES,
                )
                .append_best_effort(
                    &rightkit_logs::EventRecord::new(
                        runtime::log_now_ms(),
                        rightkit_logs::Level::Info,
                        "recovery_pending",
                    )
                    .with_field("job_id", serde_json::json!(job.id))
                    .with_field("error", serde_json::json!(error)),
                );
                emit_job_event(inner, &job.id);
                return;
            }
            let records = prepared.records;
            rightkit_logs::JsonlStore::new(
                (&inner.paths.log_dir).join(runtime::LOCAL_IMAGE_LOG_FILE),
                runtime::LOCAL_IMAGE_LOG_MAX_BYTES,
            )
            .append_best_effort(
                &rightkit_logs::EventRecord::new(
                    runtime::log_now_ms(),
                    rightkit_logs::Level::Info,
                    "artifact_commit",
                )
                .with_field("job_id", serde_json::json!(job.id))
                .with_field(
                    "artifacts",
                    serde_json::json!(records.iter().map(|r| r.path.clone()).collect::<Vec<_>>()),
                ),
            );
            emit_job_event(inner, &job.id);
            inner.cancel.lock().unwrap().remove(&job.id);
            rightkit_logs::JsonlStore::new(
                (&inner.paths.log_dir).join(runtime::LOCAL_IMAGE_LOG_FILE),
                runtime::LOCAL_IMAGE_LOG_MAX_BYTES,
            )
            .append_best_effort(
                &rightkit_logs::EventRecord::new(
                    runtime::log_now_ms(),
                    rightkit_logs::Level::Info,
                    "job_terminal",
                )
                .with_field("job_id", serde_json::json!(job.id))
                .with_field("state", serde_json::json!("succeeded")),
            );
        }
        Err(error) => {
            mark_terminal(
                inner,
                job,
                JobState::Failed,
                "failed",
                Some("finalize"),
                Some(&error),
            );
        }
    }
}

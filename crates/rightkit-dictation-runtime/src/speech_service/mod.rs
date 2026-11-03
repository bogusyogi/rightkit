//! Machine-wide Orthic Labs speech service hosted by the engine process.
//!
//! The model loads once (the resident worker); other apps send file
//! transcription requests over a local current-user-only endpoint. The
//! contract (roles, discovery files, endpoint names) lives in
//! `rightkit_speech_client`; this module owns I/O.

mod conn;
mod files;
pub(crate) mod lease;
mod peer_trust;
mod transport;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use rightkit_dictation_models::shared::store::lease::{try_acquire_compat, ModelLease};
use rightkit_speech_client::{
    endpoint_for, installed_heardright_path, service_dir, takeover_request_path, HostRole,
    InstalledEngine, ServiceRecord, TakeoverRequest, ERROR_HOST_HANDOFF, SERVICE_PROTOCOL_MAJOR,
    SERVICE_PROTOCOL_MINOR_QUEUE, SPEECH_FAMILY,
};

use crate::runtime::EngineRuntime;
use conn::Ctx;
use files::{pid_alive, RecordGuard};

/// Headless host exits after this long with no clients and no file work.
pub const HEADLESS_IDLE_EXIT: Duration = Duration::from_secs(10 * 60);
const TAKEOVER_WAIT: Duration = Duration::from_secs(30);
const TAKEOVER_POLL: Duration = Duration::from_millis(250);
const YIELD_POLL: Duration = Duration::from_secs(1);

/// Model identity belongs to speech responses, not the model-neutral IPC build.
pub(super) fn engine_version() -> String {
    let choice = rightkit_dictation_inference::engine_paths::launch_model_choice();
    // The engine serves the backend it was launched with; the persisted setting only decides when
    // the launch named none.
    let backend = choice
        .backend
        .clone()
        .unwrap_or_else(crate::settings::asr_backend);
    format!(
        "{}/{}",
        crate::ipc::ENGINE_VERSION,
        model_label(&backend, &choice)
    )
}

/// The model a speech response names: `whisper`, or the installed Parakeet directory's own name
/// (the Ultra directory is named after its id), falling back to the Ultra id when the launch
/// named no directory. This label is informational; the protocol family id peers lease against
/// (`SPEECH_FAMILY`) is a separate key and does not change with it.
fn model_label(
    backend: &str,
    choice: &rightkit_dictation_inference::engine_paths::LaunchModelChoice,
) -> String {
    if matches!(backend, "whisper" | "whisper-cli") {
        return "whisper".to_string();
    }
    let dir = choice.coreml_model_dir.clone().or_else(|| {
        choice
            .asr_model_subdir
            .as_ref()
            .map(std::path::PathBuf::from)
    });
    dir.as_deref()
        .and_then(std::path::Path::file_name)
        .and_then(std::ffi::OsStr::to_str)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| rightkit_dictation_models::ultra_model::ULTRA_ID.to_string())
}

pub const BUSY_MESSAGE: &str = "Speech model is currently in use by another Orthic Labs app.";

/// Role for this process: `RIGHTKIT_SPEECH_SERVICE_ROLE` (scraperight|cutright|
/// heardright_shell|heardright_headless) wins; otherwise headless mode is
/// `HeardRightHeadless` and the app-spawned engine is `HeardRightShell`.
pub fn role_for(headless: bool) -> HostRole {
    let overridden = rightkit_dictation_core::dev_var!("RIGHTKIT_SPEECH_SERVICE_ROLE")
        .ok()
        .and_then(|v| parse_role(&v));
    overridden.unwrap_or(if headless {
        HostRole::HeardRightHeadless
    } else {
        HostRole::HeardRightShell
    })
}

pub fn parse_role(value: &str) -> Option<HostRole> {
    HostRole::parse_env(value)
}

/// Acquire the model lease. When another host holds it and we outrank that
/// host, request a takeover and retry for up to 30 s; otherwise fail with the
/// historical busy message.
pub fn acquire_lease_with_takeover(role: HostRole) -> anyhow::Result<ModelLease> {
    let busy = || anyhow::anyhow!(BUSY_MESSAGE);
    if let Some(lease) =
        try_acquire_compat(SPEECH_FAMILY, "heardright").map_err(anyhow::Error::msg)?
    {
        return Ok(lease);
    }
    let root = files::suite_root();
    let record = files::read_live_record(&root, &pid_alive);
    if !files::should_request_takeover(role, record.as_ref()) {
        return Err(busy());
    }
    if let Err(error) = files::write_takeover(&root, role) {
        tracing::warn!(event = "takeover_request_failed", %error);
        return Err(busy());
    }
    tracing::info!(
        event = "takeover_requested",
        our_role = ?role,
        holder_role = ?record.as_ref().map(|r| r.role),
        holder_pid = record.as_ref().map(|r| r.pid),
        "requested speech service takeover"
    );
    let deadline = Instant::now() + TAKEOVER_WAIT;
    while Instant::now() < deadline {
        std::thread::sleep(TAKEOVER_POLL);
        if let Some(lease) =
            try_acquire_compat(SPEECH_FAMILY, "heardright").map_err(anyhow::Error::msg)?
        {
            files::clear_takeover_if_ours(&root);
            return Ok(lease);
        }
    }
    files::clear_takeover_if_ours(&root);
    tracing::warn!(
        event = "takeover_timed_out",
        "previous speech host did not yield"
    );
    Err(busy())
}

/// Running service: listener threads, discovery record, yield watcher.
pub struct Service {
    ctx: Arc<Ctx>,
    stop: Arc<AtomicBool>,
    listener: Arc<rightkit_daemon::Listener>,
    _record: RecordGuard,
}

impl Drop for Service {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.ctx.accepting.store(false, Ordering::SeqCst);
        // Unblock the accept loop so it observes `stop` and exits.
        self.listener.wake();
    }
}

/// Start the endpoint (model must already be warm), publish discovery, and,
/// for roles that can be outranked, start the yield watcher.
pub fn start(runtime: Arc<Mutex<EngineRuntime>>, role: HostRole) -> anyhow::Result<Service> {
    let root = files::suite_root();
    std::fs::create_dir_all(service_dir(&root))?;
    if role == HostRole::HeardRightShell {
        write_installed_engine(&root);
    }
    let endpoint = endpoint_for(&root, &transport::user_tag()?);
    let ctx = Arc::new(Ctx::new(runtime, role));
    let stop = Arc::new(AtomicBool::new(false));
    let listener = transport::listen(ctx.clone(), &endpoint, stop.clone())
        .map_err(|e| anyhow::anyhow!("speech service endpoint {endpoint}: {e}"))?;
    let pid = std::process::id();
    files::write_record(
        &root,
        &ServiceRecord {
            protocol_major: SERVICE_PROTOCOL_MAJOR,
            protocol_minor: SERVICE_PROTOCOL_MINOR_QUEUE,
            role,
            pid,
            endpoint: endpoint.clone(),
            engine_version: engine_version(),
        },
    )?;
    let record = RecordGuard::new(root.clone(), pid);
    tracing::info!(event = "service_listening", ?role, pid, endpoint = %endpoint, "speech service listening");
    if role != HostRole::HeardRightShell {
        spawn_yield_watcher(ctx.clone(), root);
    }
    Ok(Service {
        ctx,
        stop,
        listener,
        _record: record,
    })
}

fn write_installed_engine(root: &std::path::Path) {
    let result = std::env::current_exe()
        .map_err(anyhow::Error::from)
        .and_then(|engine_path| {
            files::write_json_atomic(
                &installed_heardright_path(root),
                &InstalledEngine {
                    engine_path,
                    engine_version: engine_version(),
                },
            )
            .map_err(anyhow::Error::from)
        });
    if let Err(error) = result {
        tracing::warn!(event = "installed_engine_record_failed", %error);
    }
}

fn has_file_work(runtime: &Arc<Mutex<EngineRuntime>>) -> bool {
    runtime
        .lock()
        .worker_handle()
        .map(|h| h.has_file_work())
        .unwrap_or(false)
}

fn spawn_yield_watcher(ctx: Arc<Ctx>, root: std::path::PathBuf) {
    let spawned = std::thread::Builder::new()
        .name("hr-speech-yield".into())
        .spawn(move || {
            let own_pid = std::process::id();
            loop {
                std::thread::sleep(YIELD_POLL);
                let request: Option<TakeoverRequest> =
                    files::read_json(&takeover_request_path(&root));
                if !files::takeover_obliges_yield(ctx.role, request.as_ref(), own_pid, &pid_alive) {
                    continue;
                }
                let requester = request.expect("checked above");
                tracing::info!(
                    event = "takeover_yielded",
                    our_role = ?ctx.role,
                    requester_role = ?requester.role,
                    requester_pid = requester.pid,
                    "yielding speech service"
                );
                ctx.accepting.store(false, Ordering::SeqCst);
                let handle = ctx.runtime.lock().worker_handle();
                if let Ok(handle) = handle {
                    handle.fail_all_file_jobs(ERROR_HOST_HANDOFF);
                }
                RecordGuard::new(root.clone(), own_pid).remove_now();
                // Let in-flight handoff errors reach their clients.
                std::thread::sleep(Duration::from_millis(500));
                // Process exit releases the OS-bound model lease.
                std::process::exit(0);
            }
        });
    if let Err(error) = spawned {
        tracing::error!(event = "yield_watcher_failed", %error);
    }
}

/// Headless host loop (`--speech-service`): warm the model, serve the
/// endpoint, exit after [`HEADLESS_IDLE_EXIT`] with no clients or file work.
pub fn run_headless(runtime: Arc<Mutex<EngineRuntime>>) -> anyhow::Result<()> {
    runtime
        .lock()
        .ensure_worker()
        .map_err(|e| anyhow::anyhow!("worker warmup failed before ready: {e}"))?;
    let service = start(runtime.clone(), role_for(true))?;
    loop {
        std::thread::sleep(Duration::from_secs(1));
        let ctx = &service.ctx;
        if ctx.connections.load(Ordering::SeqCst) > 0 || has_file_work(&runtime) {
            ctx.touch();
            continue;
        }
        if ctx.last_activity.lock().elapsed() >= HEADLESS_IDLE_EXIT {
            tracing::info!(event = "service_idle_exit", "speech service idle; exiting");
            return Ok(());
        }
    }
}

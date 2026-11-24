// `EngineSupervisor` — the the app face of RightKit's engine supervisor.
//
// Process ownership, hello/contract check, restart policy, bounded reaping, stderr drain and
// the stdio framing are `rightkit_job_queue::EngineSupervisor` (feature `supervisor`); this file
// keeps only the app policy: the sidecar environment, binary and ffmpeg paths, the method
// allowlist, the BYOK key handshake, the shell-event bridge and the per-session final-reply wait.
// There is no second supervisor here: no child handle, no reader/writer threads, no generation
// bookkeeping.

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

use rightkit_dictation_core::engine::launch::{EngineLaunch, LAUNCH_SCHEMA, LAUNCH_VERSION};
use rightkit_dictation_core::engine::provider_handoff::{check_ack, encode_slots_frame, KeySlot};
use rightkit_dictation_core::engine::{EngineFrame, EnginePayload, EngineSchemaName, SecretString};
use rightkit_job_queue::{
    CallOptions, EngineSupervisor as RkSupervisor, ExitObserver, InitialFailure, JobError,
    LaunchFrame, Lifecycle, OwnedCommand, RawParams, RestartMode, RestartPolicy, ShutdownReport,
    SupervisorConfig as RkConfig, Terminal, MAX_FRAME_LIMIT,
};

use super::env::strip_l3_env;
use super::paths::{binary_identity_suffix, engine_cache_telemetry, resolve_engine_bin};
use super::protocol::build_request;
use super::routing::{classify_exit, Claim, EventRouter, ExitKind, KeyGate, Routed};
use super::wire::{
    error_frame_of, frame_from_value, hello_instance, hello_version, method_for, parse_accepted,
    with_spawn_hint, ACCEPTED_METHODS,
};
use crate::engine_process::EngineEvent;
use crate::sidecar_log::redact_sidecar_stderr_line;
use crate::telemetry;

/// The engine says hello only after the resident worker has loaded its model, so this is the
/// cold-load budget (`hello_timeout`), not just a process-start budget.
#[cfg(target_os = "macos")]
const SIDECAR_READY_TIMEOUT: Duration = Duration::from_secs(360);
#[cfg(not(target_os = "macos"))]
const SIDECAR_READY_TIMEOUT: Duration = Duration::from_secs(90);

/// Grace for the engine's own `shutdown` request and for each kill/reap step. A sidecar wedged
/// releasing a GPU model can never hold tray-quit or window-close longer than a few of these.
const SIDECAR_SHUTDOWN_GRACE: Duration = Duration::from_secs(3);

/// A `send` (a request whose answer is a shell event) waits this long for the engine's quick
/// reply. A write that stalls inside it is a wedged pipe: RightKit kills and reaps the engine.
const SIDECAR_SEND_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a live capability probe / key replacement waits for the engine.
const PROVIDER_KEYS_ACK_TIMEOUT: Duration = Duration::from_secs(3);

/// Bound on the handshake call that delivers the BYOK keys before `Ready`.
const SIDECAR_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Restart budget: unexpected exits allowed per rolling window.
const SIDECAR_RESTARTS_PER_WINDOW: u32 = 3;
const SIDECAR_RESTART_WINDOW: Duration = Duration::from_secs(60);
const SIDECAR_RESTART_BACKOFF_BASE: Duration = Duration::from_millis(500);
const SIDECAR_RESTART_BACKOFF_MAX: Duration = Duration::from_secs(30);
/// Replacements that never reach `Ready` in a row before the cooldown applies.
const SIDECAR_ABANDON_RETRY_CAP: u32 = 3;
const SIDECAR_ABANDON_COOLDOWN: Duration = Duration::from_secs(30);

/// Keys the sidecar legitimately needs from the parent environment (`env_allowlist`). Everything
/// else is dropped and set explicitly in `build_sidecar_command`.
///
/// - PATH / PATHEXT (Windows): the sidecar binary and bundled ffmpeg/ort DLLs must be found.
/// - HOME (Unix) / USERPROFILE (Windows): per-user config + caches.
/// - TMPDIR (Unix) / TEMP / TMP (Windows): tempfile creation.
/// - LANG / LC_ALL: locale for ASR/ITN text normalization.
/// - SYSTEMROOT (Windows): required by some native DLLs.
/// - __CFBundleIdentifier (macOS): selects the `~/Library/Caches/<bundle-id>` that owns Apple's
///   ANE compile cache (com.apple.e5rt.e5bundlecache); without it the 1.5 GB Parakeet bundle is
///   recompiled on every cold launch (~45-75s).
const SIDECAR_ENV_PASSTHROUGH: [&str; 11] = [
    "PATH",
    "PATHEXT",
    "HOME",
    "USERPROFILE",
    "TMPDIR",
    "TEMP",
    "TMP",
    "LANG",
    "LC_ALL",
    "SYSTEMROOT",
    "__CFBundleIdentifier",
];

#[derive(Debug, Clone)]
// An engine event is built once and moved to one consumer; boxing it would only add an allocation
// per event.
#[allow(clippy::large_enum_variant)]
pub enum SupervisorEvent {
    /// The sidecar reported it is alive and on the expected protocol
    /// version. Emitted on every successful start.
    Ready { engine_version: String },
    /// The sidecar process exited unexpectedly. The supervisor restarts it on the next
    /// `start_dictation` (`RestartMode::OnDemand`).
    Crashed { code: Option<i32> },
    /// The supervisor tried to spawn the sidecar and failed (binary not
    /// found, spawn IO error, no hello, failed key handshake).
    SpawnFailed { message: String },
    /// The sidecar emitted an `EngineEvent` for the shell to consume. This
    /// is the primary bridge path; the existing Tauri event-consumer thread
    /// still drives the UI from `EngineEvent` values forwarded here.
    EngineEvent(EngineEvent),
}

/// True when a sidecar capability list opts into live L3 reconfigure.
pub fn advertises_live_l3(capabilities: &[String]) -> bool {
    capabilities.iter().any(|capability| {
        capability == rightkit_dictation_core::engine::CAPABILITY_LIVE_L3_RECONFIGURE
    })
}

/// BYOK keys moved out of `SupervisorConfig` for hand-off to the engine: one owner-snapshot
/// entry per provider slot.
struct ProviderKeys {
    groq: KeySlot,
    nvidia: KeySlot,
    openrouter: KeySlot,
}

impl ProviderKeys {
    /// The empty set: every slot Absent, so a full replacement wipes the engine's copy.
    fn none() -> Self {
        Self {
            groq: KeySlot::Absent,
            nvidia: KeySlot::Absent,
            openrouter: KeySlot::Absent,
        }
    }
}

/// A slot from what the app read: a key is Present, an unreadable store is ReadFailed (the
/// engine fails closed), anything else is Absent.
fn key_slot(key: Option<SecretString>, state: &str) -> KeySlot {
    match key {
        Some(key) => KeySlot::Present(key),
        None if state == "read_error" => KeySlot::ReadFailed,
        None => KeySlot::Absent,
    }
}

/// Take the keys out of the config when cloud polish is eligible to run.
/// `None` means there is nothing to deliver (polish stays local, as today
/// when no key is configured).
fn take_provider_keys(config: &mut crate::SupervisorConfig) -> Option<ProviderKeys> {
    let keys = ProviderKeys {
        groq: key_slot(config.groq_key.take(), &config.groq_state),
        nvidia: key_slot(config.nvidia_key.take(), &config.nvidia_state),
        openrouter: key_slot(config.openrouter_key.take(), &config.openrouter_state),
    };
    (config.ai_polish_enabled && config.product_provider_ready).then_some(keys)
}

/// The polish policy for the current generation, pushed to the engine in its config.
static L3_POLICY: Mutex<Option<serde_json::Value>> = Mutex::new(None);

/// Same semantics the spawn environment carried: on macOS cleanup runs whenever polish is on
/// (local cleanup needs no provider); elsewhere only with a ready provider. Cloud consent is
/// granted only when a product provider is ready.
fn l3_policy_for(config: &crate::SupervisorConfig) -> serde_json::Value {
    let enabled = config.ai_polish_enabled;
    let ready = enabled && config.product_provider_ready;
    serde_json::json!({
        "cleanup_enabled": enabled && (cfg!(target_os = "macos") || ready),
        "cloud_consent": ready,
        "provider_state": if enabled { config.product_provider_state.clone() } else { String::new() },
    })
}

fn record_l3_policy(config: &crate::SupervisorConfig) {
    if let Ok(mut slot) = L3_POLICY.lock() {
        *slot = Some(l3_policy_for(config));
    }
}

/// The policy to push in `extras["l3_policy"]`. Before the first spawn it is the disabled policy,
/// so a stale value in settings.json can never stand in for it.
pub fn l3_policy_value() -> serde_json::Value {
    L3_POLICY
        .lock()
        .ok()
        .and_then(|slot| slot.clone())
        .unwrap_or_else(|| serde_json::json!({"cleanup_enabled": false, "cloud_consent": false, "provider_state": ""}))
}

/// Non-secret polish readiness telemetry (states and labels only).
fn report_l3_readiness(config: &crate::SupervisorConfig) {
    if !config.ai_polish_enabled {
        return;
    }
    telemetry::log_message(
        "l3_provider_readiness",
        &format!("state={}", config.product_provider_state),
    );
}

/// Build the sidecar command (environment only; stdio, containment, hidden window and spawn are
/// RightKit's). Carries NO BYOK key variable.
/// The model choice for one generation, sent as the engine's launch frame (first stdin line,
/// before hello). Only what the engine cannot derive: the backend, the Parakeet variant the
/// shell resolved (shared or legacy; a load check uses its candidate), and the load-check flag.
/// Language and the polish policy reach the engine in the pushed config instead.
pub(super) fn engine_launch_for(config: &crate::SupervisorConfig) -> EngineLaunch {
    let load_check = rightkit_dictation_models::shared::is_load_check();
    let backend = if load_check {
        "parakeet-tdt".to_string()
    } else {
        config.asr_backend.clone()
    };
    let model_subdir = crate::model_paths::subdir_for_backend(&backend);
    // Only Parakeet moves. Whisper & other app-private models retain their existing base; send
    // Parakeet's resolved directory separately.
    let models_base = crate::model_paths::install_base();
    let model_dir = crate::model_paths::resolve_model_dir(model_subdir)
        .unwrap_or_else(|| models_base.join(model_subdir));
    EngineLaunch {
        backend,
        coreml_model_dir: cfg!(target_os = "macos").then(|| model_dir.clone()),
        asr_model_subdir: (!cfg!(target_os = "macos"))
            .then(|| model_dir.to_string_lossy().into_owned()),
        load_check,
    }
}

// The provider signature is fixed by rightkit-job-queue `with_launch_frame`, whose JobError is large by design (that crate allows the lint).
#[allow(clippy::result_large_err)]
fn launch_frame_for_generation(_generation: u64) -> Result<LaunchFrame, JobError> {
    LaunchFrame::new(
        LAUNCH_SCHEMA,
        LAUNCH_VERSION,
        &engine_launch_for(&crate::supervisor_config()),
    )
}

fn build_sidecar_command(bin: &std::path::Path, _config: &crate::SupervisorConfig) -> OwnedCommand {
    let mut owned = OwnedCommand::new(bin);
    // Parent death ends the sidecar: the owner ties it (Linux PDEATHSIG, a kqueue watchdog on macOS;
    // Windows already has the kill-on-close job), so the engine needs no parent-liveness poll.
    owned.kill_on_parent_exit(true);
    // Clear the parent shell's env before populating the sidecar's env so BYOK keys,
    // debug vars, or anything the Tauri host inherited from the launcher's environment can't
    // accidentally leak across the IPC boundary. `env_allowlist` re-passes only the small set of
    // keys the sidecar needs; everything else (HR_*, HEARDRIGHT_*) is set explicitly below.
    // `require_scrubbed_env` makes RightKit refuse to boot a command that skipped this.
    owned.env_allowlist(SIDECAR_ENV_PASSTHROUGH);
    {
        let command = owned.command_mut();
        let app_data_root = crate::app_paths::app_data_root();
        // A Finder/Dock-launched macOS app runs with cwd="/" (read-only), and the
        // sidecar inherits it. Apple's ANE compiler service writes its compiled-
        // model cache artifacts relative to the process working directory, so with
        // cwd="/" the CoreML compile succeeds in memory but never persists — every
        // cold launch then recompiles the Parakeet stages onto the ANE (~45-75s,
        // field reports 2026-07). Warm `aned` masked this until an eviction exposed
        // it. Anchor the sidecar to the writable app-data dir so the compile cache
        // persists and cold launches reuse it. (Windows/Linux inherit a normal cwd
        // already; setting it here is harmless there.)
        command.current_dir(&app_data_root);
        // The engine derives its own app-data root, models base and bundled resources
        // (rightkit-dictation-runtime `engine_paths`). The model choice travels in the launch frame
        // (`engine_launch_for`), never in the environment (release switch rule).
        // No suite-root or app-data-root variable is forwarded: RightKit's `suite_root()` reads no
        // override variable and resolves the per-user default from the account database in both
        // processes, so the engine lands on the same root with nothing set (rightkit-daemon
        // `Suite::resolve`).
        strip_l3_env(command);
        // The polish policy (cleanup on, cloud consent, provider state) is no longer an
        // environment switch: `prepare_spawn` records it per generation and the shell pushes it in
        // the engine config (`l3_policy_value`, extras["l3_policy"]), which the engine reads before
        // any polish call. BYOK provider keys never enter the sidecar environment.
        // They go over the private stdin IPC channel with `call_raw_with` once per
        // `Ready` generation (`ensure_ready`), and dependent work is gated until that settles.
    }
    owned
}

/// What the last spawn resolved, for the `sidecar_started` telemetry line.
pub(crate) struct SpawnNote {
    bin: PathBuf,
    app_data_root: PathBuf,
    models_base: PathBuf,
}

pub(crate) type SharedSpawnNote = Arc<Mutex<Option<SpawnNote>>>;

/// Builds the command for one generation. RightKit calls it before every boot (first start,
/// automatic and on-demand restarts), so the binary, config and models are re-resolved each time.
fn prepare_spawn(note: &SharedSpawnNote) -> io::Result<OwnedCommand> {
    let Some(bin) = resolve_engine_bin() else {
        let message = format!(
            "{} binary not found",
            crate::host::engine_host().binary_stem
        );
        telemetry::log_message("sidecar_spawn_failed", &message);
        return Err(io::Error::new(io::ErrorKind::NotFound, message));
    };
    // Single model-location authority: verified shared variant, then legacy. The config's BYOK
    // keys are not used here: they go over IPC only, never through the environment.
    let config = crate::supervisor_config();
    report_l3_readiness(&config);
    record_l3_policy(&config);
    let command = build_sidecar_command(&bin, &config);
    if let Ok(mut slot) = note.lock() {
        *slot = Some(SpawnNote {
            bin,
            app_data_root: crate::app_paths::app_data_root(),
            models_base: crate::model_paths::install_base(),
        });
    }
    Ok(command)
}

/// Writes the crash dump when a generation that ran dies unexpectedly (RightKit calls this on the
/// restart/reader thread after the lifecycle transition, with no supervisor lock held).
struct CrashDumpObserver;

impl ExitObserver for CrashDumpObserver {
    fn engine_exited(&self, _helper: &str, exit: &str, _restart_count: u32) {
        // A boot that never produced a hello has nothing to dump.
        if ["spawn_failed", "hello_failed", "handshake_failed"]
            .iter()
            .any(|boot_failure| exit.contains(boot_failure))
        {
            return;
        }
        let _ = crate::crash_dump::write_dump("sidecar_crash");
    }
}

/// The app's restart policy: crashes restart on the next `start()` (`OnDemand`: a
/// long dictation on a wedged sidecar is a UI freeze). `Automatic` hot restart after exponential
/// backoff is available to a caller that passes `autorestart`. Replacements that never reach
/// `Ready` stop being retried at backoff speed after `SIDECAR_ABANDON_RETRY_CAP`. A spent budget
/// is not permanent: `EngineSupervisor::ensure_running` starts a fresh run after
/// `SIDECAR_ABANDON_COOLDOWN`.
fn restart_policy(autorestart: bool) -> RestartPolicy {
    let mode = if autorestart {
        RestartMode::Automatic
    } else {
        RestartMode::OnDemand
    };
    RestartPolicy::new(
        mode,
        SIDECAR_RESTARTS_PER_WINDOW,
        SIDECAR_RESTART_WINDOW,
        SIDECAR_RESTART_BACKOFF_BASE,
        SIDECAR_RESTART_BACKOFF_MAX,
    )
    .and_then(|policy| {
        policy.with_consecutive_failures(SIDECAR_ABANDON_RETRY_CAP, SIDECAR_ABANDON_COOLDOWN)
    })
    .unwrap_or_default()
}

/// Error for a failed or uncertain key delivery; the typed code only, never key material.
fn delivery_error(code: &str) -> String {
    let code = match code {
        "timeout" | "delivery_unknown" | "engine_exited" | "engine_write" | "encode_failed"
        | "ack_invalid" => code,
        _ => "engine_rejected",
    };
    format!("provider key delivery failed: {code}")
}

/// Why an exchange with the engine failed.
enum ExchangeError {
    /// The request was written but no reply arrived in time.
    NoReply(String),
    Failed(String),
}

impl ExchangeError {
    fn into_message(self) -> String {
        match self {
            Self::NoReply(message) | Self::Failed(message) => message,
        }
    }
}

/// The supervisor owns the sidecar subprocess. One instance per shell.
pub struct EngineSupervisor {
    rk: Arc<RkSupervisor>,
    next_request_id: AtomicU64,
    /// `(generation, advertises live L3 reconfigure)`; capabilities are asked
    /// once per sidecar generation.
    live_l3_support: Mutex<Option<(u64, bool)>>,
    /// Bounded final-transcript wait (#159); see `final_wait`.
    final_waits: Mutex<super::final_wait::FinalWaitTracker>,
    /// BYOK delivery per `Ready` generation; key-dependent work waits on it.
    gate: KeyGate,
    /// Generation-fenced, exactly-once routing of id-less engine events.
    router: Arc<EventRouter>,
    /// True once a run was started with `rk.start()`. Recovery after a crash never goes back
    /// through `start()` (it would reset the restart budget): it demands the on-demand restart
    /// through a call, which RightKit grants or refuses against the budget. Cleared by
    /// `shutdown()`, which is what makes the next `start()` an explicit new run.
    ran: AtomicBool,
    /// Highest generation an intentional shutdown stopped; its exits are not crashes.
    shutdown_generation: Arc<AtomicU64>,
    /// When `Terminal::Failed` (a spent restart budget) was first seen. RightKit holds the failure
    /// until `start()` runs again, and only `shutdown()` allowed that, so a spent budget used to
    /// disable dictation until the user changed a setting or relaunched. After
    /// `abandon_cooldown` the next demand starts a fresh run with a fresh budget. Cleared once the
    /// engine is `Ready` again.
    terminal_since: Mutex<Option<Instant>>,
    abandon_cooldown: Duration,
    /// The generation whose background key delivery failed or was uncertain while it was
    /// recording. The engine of that generation keeps running (the recording is never touched);
    /// the next recording start, which runs with no session in flight, retires it so a fresh
    /// generation gets its own delivery. See [`EngineSupervisor::deliver_claimed_provider_keys`].
    keys_retry_generation: Mutex<Option<u64>>,
    /// Captures that hold this engine: one per reserved start (`hold_capture`). A generation is
    /// never retired while this is non-zero or while an active session exists (`capture_busy`).
    capture_holds: AtomicUsize,
    /// Serializes capture admission (`hold_capture`) against a retirement claim
    /// (`retire_generation`). `true` while a claimed retirement is shutting the engine down; a
    /// start waits on `retirement_done` until it ends, so it can never attach to the dying
    /// generation. The flag is set and cleared under the lock; the shutdown itself runs unlocked.
    retiring: Mutex<bool>,
    retirement_done: Condvar,
    /// The shell's mirror of its active session (`register_active_session_probe`): `Some` from
    /// the moment a start allocates its session until the session reaches a terminal state.
    session_probe: Mutex<Option<Arc<Mutex<Option<String>>>>>,
}

/// A held capture: while it lives the engine generation is not retired by any key-delivery path.
/// Released on drop.
pub struct CaptureHold {
    supervisor: Arc<EngineSupervisor>,
}

impl Drop for CaptureHold {
    fn drop(&mut self) {
        self.supervisor.capture_holds.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The provider-key delivery claim a recording start takes synchronously
/// ([`EngineSupervisor::claim_provider_keys_for_start`]); the delivery then runs on any thread
/// ([`EngineSupervisor::deliver_claimed_provider_keys`]).
pub struct ProviderKeyClaim {
    generation: u64,
    deliver: bool,
}

impl EngineSupervisor {
    /// Build a new supervisor. The event channel feeds the existing Tauri
    /// event-consumer thread; the supervisor never owns UI state itself.
    /// `autorestart` selects the restart mode (see `restart_policy`).
    pub fn new(autorestart: bool) -> (Self, Receiver<SupervisorEvent>) {
        let note: SharedSpawnNote = Arc::new(Mutex::new(None));
        let spawn_note = Arc::clone(&note);
        Self::with_parts(restart_policy(autorestart), note, true, move || {
            prepare_spawn(&spawn_note)
        })
    }

    pub(crate) fn with_parts(
        policy: RestartPolicy,
        note: SharedSpawnNote,
        launch_frame: bool,
        spawn: impl Fn() -> io::Result<OwnedCommand> + Send + Sync + 'static,
    ) -> (Self, Receiver<SupervisorEvent>) {
        let (event_tx, event_rx) = channel();
        let config = RkConfig::new(crate::host::engine_host().binary_stem, spawn);
        // The real engine reads its launch frame first; test fakes built with `with_parts` do not.
        let config = if launch_frame {
            config.with_launch_frame(launch_frame_for_generation)
        } else {
            config
        };
        let config = config
            .with_hello_timeout(SIDECAR_READY_TIMEOUT)
            // A caller that finds the engine booting waits out the same cold load the booter does.
            .with_ready_wait(SIDECAR_READY_TIMEOUT + Duration::from_secs(5))
            .with_rpc_timeout(SIDECAR_HANDSHAKE_TIMEOUT)
            // File transcripts (SRT/VTT/words) are large and the engine is a trusted local child.
            .with_frame_limit(MAX_FRAME_LIMIT)
            .with_shutdown_grace(SIDECAR_SHUTDOWN_GRACE)
            .with_restart(policy)
            // A binary that cannot even say hello is a configuration problem: report it to the
            // caller of `start()` instead of retrying it behind their back.
            .with_initial_failure(InitialFailure::Terminal)
            .require_current_contract_version()
            .with_mandatory_contract_check()
            .require_scrubbed_env()
            .with_stderr_redaction(|line| Some(redact_sidecar_stderr_line(line)))
            .with_stderr_sink(|line| telemetry::log_message("sidecar_stderr", line))
            // The message only: the offending line may carry transcript text.
            .with_protocol_error_sink(|error| {
                telemetry::log_message("sidecar_frame_parse", &error.message)
            })
            .with_exit_observer(Arc::new(CrashDumpObserver));
        let rk = Arc::new(RkSupervisor::new(config));
        let router = Arc::new(EventRouter::default());
        let shutdown_generation = Arc::new(AtomicU64::new(0));
        spawn_bridges(&rk, &event_tx, &note, &router, &shutdown_generation);
        (
            Self {
                rk,
                next_request_id: AtomicU64::new(1),
                live_l3_support: Mutex::new(None),
                final_waits: Mutex::new(Default::default()),
                gate: KeyGate::default(),
                router,
                ran: AtomicBool::new(false),
                shutdown_generation,
                terminal_since: Mutex::new(None),
                abandon_cooldown: SIDECAR_ABANDON_COOLDOWN,
                keys_retry_generation: Mutex::new(None),
                capture_holds: AtomicUsize::new(0),
                retiring: Mutex::new(false),
                retirement_done: Condvar::new(),
                session_probe: Mutex::new(None),
            },
            event_rx,
        )
    }

    /// Make the engine ready for work and return its version (`already-running` when it was
    /// already `Ready`). The first call after construction or `shutdown()` is the one explicit
    /// `rk.start()`; every later call only demands recovery through RightKit's restart policy.
    /// Either way the BYOK keys for the current generation are delivered before this returns.
    pub fn start(&self) -> Result<String, String> {
        self.ensure_ready()
    }

    fn ensure_ready(&self) -> Result<String, String> {
        let version = self.ensure_running()?;
        self.ensure_provider_keys()?;
        Ok(version)
    }

    /// The BYOK key-delivery gate of the current generation, on its own: claim it and, when this
    /// caller is the one that delivers, deliver (up to `PROVIDER_KEYS_ACK_TIMEOUT`); a caller that
    /// finds a delivery in flight waits for it. `ensure_ready` is `ensure_running` plus this.
    fn ensure_provider_keys(&self) -> Result<(), String> {
        let generation = self.rk.status().generation;
        match self.gate.claim(generation) {
            Claim::Done => {}
            // The generation's delivery failed or is uncertain: never resend to it. It is retired
            // (once) so the next start spawns a fresh generation that gets its own delivery; this
            // call alone reports the error.
            Claim::Failed(reason) => {
                self.retire_generation(generation);
                return Err(reason);
            }
            Claim::Deliver => {
                let outcome = self.deliver_provider_keys();
                self.gate.settle(generation, outcome.clone());
                if let Err(reason) = outcome {
                    telemetry::log_message(
                        "provider_keys_delivery_failed",
                        &format!("generation={generation}; reason={reason}"),
                    );
                    self.retire_generation(generation);
                    return Err(reason);
                }
            }
        }
        Ok(())
    }

    /// End `generation` after its key delivery failed or is uncertain. The gate keeps that
    /// generation's failure for good (it never resends), so leaving the engine up would fail every
    /// later `start()` until a settings change or relaunch. `shutdown()` ends the run: the next
    /// `start()` is a new run and a new generation, which the gate claims afresh. Only acts while
    /// that generation is still the live one, so a caller that lost a race never stops the
    /// replacement.
    fn retire_generation(&self, generation: u64) {
        // No key-delivery path (prewarm, an ordinary call, the background delivery) may end an
        // engine that is capturing or about to: the generation stays up with its keys marked
        // unavailable (the gate keeps the failure; provider processing falls back to local) and
        // the retirement is deferred to the next recording start, which runs with no session in
        // flight on the engine (`start_without_keys`).
        //
        // The busy check and the retirement claim happen under `retiring`, the same lock
        // `hold_capture` admits under, so a start cannot slip in between the check and the
        // shutdown: either the start holds first (retirement deferred) or the claim is first and
        // the start waits for the shutdown to finish, then starts a fresh generation.
        {
            let mut retiring = self
                .retiring
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if *retiring {
                return;
            }
            if self.capture_busy() {
                telemetry::log_message(
                    "provider_keys_generation_retire_deferred",
                    &format!("generation={generation}"),
                );
                *self
                    .keys_retry_generation
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(generation);
                return;
            }
            *retiring = true;
        }
        // App I/O (the engine shutdown) runs with no lock held.
        self.retire_generation_now(generation);
        let mut retiring = self
            .retiring
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *retiring = false;
        self.retirement_done.notify_all();
    }

    fn retire_generation_now(&self, generation: u64) {
        if self.rk.lifecycle() != Lifecycle::Ready || self.rk.status().generation != generation {
            return;
        }
        telemetry::log_message(
            "provider_keys_generation_retired",
            &format!("generation={generation}"),
        );
        self.shutdown();
    }

    /// True while a capture holds this engine: a reserved start (`hold_capture`) or an active
    /// session in the shell's mirror. An unreadable mirror counts as busy.
    fn capture_busy(&self) -> bool {
        if self.capture_holds.load(Ordering::SeqCst) > 0 {
            return true;
        }
        let probe = self
            .session_probe
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        probe.is_some_and(|probe| {
            probe
                .lock()
                .map(|session| session.is_some())
                .unwrap_or(true)
        })
    }

    /// Hold this engine for one reserved start until the returned guard drops.
    /// Non-waiting admission under `retiring`, for callers that must return at once (a start
    /// reservation claimed on a hotkey dispatch thread while it holds the pending-start slot):
    /// `None` while a claimed retirement is shutting the engine down; the caller then takes the
    /// hold later with [`EngineSupervisor::hold_capture`] off that thread, holding no slot.
    pub fn try_hold_capture(self: &Arc<Self>) -> Option<CaptureHold> {
        let retiring = self
            .retiring
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *retiring {
            return None;
        }
        self.capture_holds.fetch_add(1, Ordering::SeqCst);
        drop(retiring);
        Some(CaptureHold {
            supervisor: Arc::clone(self),
        })
    }

    /// Admitted under `retiring`: a claimed retirement finishes first (the wait releases the
    /// lock), so the start that follows runs on a fresh generation, never the one shutting down.
    /// Blocks for at most the retirement's shutdown: call it only from a thread that holds no
    /// pending-start slot or dispatch thread (see `try_hold_capture`).
    pub fn hold_capture(self: &Arc<Self>) -> CaptureHold {
        let mut retiring = self
            .retiring
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while *retiring {
            retiring = self
                .retirement_done
                .wait(retiring)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        self.capture_holds.fetch_add(1, Ordering::SeqCst);
        drop(retiring);
        CaptureHold {
            supervisor: Arc::clone(self),
        }
    }

    /// Give the supervisor the shell's active-session mirror so it can tell a recording engine
    /// from an idle one (see [`EngineSupervisor::hold_capture`]). Called once at runtime open.
    pub fn register_active_session_probe(&self, probe: Arc<Mutex<Option<String>>>) {
        *self
            .session_probe
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(probe);
    }

    fn terminal_since_lock(&self) -> std::sync::MutexGuard<'_, Option<Instant>> {
        self.terminal_since
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Record that the spent budget was seen now, unless it was already seen earlier.
    fn note_terminal_failure(&self) -> Instant {
        let mut since = self.terminal_since_lock();
        *since.get_or_insert_with(Instant::now)
    }

    fn ensure_running(&self) -> Result<String, String> {
        if self.rk.lifecycle() == Lifecycle::Ready {
            *self.terminal_since_lock() = None;
            return Ok("already-running".to_string());
        }
        // A spent restart budget is sticky in RightKit: only a new `start()` clears it. Within the
        // cooldown the failure is reported as is; after it, this demand is the explicit new run
        // (a fresh budget), so a persistently broken engine costs one boot per cooldown, and only
        // when the user asks for dictation.
        if self.ran.load(Ordering::Acquire) {
            if let Some(Terminal::Failed { reason }) = self.rk.status().terminal {
                let first_seen = self.note_terminal_failure();
                if first_seen.elapsed() < self.abandon_cooldown {
                    return Err(self.report_failure(format!("sidecar is down: {reason}")));
                }
                *self.terminal_since_lock() = None;
                telemetry::log_message("sidecar_budget_reset", &format!("after={reason}"));
                self.ran.store(false, Ordering::Release);
            }
        }
        if !self.ran.load(Ordering::Acquire) {
            // An explicit new run: the only place `start()` is called.
            self.rk
                .start()
                .map_err(|error| self.report_failure(with_spawn_hint(&error.to_string())))?;
            self.ran.store(true, Ordering::Release);
        } else {
            // Recovery: a call makes RightKit grant (or refuse) the on-demand restart with
            // `request_restart`, against the same rolling budget and consecutive-failure cap; it
            // waits out a restart already under way. The probe's own result is not needed.
            let request_id = format!("engine_health:{}", self.next_request_id());
            let probe = build_request(
                &request_id,
                EngineSchemaName::EngineHealth,
                None,
                Some(EnginePayload::Health {
                    status: rightkit_dictation_core::engine::EngineHealthStatus::Ok,
                    diagnostics: None,
                }),
            );
            let method = method_for(&probe)?;
            let params =
                serde_json::to_value(&probe).map_err(|error| format!("encode request: {error}"))?;
            let options = CallOptions {
                timeout: Some(SIDECAR_SEND_TIMEOUT),
                keep_alive_on_timeout: true,
                ..CallOptions::default()
            };
            // Never ignored and never replayed: any error (down, not_ready, timeout,
            // delivery_unknown, engine_exited) is the caller's, and this demand is not retried.
            if let Err(error) = self.rk.call_with(&method, params, options) {
                // The demand that spends the budget reports it through this error, not the
                // terminal check above: start the cooldown from here.
                if matches!(self.rk.status().terminal, Some(Terminal::Failed { .. })) {
                    self.note_terminal_failure();
                }
                return Err(self.report_failure(with_spawn_hint(&error.to_string())));
            }
        }
        Ok(hello_version(self.rk.status().hello.as_ref()))
    }

    fn report_failure(&self, message: String) -> String {
        telemetry::log_message("sidecar_spawn_failed", &message);
        message
    }

    /// Read the BYOK keys fresh and send them to the current generation. Nothing to do when cloud
    /// polish is not eligible (a fresh engine holds no keys). Only a fixed, non-secret reason is
    /// returned on failure.
    fn deliver_provider_keys(&self) -> Result<(), String> {
        let mut config = crate::supervisor_config();
        let Some(keys) = take_provider_keys(&mut config) else {
            return Ok(());
        };
        self.send_provider_keys(keys)
    }

    /// Hand the engine its provider keys as the owner's `rightkit-credentials` snapshot (a full
    /// replacement, rightkit#886) carried by `SetProviderKeys` over the engine's private stdio,
    /// and require the owner's Ack for the same correlation id (`check_ack`). A failure carries
    /// the typed error code (`timeout`, `delivery_unknown`, `engine_exited`, `engine_write`,
    /// `encode_failed`, `ack_invalid`, or `engine_rejected`), never key material; it is not
    /// retried here. Transient secret handling: the slots' `SecretString`s are copied into the
    /// owner's zeroized `Secret`s and wiped as they drop, the encoded frame is a `SecretString`
    /// dropped (wiped) as soon as the request is serialized, and the serialized params live in
    /// RightKit's zeroizing `RawParams` buffer, which `call_raw_with` splices into the outbound
    /// frame without a `serde_json::Value` copy and which is wiped when it drops here.
    fn send_provider_keys(&self, keys: ProviderKeys) -> Result<(), String> {
        let id = self.next_request_id();
        let request_id = format!("set_provider_keys:{id}");
        let ProviderKeys {
            groq,
            nvidia,
            openrouter,
        } = keys;
        let carrier = encode_slots_frame(id, groq, nvidia, openrouter)
            .map_err(|_| delivery_error("encode_failed"))?;
        let frame = build_request(
            &request_id,
            EngineSchemaName::SetProviderKeys,
            None,
            Some(EnginePayload::SetProviderKeys { frame: carrier }),
        );
        let params = RawParams::serialize(&frame, MAX_FRAME_LIMIT)
            .map_err(|_| delivery_error("encode_failed"));
        drop(frame); // wipes the carrier
        let options = CallOptions {
            timeout: Some(PROVIDER_KEYS_ACK_TIMEOUT),
            keep_alive_on_timeout: true,
            ..CallOptions::default()
        };
        let params = params?;
        let reply = self
            .rk
            .call_raw_with("set_provider_keys", &params, options)
            .map_err(|error| delivery_error(&error.code))?;
        drop(params);
        let ack = frame_from_value(&reply)
            .ok()
            .and_then(|frame| match frame.payload {
                Some(EnginePayload::EngineAck {
                    detail: Some(detail),
                    ..
                }) => Some(detail),
                _ => None,
            })
            .ok_or_else(|| delivery_error("ack_invalid"))?;
        check_ack(&ack, id).map_err(|_| delivery_error("ack_invalid"))
    }

    /// Apply an L3 settings / BYOK-key change to the running sidecar without a
    /// restart (go-live M-19). `Ok(true)` = applied live (or no sidecar is
    /// running, so the next spawn reads the new config). `Ok(false)` = this
    /// sidecar does not advertise `live_l3_reconfigure`; nothing was sent and
    /// the caller must restart it. `Err` = the live path was attempted and
    /// failed; the caller must also restart so state cannot diverge.
    /// The keys are re-delivered fresh (an empty set when polish is off).
    pub fn reconfigure_l3_live(&self) -> Result<bool, String> {
        if !self.is_running() {
            return Ok(true);
        }
        self.ensure_ready()?;
        let generation = self.generation();
        if !self.sidecar_supports_live_l3(generation)? {
            return Ok(false);
        }
        let mut config = crate::supervisor_config();
        let provider_state = config.product_provider_state.clone();
        let provider_ready = config.ai_polish_enabled && config.product_provider_ready;
        let ai_polish_enabled = config.ai_polish_enabled;
        // Keys are a full replacement: when polish is off or no provider is eligible the empty
        // set wipes the sidecar's copy.
        let keys = take_provider_keys(&mut config).unwrap_or_else(ProviderKeys::none);
        drop(config);
        self.send_provider_keys(keys)
            .map_err(|reason| format!("live L3 key replace failed: {reason}"))?;
        let request_id = format!("reconfigure_l3:{}", self.next_request_id());
        let frame = build_request(
            &request_id,
            EngineSchemaName::ReconfigureL3,
            None,
            Some(EnginePayload::ReconfigureL3 {
                ai_polish_enabled,
                provider_state,
                provider_ready,
            }),
        );
        let response = self.call(&frame, PROVIDER_KEYS_ACK_TIMEOUT)?;
        match response.error {
            Some(error) => Err(format!("live L3 reconfigure rejected: {}", error.message)),
            None => Ok(true),
        }
    }

    fn sidecar_supports_live_l3(&self, generation: u64) -> Result<bool, String> {
        if let Ok(cached) = self.live_l3_support.lock() {
            if let Some((cached_generation, supported)) = *cached {
                if cached_generation == generation {
                    return Ok(supported);
                }
            }
        }
        let request_id = format!("engine_capabilities:{}", self.next_request_id());
        let frame = build_request(
            &request_id,
            EngineSchemaName::EngineCapabilities,
            None,
            Some(EnginePayload::Capabilities {
                capabilities: Vec::new(),
            }),
        );
        let response = self.call(&frame, PROVIDER_KEYS_ACK_TIMEOUT)?;
        let supported = response.error.is_none()
            && matches!(
                response.payload,
                Some(EnginePayload::Capabilities { ref capabilities })
                    if advertises_live_l3(capabilities)
            );
        if let Ok(mut cached) = self.live_l3_support.lock() {
            *cached = Some((generation, supported));
        }
        Ok(supported)
    }

    /// One request/response round trip with the engine as it stands (no gate, no start). The
    /// engine's own error reply (`ok:false` carrying its error frame) comes back as `Ok(frame)`
    /// with `frame.error` set. A timeout abandons only the wait: the engine is kept alive.
    fn exchange(
        &self,
        frame: &EngineFrame,
        timeout: Duration,
    ) -> Result<EngineFrame, ExchangeError> {
        let method = method_for(frame).map_err(ExchangeError::Failed)?;
        if ACCEPTED_METHODS.contains(&method.as_str()) {
            return Err(ExchangeError::Failed(format!(
                "{method} is acknowledged and completed by event: use send()"
            )));
        }
        let params = serde_json::to_value(frame)
            .map_err(|error| ExchangeError::Failed(format!("encode request: {error}")))?;
        let options = CallOptions {
            timeout: Some(timeout),
            keep_alive_on_timeout: true,
            ..CallOptions::default()
        };
        match self.rk.call_with(&method, params, options) {
            Ok(result) => frame_from_value(&result).map_err(ExchangeError::Failed),
            Err(error) => match error_frame_of(&error) {
                Some(reply) => Ok(reply),
                None if error.code == "timeout" => Err(ExchangeError::NoReply(format!(
                    "sidecar call timeout/request failure: {error}"
                ))),
                None => Err(ExchangeError::Failed(format!(
                    "sidecar call timeout/request failure: {error}"
                ))),
            },
        }
    }

    /// Deliver an accepted-style frame (stop, cancel, context update): the engine answers
    /// `{"accepted":true}` at once and reports the result as an id-less event carrying its
    /// generation (routed by the event bridge, fenced, exactly once). A write that stalls makes
    /// RightKit kill and reap the wedged engine; a missing or negative acceptance is an error.
    fn accept(&self, frame: &EngineFrame) -> Result<(), String> {
        let method = method_for(frame)?;
        if !ACCEPTED_METHODS.contains(&method.as_str()) {
            return Err(format!("{method} is not an accepted-style request"));
        }
        let params =
            serde_json::to_value(frame).map_err(|error| format!("encode request: {error}"))?;
        let options = CallOptions {
            timeout: Some(SIDECAR_SEND_TIMEOUT),
            keep_alive_on_timeout: true,
            ..CallOptions::default()
        };
        parse_accepted(self.rk.call_with(&method, params, options))
    }

    /// Send an `EngineFrame` whose result the shell consumes as an event (stop, cancel, context
    /// update). Never starts the engine; waits (bounded) only for a key delivery in flight.
    pub fn send(&self, frame: &EngineFrame) -> Result<(), String> {
        if !self.is_running() {
            return Err("sidecar not running".to_string());
        }
        self.gate.wait_if_inflight(self.rk.status().generation);
        self.accept(frame)
    }

    /// Deliver a shortcut-press capture-control frame (`capture_warmup`, resume or revoke) to an
    /// engine that is already running. Narrowly typed on purpose: it accepts nothing but a
    /// `CaptureWarmup` frame, it never spawns or readies an engine, and it does not wait for a
    /// provider-key delivery in flight (`send` does, up to the key gate's bound), because a
    /// microphone resume or revoke has nothing to do with keys or config and must not queue behind
    /// them. Only the engine's own immediate acceptance (bounded by `SIDECAR_SEND_TIMEOUT`) is
    /// awaited. A normal start keeps the key gate (`start` and `call`).
    pub fn send_capture_control(&self, frame: &EngineFrame) -> Result<(), String> {
        if frame.schema_name != EngineSchemaName::CaptureWarmup
            || !matches!(frame.payload, Some(EnginePayload::CaptureWarmup { .. }))
        {
            return Err("send_capture_control carries only capture_warmup frames".to_string());
        }
        if !self.is_running() {
            return Err("sidecar not running".to_string());
        }
        self.accept(frame)
    }

    /// The engine is running for a recording start: the same recovery as `start`, but WITHOUT the
    /// provider-key delivery gate. A recording start must not wait (up to the key gate's bound)
    /// for keys that only cloud features need at finalization; the caller delivers them after the
    /// start frame through [`EngineSupervisor::deliver_claimed_provider_keys`]. While the mic
    /// is warm from the shortcut press (the worker's bounded warm-up), this keeps the start frame
    /// from arriving after the warm-up expired.
    pub fn start_without_keys(&self) -> Result<String, String> {
        // A previous recording's background key delivery failed on this generation. This start
        // runs with no session in flight (the shell reserves one start at a time and refuses a
        // start while a dictation is still finishing), so it is the safe moment to end that
        // generation: the engine respawns below and its new generation gets its own delivery.
        let retry = self
            .keys_retry_generation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(generation) = retry {
            // No session of this engine is in flight at a start (the shell refuses a start while a
            // dictation is still finishing), so this retirement, deferred from a capture, is safe.
            self.retire_generation_now(generation);
        }
        self.ensure_running()
    }

    /// `call` for a recording-start frame (and the config pushes before it): readies the engine
    /// like `call`, without the key delivery gate. See [`EngineSupervisor::start_without_keys`].
    pub fn call_without_keys(
        &self,
        frame: &EngineFrame,
        timeout: Duration,
    ) -> Result<EngineFrame, String> {
        self.ensure_running()?;
        self.exchange(frame, timeout)
            .map_err(ExchangeError::into_message)
    }

    /// Take the provider-key delivery claim of the current generation for a recording start,
    /// synchronously and without ever waiting: when this caller is to deliver, the gate has the
    /// delivery in flight before this returns, so a stop that follows cannot get ahead of it
    /// (`send_stop` waits on exactly this claim). When another caller is already delivering, or the
    /// generation settled, the claim is empty and the in-flight delivery is the one a stop waits
    /// for.
    pub fn claim_provider_keys_for_start(&self) -> ProviderKeyClaim {
        let generation = self.rk.status().generation;
        let deliver = matches!(self.gate.try_claim(generation), Claim::Deliver);
        ProviderKeyClaim {
            generation,
            deliver,
        }
    }

    /// Run the delivery for a claim from [`EngineSupervisor::claim_provider_keys_for_start`], on
    /// any thread, WITHOUT ever touching the recording: it never retires the generation, never
    /// shuts the engine down and never starts one. The keys gate only provider (AI cleanup)
    /// processing in the engine; capture start and stop never wait for them. A failed or uncertain
    /// delivery returns `Err` (a fixed, non-secret reason): the generation's gate is settled as
    /// done (it is recording with no keys, so cleanup falls back to local polish, and no other
    /// caller can retire it mid-recording through the gate's failure path) and the generation is
    /// remembered in `keys_retry_generation` for the next start. A claim whose generation has
    /// since been replaced is released without sending (the new generation gets its own claim).
    pub fn deliver_claimed_provider_keys(&self, claim: ProviderKeyClaim) -> Result<(), String> {
        if !claim.deliver {
            return Ok(());
        }
        let generation = claim.generation;
        if self.rk.status().generation != generation {
            self.gate.settle(generation, Ok(()));
            return Ok(());
        }
        let outcome = self.deliver_provider_keys();
        if let Err(reason) = &outcome {
            telemetry::log_message(
                "provider_keys_delivery_failed",
                &format!("generation={generation}; background; reason={reason}"),
            );
            *self
                .keys_retry_generation
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(generation);
        }
        // Settled as done on both outcomes (see above): the recording keeps its engine.
        self.gate.settle(generation, Ok(()));
        outcome
    }

    /// Send the stop (or cancel-to-history) frame of a session: the finalize waits for the
    /// provider-key delivery of the current generation, bounded by `keys_wait`, so a cloud cleanup
    /// never depends on scheduler order (a recording start registers its delivery claim before it
    /// returns). When the wait runs out the stop is sent anyway and finalizes with local polish
    /// unless the keys land first; that is logged. The audio was already sealed by
    /// [`EngineSupervisor::seal_capture`], so this wait never lengthens the recording.
    pub fn send_stop(&self, frame: &EngineFrame, keys_wait: Duration) -> Result<(), String> {
        if !self.is_running() {
            return Err("sidecar not running".to_string());
        }
        let generation = self.rk.status().generation;
        if !self.gate.wait_settled(generation, keys_wait) {
            telemetry::log_message(
                "provider_keys_wait_timeout",
                &format!(
                    "generation={generation}; finalize proceeds without waiting (local polish)"
                ),
            );
        }
        self.accept(frame)
    }

    /// The synchronous, ordered capture seal: send a `CaptureSeal` frame to the running engine and
    /// wait (bounded by `timeout`) for its reply, which is sent only after the worker applied the
    /// seal on the same ordered path as the stop. Never spawns or readies an engine, never waits
    /// on the provider-key gate, no config work. `Err` when no engine runs, the reply does not
    /// come in time or the engine refuses; the stop then seals itself in the worker.
    pub fn seal_capture(
        &self,
        frame: &EngineFrame,
        timeout: Duration,
    ) -> Result<EngineFrame, String> {
        if frame.schema_name != EngineSchemaName::CaptureSeal
            || !matches!(frame.payload, Some(EnginePayload::CaptureSeal { .. }))
        {
            return Err("seal_capture carries only capture_seal frames".to_string());
        }
        if !self.is_running() {
            return Err("sidecar not running".to_string());
        }
        self.exchange(frame, timeout)
            .map_err(ExchangeError::into_message)
    }

    /// Deliver the session-correlated cancel of a start whose RPC ended in an error or a timeout
    /// after its frame may already have reached the engine (the engine can be recording even
    /// though the caller never saw the reply). Narrowly typed: accepts only a `Cancel` stop frame
    /// that names a session, only for an engine that is already running (never spawns one), and
    /// does not wait on the provider-key gate (the cancel must not queue behind the delivery that
    /// may be why the start timed out). The engine closes that session and fences its attempt.
    pub fn send_session_cancel(&self, frame: &EngineFrame) -> Result<(), String> {
        let is_cancel = frame.schema_name == EngineSchemaName::TranscribingStarted
            && frame.session_id.is_some()
            && matches!(
                frame.payload,
                Some(EnginePayload::TranscribingStarted {
                    stop_kind: Some(rightkit_dictation_core::engine::StopKind::Cancel),
                    ..
                })
            );
        if !is_cancel {
            return Err("send_session_cancel carries only a session cancel frame".to_string());
        }
        if !self.is_running() {
            return Err("sidecar not running".to_string());
        }
        self.accept(frame)
    }

    pub fn call(&self, frame: &EngineFrame, timeout: Duration) -> Result<EngineFrame, String> {
        self.ensure_ready()?;
        self.exchange(frame, timeout)
            .map_err(ExchangeError::into_message)
    }

    /// Revoke an in-flight operation and wait for the sidecar's acknowledgement
    /// (#149). The cancel is accepted at once; its completion event
    /// `TranscribingStarted { stop_kind: Cancel }` (request id echoed, no error) is the ack, and
    /// is consumed here, not forwarded. On a missing, errored or wrong ack (or a dead sidecar)
    /// the process is torn down so no late work from the revoked operation can surface; the next
    /// `start()` respawns it. Never spawns a sidecar just to revoke.
    pub fn revoke_with_ack(
        &self,
        frame: &EngineFrame,
        timeout: Duration,
    ) -> super::revocation::RevocationOutcome {
        use super::revocation::{classify_revocation_reply, RevocationOutcome};
        if !self.is_running() {
            return RevocationOutcome::NotRunning;
        }
        let started = Instant::now();
        let (tx, rx) = channel();
        self.router.register(&frame.request_id, tx);
        let reply = match self.accept(frame) {
            Err(message) => {
                self.router.forget(&frame.request_id);
                Err(message)
            }
            Ok(()) => rx
                .recv_timeout(timeout.saturating_sub(started.elapsed()))
                .map_err(|_| {
                    self.router.forget(&frame.request_id);
                    "sidecar call timeout/request failure: revocation completion not received"
                        .to_string()
                }),
        };
        let outcome = classify_revocation_reply(reply);
        if let RevocationOutcome::Failed { reason } = &outcome {
            telemetry::log_message(
                "sidecar_revocation_failed_restart",
                &format!("request_id={}; reason={reason}", frame.request_id),
            );
            self.shutdown();
        }
        outcome
    }

    /// Record that `session_id` began recording (audio-length origin).
    pub fn note_recording_started(&self, session_id: &str) {
        if let Ok(mut w) = self.final_waits.lock() {
            w.note_recording_started(session_id, Instant::now());
        }
    }

    /// Arm the audio-length-scaled final-transcript deadline; call when Stop is sent.
    pub fn arm_final_wait(&self, session_id: &str) {
        if let Ok(mut w) = self.final_waits.lock() {
            w.arm(session_id, Instant::now());
        }
    }

    /// Drop the wait once the session reached a terminal outcome.
    pub fn clear_final_wait(&self, session_id: &str) {
        if let Ok(mut w) = self.final_waits.lock() {
            w.clear(session_id);
        }
    }

    /// True when a final-reply deadline is armed for this session.
    pub fn final_wait_armed(&self, session_id: &str) -> bool {
        self.final_waits
            .lock()
            .map(|w| w.armed(session_id))
            .unwrap_or(false)
    }

    /// True when the session's final reply budget has run out. Lock-light: only
    /// this tracker's own mutex, safe for the watchdog thread.
    pub fn final_wait_expired(&self, session_id: &str) -> bool {
        self.final_waits
            .lock()
            .map(|w| w.expired(session_id, Instant::now()))
            .unwrap_or(false)
    }

    /// Next request id. Monotonic per-supervisor.
    pub fn next_request_id(&self) -> u64 {
        self.next_request_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Current sidecar process generation (every boot attempt counts). Runtime callers use this
    /// to detect restarts performed through a cloned supervisor handle and resend process-local
    /// state.
    pub fn generation(&self) -> u64 {
        self.rk.status().generation
    }

    /// Tear the sidecar down: the engine's own `shutdown`, then the process tree is killed after
    /// the grace period and every process is waited (bounded). Idempotent. This ends the run: the
    /// next `start()` is an explicit new run with a fresh restart budget.
    pub fn shutdown(&self) {
        if let Ok(mut w) = self.final_waits.lock() {
            w.clear_all();
        }
        self.shutdown_generation
            .fetch_max(self.rk.status().generation, Ordering::AcqRel);
        self.ran.store(false, Ordering::Release);
        let report = self.rk.shutdown_detailed();
        telemetry::log_message("sidecar_shutdown", &shutdown_log_value(&report));
    }

    /// Is the sidecar alive and handshaken?
    pub fn is_running(&self) -> bool {
        self.rk.lifecycle() == Lifecycle::Ready
    }
}

fn shutdown_log_value(report: &ShutdownReport) -> String {
    format!(
        "outcome={:?} reaped={} unreaped={} elapsed_ms={}",
        report.outcome,
        report.is_reaped(),
        report.unreaped,
        report.elapsed.as_millis()
    )
}

/// Bridge RightKit's lifecycle and unsolicited-event streams into the shell's `SupervisorEvent`
/// channel. The threads hold only a `Weak` supervisor so dropping the last handle still shuts the
/// engine down, which ends both streams. Exits are classified from generations and the typed
/// `status().terminal` (see `classify_exit`), events are fenced on the engine generation.
fn spawn_bridges(
    rk: &Arc<RkSupervisor>,
    tx: &Sender<SupervisorEvent>,
    note: &SharedSpawnNote,
    router: &Arc<EventRouter>,
    shutdown_generation: &Arc<AtomicU64>,
) {
    let lifecycle_rx = rk.subscribe_lifecycle();
    let events_rx = rk.subscribe_events();

    let weak: Weak<RkSupervisor> = Arc::downgrade(rk);
    let lifecycle_tx = tx.clone();
    let note = Arc::clone(note);
    let lifecycle_router = Arc::clone(router);
    let shutdown_generation = Arc::clone(shutdown_generation);
    let lifecycle_weak = weak.clone();
    let lifecycle = std::thread::Builder::new()
        .name("dictation-engine-lifecycle".to_string())
        .spawn(move || {
            let mut last_reported_generation = 0_u64;
            let mut last_ready_generation = 0_u64;
            for event in lifecycle_rx {
                match event.state {
                    Lifecycle::Ready => {
                        last_ready_generation = event.generation;
                        let hello = lifecycle_weak.upgrade().and_then(|rk| rk.status().hello);
                        if let Some(instance) = hello_instance(hello.as_ref()) {
                            lifecycle_router.set_instance(&instance);
                        }
                        let version = hello_version(hello.as_ref());
                        log_sidecar_started(&note, &version);
                        let _ = lifecycle_tx.send(SupervisorEvent::Ready {
                            engine_version: version,
                        });
                    }
                    Lifecycle::Recovering | Lifecycle::Down => {
                        if event.last_exit_reason.is_none() {
                            continue;
                        }
                        let terminal = lifecycle_weak
                            .upgrade()
                            .map(|rk| rk.status())
                            .filter(|status| status.generation == event.generation)
                            .and_then(|status| status.terminal);
                        let kind = classify_exit(
                            event.generation,
                            last_reported_generation,
                            last_ready_generation,
                            shutdown_generation.load(Ordering::Acquire),
                            terminal.as_ref(),
                        );
                        match kind {
                            ExitKind::Ignore => {}
                            ExitKind::SpawnFailure => {
                                last_reported_generation = event.generation;
                                // The real error text (OS error included) from RightKit's exit reason.
                                let message = with_spawn_hint(
                                    event.last_exit_reason.as_deref().unwrap_or(&event.message),
                                );
                                telemetry::log_message("sidecar_spawn_failed", &message);
                                let _ = lifecycle_tx.send(SupervisorEvent::SpawnFailed { message });
                            }
                            ExitKind::Crash => {
                                last_reported_generation = event.generation;
                                let _ = lifecycle_tx.send(SupervisorEvent::Crashed {
                                    code: event.last_exit_code,
                                });
                            }
                        }
                    }
                    Lifecycle::Booting => {}
                }
            }
        });
    if let Err(error) = lifecycle {
        telemetry::log_message(
            "sidecar_bridge_spawn_failed",
            &format!("lifecycle: {error}"),
        );
    }

    let events_tx = tx.clone();
    let events_router = Arc::clone(router);
    let events = std::thread::Builder::new()
        .name("dictation-engine-events".to_string())
        .spawn(move || {
            for envelope in events_rx {
                let current = || {
                    weak.upgrade()
                        .and_then(|rk| hello_instance(rk.status().hello.as_ref()))
                };
                match events_router.route(&envelope, current) {
                    Routed::Event(event) => {
                        let _ = events_tx.send(SupervisorEvent::EngineEvent(event));
                    }
                    Routed::Stale => telemetry::log_message(
                        "sidecar_event_stale",
                        "dropped an event from another engine generation",
                    ),
                    Routed::Completed | Routed::Ignored => {}
                }
            }
        });
    if let Err(error) = events {
        telemetry::log_message("sidecar_bridge_spawn_failed", &format!("events: {error}"));
    }
}

fn log_sidecar_started(note: &SharedSpawnNote, version: &str) {
    let Ok(slot) = note.lock() else {
        return;
    };
    let Some(spawned) = slot.as_ref() else {
        return;
    };
    let cache_telemetry = engine_cache_telemetry(&spawned.bin, &spawned.app_data_root);
    telemetry::log_message(
        "sidecar_started",
        &format!(
            "bin={} version={version} {} {} models={}",
            spawned.bin.display(),
            binary_identity_suffix(&spawned.bin),
            cache_telemetry,
            spawned.models_base.display()
        ),
    );
}

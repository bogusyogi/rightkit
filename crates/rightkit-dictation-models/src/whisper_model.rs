//! Whisper multilingual model — Pro, downloaded on demand (not bundled).
//!
//! Parakeet (free) auto-detects 25 European languages but has NO language input,
//! so locking to a specific language requires Whisper. Whisper is NOT bundled in
//! the installer; a Pro user downloads it once from Cloudflare R2 the first time
//! they pick a locked language.
//!
//! Two DIFFERENT archive formats land in app-data, per platform:
//! - macOS: CoreML `.mlmodelc` bundle → `<app-data>/models/whisper-multi`,
//!   where the engine's `whisper_coreml_dir` looks for it. Needs the one-time
//!   ANE compile ("prepare") step below.
//! - Windows: a whisper.cpp GGUF file → `<app-data>/models/whisper-win`, where
//!   `rightkit-dictation-engine`'s `whisper_win::resolve_whisper_model` looks for it.
//!   No prepare step (Vulkan CLI loads the GGUF directly on first inference).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant, UNIX_EPOCH};

/// In-process transaction gate for model lifecycle operations.
///
/// Download commands run on independent blocking workers. A per-model gate is
/// required because resume, checksum verification, archive replacement, and
/// activation all share on-disk state. The lock entries intentionally remain
/// allocated for process lifetime; at most [`MAX_OPERATION_LOCKS`] distinct names get one, and
/// every later name shares a single overflow gate (serializing more, never growing the map).
static MODEL_OPERATION_LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();

/// Distinct model names that get their own gate.
const MAX_OPERATION_LOCKS: usize = 64;

/// Return the serialization gate for one model's download/install transaction.
pub fn model_operation_lock(model: &str) -> Arc<Mutex<()>> {
    let locks = MODEL_OPERATION_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut locks = locks
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // Past the bound a name shares the overflow gate (the empty name, which no model has).
    let key = if locks.contains_key(model) || locks.len() < MAX_OPERATION_LOCKS {
        model
    } else {
        ""
    };
    Arc::clone(
        locks
            .entry(key.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(()))),
    )
}

/// Build a resumable archive path scoped to exact model contents.
///
/// A single fixed `.part` path can contain bytes for a different revision
/// after a route/checksum change. Including the verified artifact digest keeps
/// resumptions isolated while retaining restart-resume behavior.
///
/// `model_name` must be a plain name ([`crate::archive_install::is_plain_name`]) and
/// `expected_sha256` 64 hex digits; anything else is refused, so neither can carry a separator or
/// a `..` into the path.
pub fn partial_archive_path(
    models_root: &Path,
    model_name: &str,
    expected_sha256: &str,
) -> Result<PathBuf, String> {
    let sha = expected_sha256.trim().to_ascii_lowercase();
    if !crate::archive_install::is_plain_name(model_name)
        || !crate::ultra_model::is_sha256_hex(&sha)
    {
        return Err("partial archive name is invalid".to_string());
    }
    Ok(models_root.join(format!(".{model_name}.{sha}.tar.gz.part")))
}

pub trait WhisperHost: Send + Sync {
    fn resolve_engine_bin(&self) -> Option<PathBuf>;
    fn has_pro(&self) -> bool;
    fn signed_entitlement_token(&self) -> Option<String>;
    fn emit_progress(&self, phase: &str, received: u64, total: u64, message: &str);
}

/// Gated Cloudflare Worker URL for the Whisper archive (Pro users only). The
/// Worker (`cloudflare-workers/model-serve/`) authorizes the download by
/// VERIFYING the caller's server-signed Ed25519 entitlement token — the same
/// `signed_token` the Rust Pro gate checks (`license::has_pro`). No shared
/// secret ships in the binary, so it can't be extracted to bypass the paywall.
#[cfg(target_os = "macos")]
const WHISPER_ARCHIVE_URL: &str =
    "https://heardright-models.adrdsouza.workers.dev/whisper-multi.tar.gz";
#[cfg(target_os = "macos")]
const WHISPER_ARCHIVE_SHA256: &str =
    "046ce2aea772e2b102a31bfb52143b58b8ab97637dadc661d4723e4fd5a7f1d7";
/// Largest archive the download accepts, and the most it may unpack to. Ceilings above the real
/// archive (a CoreML bundle), not its measured size: the archive carries no pinned size.
#[cfg(target_os = "macos")]
const WHISPER_ARCHIVE_MAX_BYTES: u64 = 4 << 30;
#[cfg(target_os = "macos")]
const WHISPER_INSTALLED_MAX_BYTES: u64 = 8 << 30;

/// Windows whisper.cpp GGML archive — the decided ship model
/// `whisper_turbo_q5_k.bin` (large-v3-turbo, q5_k) from
/// `model_registry/final/asr/`, packaged as a single top-level `whisper-win/`
/// dir. Uploaded to the private R2 bucket `rightapps-updates` at
/// `heardright/whisper-win.tar.gz` (2026-07-03) and served Pro-gated through the
/// same Worker as the macOS CoreML archive.
#[cfg(not(target_os = "macos"))]
const WHISPER_ARCHIVE_URL: &str =
    "https://heardright-models.adrdsouza.workers.dev/whisper-win.tar.gz";
#[cfg(not(target_os = "macos"))]
const WHISPER_ARCHIVE_SHA256: &str =
    "e4fa834a3b6000ca51b71ac42799277b33496e2f86c43e95b3da57b170f76f9f";
/// Largest archive the download accepts, and the most it may unpack to. Ceilings above the real
/// archive (one GGUF file), not its measured size: the archive carries no pinned size.
#[cfg(not(target_os = "macos"))]
const WHISPER_ARCHIVE_MAX_BYTES: u64 = 2 << 30;
#[cfg(not(target_os = "macos"))]
const WHISPER_INSTALLED_MAX_BYTES: u64 = 4 << 30;

fn url_configured() -> bool {
    !WHISPER_ARCHIVE_URL.contains("CHANGE-ME")
}

fn models_dir() -> PathBuf {
    crate::app_paths::app_data_file("models")
}

#[cfg(target_os = "macos")]
fn whisper_dir_name() -> &'static str {
    "whisper-multi"
}

#[cfg(not(target_os = "macos"))]
fn whisper_dir_name() -> &'static str {
    "whisper-win"
}

fn whisper_dir() -> PathBuf {
    models_dir().join(whisper_dir_name())
}

/// Self-heals a process death that landed between
/// `archive_install::install_single_top_level_tar_gz`'s two renames (the
/// previously-installed Whisper archive moved aside, but the replacement
/// never got moved into `whisper_dir()`) by promoting the orphaned aside
/// back before `installed()` trusts what's on disk. See
/// `archive_install::recover_orphaned_previous_install` and
/// `model_paths::local_asr_ready` for the same pattern applied to the
/// primary ASR models.
fn recover_orphaned_install() {
    match crate::archive_install::recover_orphaned_previous_install(
        &models_dir(),
        whisper_dir_name(),
    ) {
        Ok(true) => crate::telemetry::log_message(
            "model_install_recovered",
            &format!(
                "promoted orphaned previous Whisper install back to {} after an interrupted replacement",
                whisper_dir().display()
            ),
        ),
        Ok(false) => {}
        Err(err) => crate::telemetry::log_message(
            "model_install_recovery_failed",
            &format!("failed to recover orphaned previous Whisper install: {err}"),
        ),
    }
}

#[cfg(target_os = "macos")]
fn installed() -> bool {
    recover_orphaned_install();
    let d = whisper_dir();
    d.join("AudioEncoder.mlmodelc").exists() && d.join("TextDecoder.mlmodelc").exists()
}

/// Windows: "installed" means at least one `.gguf` file landed in the whisper
/// dir (the archive contains exactly one; `whisper_win::resolve_whisper_model`
/// picks it up by extension, not a fixed name, so this mirrors that lookup).
#[cfg(not(target_os = "macos"))]
fn installed() -> bool {
    recover_orphaned_install();
    let d = whisper_dir();
    let Ok(entries) = fs::read_dir(&d) else {
        return false;
    };
    entries
        .filter_map(|e| e.ok())
        .take(MAX_DIR_SCAN)
        .any(|e| e.path().extension().and_then(|s| s.to_str()) == Some("gguf"))
}

/// Marker written after the one-time ANE compile (the ~3-6 min "Preparing"
/// step) succeeds. The model is only fully USABLE once this exists — until then
/// loading it would pay the cold compile and block the engine.
fn prepared_marker() -> PathBuf {
    whisper_dir().join(".ane-prepared")
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct PreparedMarker {
    version: u32,
    os_build: String,
    engine_bin: String,
    engine_len: u64,
    engine_modified_s: u64,
    model_fingerprint: String,
}

#[derive(Clone, serde::Serialize)]
pub struct WhisperModelStatus {
    installed: bool,
    prepared: bool,
}

fn prepared_marker_now(host: &dyn WhisperHost) -> Result<PreparedMarker, String> {
    let bin = host
        .resolve_engine_bin()
        .ok_or_else(|| "engine binary not found for model prepare".to_string())?;
    let meta = fs::metadata(&bin).map_err(|e| format!("stat {}: {e}", bin.display()))?;
    Ok(PreparedMarker {
        version: 1,
        os_build: os_build(),
        engine_bin: bin.display().to_string(),
        engine_len: meta.len(),
        engine_modified_s: modified_s(&meta),
        model_fingerprint: model_fingerprint()?,
    })
}

/// Largest prepared-marker file read: a few short fields.
const MAX_MARKER_BYTES: u64 = 64 * 1024;
/// Most directory entries scanned when looking for the model file (Windows).
#[cfg(not(target_os = "macos"))]
const MAX_DIR_SCAN: usize = 1024;

fn prepared(host: &dyn WhisperHost) -> bool {
    let Ok(Some(raw)) =
        crate::confined_remove::read_capped_at(&prepared_marker(), MAX_MARKER_BYTES)
    else {
        return false;
    };
    let Ok(saved) = serde_json::from_slice::<PreparedMarker>(&raw) else {
        return false;
    };
    let Ok(now) = prepared_marker_now(host) else {
        return false;
    };
    marker_matches(&saved, &now)
}

fn marker_matches(saved: &PreparedMarker, now: &PreparedMarker) -> bool {
    saved.version == now.version
        && saved.os_build == now.os_build
        && saved.engine_bin == now.engine_bin
        && saved.engine_len == now.engine_len
        && saved.engine_modified_s == now.engine_modified_s
        && saved.model_fingerprint == now.model_fingerprint
}

#[cfg(target_os = "macos")]
fn model_fingerprint() -> Result<String, String> {
    let d = whisper_dir();
    let mut parts = Vec::new();
    for rel in [
        "AudioEncoder.mlmodelc/model.mil",
        "AudioEncoder.mlmodelc/coremldata.bin",
        "AudioEncoder.mlmodelc/weights/weight.bin",
        "TextDecoder.mlmodelc/model.mil",
        "TextDecoder.mlmodelc/coremldata.bin",
        "TextDecoder.mlmodelc/weights/weight.bin",
        "tokenizer.json",
        "config.json",
        "generation_config.json",
    ] {
        let path = d.join(rel);
        let meta = fs::metadata(&path).map_err(|e| format!("stat {}: {e}", path.display()))?;
        parts.push(format!("{rel}:{}:{}", meta.len(), modified_s(&meta)));
    }
    Ok(rightkit_fs::digest::sha256_bytes(
        parts.join("|").as_bytes(),
    ))
}

/// Windows has no ANE-compile step, so `prepared_marker_now`/`marker_matches`
/// are dead weight there (`prepare_whisper` below just writes the marker
/// unconditionally) — but `prepared_marker_now()` still calls this to build
/// the struct, so it needs a cheap real implementation: fingerprint the one
/// `.gguf` file's size + mtime instead of a fixed CoreML file list.
#[cfg(not(target_os = "macos"))]
fn model_fingerprint() -> Result<String, String> {
    let d = whisper_dir();
    let entries = fs::read_dir(&d).map_err(|e| format!("read {}: {e}", d.display()))?;
    let gguf = entries
        .filter_map(|e| e.ok())
        .take(MAX_DIR_SCAN)
        .map(|e| e.path())
        .find(|p| p.extension().and_then(|s| s.to_str()) == Some("gguf"))
        .ok_or_else(|| format!("no .gguf file in {}", d.display()))?;
    let meta = fs::metadata(&gguf).map_err(|e| format!("stat {}: {e}", gguf.display()))?;
    let part = format!(
        "{}:{}:{}",
        gguf.file_name().and_then(|n| n.to_str()).unwrap_or(""),
        meta.len(),
        modified_s(&meta)
    );
    Ok(rightkit_fs::digest::sha256_bytes(part.as_bytes()))
}

fn modified_s(meta: &fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(target_os = "macos")]
fn os_build() -> String {
    crate::system_identity::current().os_build
}

#[cfg(not(target_os = "macos"))]
fn os_build() -> String {
    "non-macos".to_string()
}

/// Total time one [`download_whisper_model_within`] caller may spend by default: waiting for the
/// model's operation gate, the download (its own client budget is one hour), the install and the
/// preparation. Fixed once when the caller enters; every step is given only what is left, and
/// the download is cut off by it ([`fetch_and_install`]).
const DOWNLOAD_CALL_BUDGET: Duration = Duration::from_secs(2 * 60 * 60);

/// Longest one preparation may take for a caller, counted from the start of that caller's
/// preparation phase (after the download and install): waiting for admission, the engine's run
/// and its termination together. The one-time compile takes minutes
/// (3 to 6 on supported hardware); half an hour bounds an engine that hangs. Not measured on a
/// slow machine.
const PREPARE_BUDGET: Duration = Duration::from_secs(30 * 60);

/// Part of the deadline kept for signalling the engine's process group and reaping its leader:
/// the run is cut off this long before the deadline so that the kill and the reap are given
/// real time, and the whole call still ends by the deadline.
#[cfg(target_os = "macos")]
const TERMINATE_RESERVE: Duration = Duration::from_secs(15);

/// How often a caller polls for the model's operation gate (`try_lock`), and the longest sleep.
const GATE_POLL: Duration = Duration::from_millis(25);

/// Why a whisper model call did not finish.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WhisperModelError {
    /// The caller's absolute deadline passed while it waited for admission, while the engine
    /// was being started, while it ran, or while it was being stopped.
    Timeout,
    /// An earlier preparation of this model left an engine process, or a spawn that has not
    /// returned, without the evidence this module accepts. No new preparation starts until it
    /// has that evidence ([`stop_confirmed`]: the leader reaped by `terminate_tree_bounded` and
    /// the engine's process group observed to have no members at that moment; this does not
    /// show that a descendant that left the group, or work an OS daemon does for the engine, has
    /// stopped); the model is never released on a failure, a refused hand-off or a timeout of
    /// the reap or of the spawn. The reason is a stable code
    /// ([`whisper_preparation_unreaped_code`]) and is also sent as the message of the `error`
    /// progress event.
    PreparationUnreaped,
    /// Any other failure; the text is safe to show.
    Failed(String),
}

impl std::fmt::Display for WhisperModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout => f.write_str("model preparation timed out"),
            Self::PreparationUnreaped => f.write_str(
                "a previous model preparation is still being stopped; try again in a moment",
            ),
            Self::Failed(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for WhisperModelError {}

impl WhisperModelError {
    /// A stable, machine-readable code for the error; the `Display` text may change.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Timeout => "whisper_model_timeout",
            Self::PreparationUnreaped => "whisper_preparation_unreaped",
            Self::Failed(_) => "whisper_model_failed",
        }
    }
}

impl From<String> for WhisperModelError {
    fn from(message: String) -> Self {
        Self::Failed(message)
    }
}

/// An absolute point on the monotonic clock, fixed once when a caller enters. Every wait takes
/// [`Deadline::remaining`]; none starts a fresh budget. All arithmetic is checked.
#[derive(Clone, Copy, Debug)]
struct Deadline(Instant);

impl Deadline {
    fn after(budget: Duration) -> Result<Self, WhisperModelError> {
        Self::from_entry(Instant::now(), budget)
    }

    /// The deadline of a call that entered at `entered`.
    fn from_entry(entered: Instant, budget: Duration) -> Result<Self, WhisperModelError> {
        entered
            .checked_add(budget)
            .map(Self)
            .ok_or(WhisperModelError::Timeout)
    }

    /// What is left, or `None` once the deadline has passed.
    fn remaining(self) -> Option<Duration> {
        self.0
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
    }

    fn earlier(self, other: Self) -> Self {
        Self(self.0.min(other.0))
    }

    /// The absolute instant itself, for a step that derives its own timeout from it when it
    /// starts (the download engine); never a duration taken earlier.
    fn instant(self) -> Instant {
        self.0
    }

    #[cfg(target_os = "macos")]
    fn minus(self, reserve: Duration) -> Option<Self> {
        self.0.checked_sub(reserve).map(Self)
    }
}

/// Take a model gate without waiting past `deadline`: `try_lock` and a short sleep, so a holder
/// that never lets go costs the caller its remaining time and a typed timeout, never a hang. The
/// deadline is read before every `try_lock`, the first included: a zero or spent budget is
/// refused before the gate is taken, even when the gate is free.
fn lock_until(
    gate: &Mutex<()>,
    deadline: Deadline,
) -> Result<std::sync::MutexGuard<'_, ()>, WhisperModelError> {
    loop {
        let left = deadline.remaining().ok_or(WhisperModelError::Timeout)?;
        match gate.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => return Ok(poisoned.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => {}
        }
        std::thread::sleep(GATE_POLL.min(left));
    }
}

/// [`model_operation_lock`]'s gate, taken for at most `budget`: the bounded form of `lock()` for
/// hosts that serialize their own model work on it. A holder that never lets go costs the caller
/// `budget` and [`WhisperModelError::Timeout`].
pub fn lock_model_operation_within(
    gate: &Mutex<()>,
    budget: Duration,
) -> Result<std::sync::MutexGuard<'_, ()>, WhisperModelError> {
    lock_until(gate, Deadline::after(budget)?)
}

/// The result of one preparation, shared by the caller that runs it and every caller that
/// arrived while it ran.
struct Slot {
    result: Mutex<Option<Result<(), WhisperModelError>>>,
    done: Condvar,
}

/// Per-model preparation state. Moves only under the one `PREPARATIONS` lock, whose critical
/// sections are a map lookup and an assignment (no I/O, no wait, no call out), so taking it
/// cannot block on a slow holder.
///
/// - `Idle`: nothing prepared by this process is in flight.
/// - `Running`: one caller runs the engine; everyone else waits on the shared slot.
/// - `Done`: the last run ended at `at`. A caller that entered at or before `at` was concurrent
///   with that run and gets its result; a caller that entered later treats it as `Idle`, so a
///   failed run is retried only by a new call after it has fully ended, never by a waiter.
/// - `Spawning`: the runner's spawn of the engine is in flight on the module's spawn thread
///   ([`spawn_bounded`]). Set from the moment the spawn starts until its outcome is known and,
///   when a child exists, until that child is under the module's custody. Callers wait on the
///   run's slot exactly as for `Running`; nothing else may start or install.
/// - `Unreaped`: a run left an engine process (or a spawn that has not returned, or a spawn
///   that failed after launching something) without the evidence this module accepts. It leaves
///   this state only on that evidence ([`stop_confirmed`]: leader reaped, process group without
///   members at that observation), by the reaper thread or by a preparation caller's own
///   bounded retry ([`retry_reap`]), or when a spawn failed with nothing launched; never on an
///   error, a failed thread spawn, a full or disconnected queue or a timeout. It then becomes
///   `Done` with the run's own result and end time, so a caller that was concurrent with that
///   run still gets its result and only a later call starts a new run. A spawn that failed
///   after launching something, with no handle returned to this module, has no exit in this
///   process: only a new process clears it (see `Custody::Unowned`). The state is per
///   process: it is not saved.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
enum PrepState {
    Idle,
    Running(Arc<Slot>),
    Spawning {
        slot: Arc<Slot>,
        /// This spawn attempt's own number: a spawn thread that finishes late acts only on the
        /// state its own attempt left, never on a newer one.
        ticket: u64,
        /// Set by the spawn thread, under the lock, as its last step before `owned.spawn()`: from
        /// then a launch is in flight and the custody of what it produces stays with the spawn
        /// thread even if the runner gives up. While it is `false` the runner's give-up cancels
        /// the attempt (back to `Running`, nothing launched) and the spawn thread never launches.
        launching: bool,
    },
    Done {
        result: Result<(), WhisperModelError>,
        at: Instant,
    },
    Unreaped {
        cause: UnreapedCause,
        custody: Custody,
        /// What the run that left the process returned, and when it ended (the hand-off).
        result: WhisperModelError,
        ended: Instant,
    },
}

/// Why a model is `Unreaped`, as a stable code ([`UnreapedCause::code`]).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UnreapedCause {
    /// The termination was bounded and did not produce the group-empty evidence in time.
    ReapTimedOut,
    /// The termination returned an error.
    ReapFailed,
    /// The OS refused to start the reaper thread.
    ReaperUnavailable,
    /// The reaper's queue was full.
    ReaperQueueFull,
    /// The reaper's queue is disconnected (the reaper thread has ended).
    ReaperDisconnected,
    /// The reaper stopped without an outcome (it panicked).
    ReaperLost,
    /// The spawn thread has not returned by the time the caller had to answer: whatever it
    /// produces is the module's to stop.
    SpawnPending,
    /// The spawn failed after it launched something (the crate's error carries a
    /// `RunnerFailure`); no handle reached this module.
    SpawnFailedAfterLaunch,
}

impl UnreapedCause {
    fn code(self) -> &'static str {
        match self {
            Self::ReapTimedOut => "reap_timed_out",
            Self::ReapFailed => "reap_failed",
            Self::ReaperUnavailable => "reaper_unavailable",
            Self::ReaperQueueFull => "reaper_queue_full",
            Self::ReaperDisconnected => "reaper_disconnected",
            Self::ReaperLost => "reaper_lost",
            Self::SpawnPending => "spawn_pending",
            Self::SpawnFailedAfterLaunch => "spawn_failed_after_launch",
        }
    }
}

/// Who holds the engine process that has no group-empty evidence yet.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
enum Custody {
    /// The spawn thread with this ticket has not returned. Whatever it returns is terminated
    /// and parked through the reaper path, never dropped.
    Spawning(u64),
    /// A launch happened and no handle reached this module: the crate's error says so but
    /// carries no pid, group id or child. Nobody here can stop it or observe its group, so
    /// there is no exit from this state in this process (crate-side request in HANDOFF.md 4.6).
    Unowned,
    /// The reaper thread holds it and is stopping it.
    Reaper,
    /// A preparation caller took it out to retry the stop ([`retry_reap`]).
    Retrying,
    /// Nobody is working on it; the next preparation attempt for this model retries the stop.
    Held(Box<rightkit_child_process::OwnedChild>),
}

type PrepMap = HashMap<&'static str, PrepState>;

static PREPARATIONS: OnceLock<Mutex<PrepMap>> = OnceLock::new();

fn preparations() -> std::sync::MutexGuard<'static, PrepMap> {
    PREPARATIONS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Publishes the runner's result exactly once, also when the run panics (then as a failure), so
/// a waiter is never left on a slot nobody will fill and the state never stays `Running`.
struct RunGuard {
    key: &'static str,
    slot: Arc<Slot>,
    published: bool,
}

impl RunGuard {
    fn publish(&mut self, result: Result<(), WhisperModelError>) {
        if std::mem::replace(&mut self.published, true) {
            return;
        }
        {
            let mut map = preparations();
            if let Some(state) = map.get_mut(self.key) {
                // Only this run's own `Running` becomes `Done`: `Unreaped` (set at hand-off) and
                // anything later stay as they are.
                if matches!(state, PrepState::Running(s) if Arc::ptr_eq(s, &self.slot)) {
                    *state = PrepState::Done {
                        result: result.clone(),
                        at: Instant::now(),
                    };
                }
            }
        }
        *self
            .slot
            .result
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(result);
        self.slot.done.notify_all();
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        self.publish(Err(WhisperModelError::Failed(
            "model preparation stopped unexpectedly".to_string(),
        )));
    }
}

/// Wait on a run another caller owns, for at most the time this caller has left. A timeout here
/// only ends this caller's wait; the run, and the kill if it hangs, belong to its runner.
fn wait_for_slot(slot: &Slot, deadline: Deadline) -> Result<(), WhisperModelError> {
    let mut guard = slot
        .result
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    loop {
        if let Some(result) = guard.as_ref() {
            return result.clone();
        }
        let left = deadline.remaining().ok_or(WhisperModelError::Timeout)?;
        guard = slot
            .done
            .wait_timeout(guard, left)
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .0;
    }
}

/// What a caller of [`prepare_single_flight`] does, decided under the `PREPARATIONS` lock.
enum Role {
    Finished(Result<(), WhisperModelError>),
    Wait(Arc<Slot>),
    Run(Arc<Slot>),
    /// The model is `Unreaped` with nobody working on the process: this caller retries the stop.
    Reap(Box<rightkit_child_process::OwnedChild>),
    /// The model is `Unreaped` and the reaper (or another caller) is on it: refused.
    Refused(UnreapedCause),
    /// The caller's deadline had passed when it would have become the runner or retried the
    /// stop: nothing was started.
    Expired,
}

/// Take the parked child out of `custody` for a retry, leaving `Retrying` so no one else touches
/// it; any other custody is put back unchanged and `None` is returned.
fn take_held(custody: &mut Custody) -> Option<Box<rightkit_child_process::OwnedChild>> {
    match std::mem::replace(custody, Custody::Retrying) {
        Custody::Held(child) => Some(child),
        other => {
            *custody = other;
            None
        }
    }
}

/// What an `Unreaped` state means for a caller that entered at `entered`, or `None` for any
/// other state. A caller concurrent with the run that left the process takes that run's result;
/// a later caller with no time left starts nothing; otherwise it retries the stop (parked
/// child) or is refused (reaper or another caller on it).
fn unreaped_role(state: &mut PrepState, entered: Instant, deadline: Deadline) -> Option<Role> {
    // The engine's spawn is in flight: nothing else may start or install.
    if matches!(state, PrepState::Spawning { .. }) {
        return Some(Role::Refused(UnreapedCause::SpawnPending));
    }
    let PrepState::Unreaped {
        cause,
        custody,
        result,
        ended,
    } = state
    else {
        return None;
    };
    if *ended >= entered {
        return Some(Role::Finished(Err(result.clone())));
    }
    if deadline.remaining().is_none() {
        return Some(Role::Expired);
    }
    Some(match take_held(custody) {
        Some(child) => Role::Reap(child),
        None => Role::Refused(*cause),
    })
}

/// Decide the caller's role under the `PREPARATIONS` lock. `entered` is the instant the public
/// call began (before the operation gate). The deadline is read inside the same critical
/// section as the transition to `Running` (and as the retry's take of the parked child), so a
/// spent deadline can never be turned into a run: the transition is refused with `Expired`.
fn claim_role(key: &'static str, entered: Instant, deadline: Deadline) -> Role {
    let mut map = preparations();
    let state = map.entry(key).or_insert(PrepState::Idle);
    match &*state {
        PrepState::Running(slot) | PrepState::Spawning { slot, .. } => {
            return Role::Wait(Arc::clone(slot))
        }
        PrepState::Done { result, at } if *at >= entered => return Role::Finished(result.clone()),
        _ => {}
    }
    if let Some(role) = unreaped_role(state, entered, deadline) {
        return role;
    }
    // `Idle`, or a `Done` from before this caller entered: the runner is chosen only with time
    // left.
    if deadline.remaining().is_none() {
        return Role::Expired;
    }
    let slot = Arc::new(Slot {
        result: Mutex::new(None),
        done: Condvar::new(),
    });
    *state = PrepState::Running(Arc::clone(&slot));
    Role::Run(slot)
}

/// Keep conflicting work away from a model whose earlier engine process has no group-empty
/// evidence yet, or whose spawn is outstanding: returns `Ok` only when the model is neither
/// `Unreaped` nor `Spawning`. A caller concurrent with the run that
/// left the process gets that run's result; a later one retries the stop of a parked child
/// ([`retry_reap`], within its own deadline) and goes on only on positive evidence, otherwise it
/// is refused with [`WhisperModelError::PreparationUnreaped`]. Starts nothing itself.
fn exclude_unreaped(
    host: &dyn WhisperHost,
    key: &'static str,
    entered: Instant,
    deadline: Deadline,
) -> Result<(), WhisperModelError> {
    loop {
        let role = {
            let mut map = preparations();
            map.get_mut(key)
                .and_then(|state| unreaped_role(state, entered, deadline))
        };
        match role {
            None => return Ok(()),
            Some(Role::Finished(result)) => return result,
            Some(Role::Reap(child)) => match retry_reap(key, child, deadline) {
                Ok(()) => continue,
                Err((error, cause)) => {
                    report_unreaped(host, cause);
                    return Err(error);
                }
            },
            Some(Role::Refused(cause)) => {
                report_unreaped(host, cause);
                return Err(WhisperModelError::PreparationUnreaped);
            }
            Some(Role::Expired) => return Err(WhisperModelError::Timeout),
            // `unreaped_role` produces none of these; refuse rather than go on.
            Some(Role::Wait(_) | Role::Run(_)) => {
                return Err(WhisperModelError::PreparationUnreaped)
            }
        }
    }
}

/// Tell the host, with the stable codes, that the model is refused because its engine process
/// has no group-empty evidence yet or its spawn is outstanding.
fn report_unreaped(host: &dyn WhisperHost, cause: UnreapedCause) {
    emit(
        host,
        "error",
        0,
        0,
        &format!(
            "{}: {}",
            WhisperModelError::PreparationUnreaped.code(),
            cause.code()
        ),
    );
}

/// The stable reason code while the Whisper model is `Unreaped` (an engine process from an
/// earlier preparation has no group-empty evidence yet, or its spawn is outstanding, and
/// preparation is refused), else `None`. One of `reap_timed_out`, `reap_failed`,
/// `reaper_unavailable`, `reaper_queue_full`, `reaper_disconnected`, `reaper_lost`,
/// `spawn_pending`, `spawn_failed_after_launch`. Per process.
pub fn whisper_preparation_unreaped_code() -> Option<&'static str> {
    match preparations().get(whisper_dir_name()) {
        Some(PrepState::Unreaped { cause, .. }) => Some(cause.code()),
        Some(PrepState::Spawning { .. }) => Some(UnreapedCause::SpawnPending.code()),
        _ => None,
    }
}

/// Make sure the model is prepared, running at most one preparation however many callers ask.
///
/// One absolute deadline (`call`, further capped at [`PREPARE_BUDGET`] from the start of this
/// preparation phase) covers admission, the run and the termination. `entered` is the instant
/// the public call began, taken before the operation gate was waited for: a run that ends at or
/// after it is concurrent with this caller, so a caller that was already queued behind a run
/// that fails takes that run's result and never starts another. A caller that finds the model
/// prepared returns
/// at once; one that finds a run in flight waits on its shared result and never starts another;
/// the one that finds nothing in flight becomes the runner. A failed run is not retried by the
/// callers that waited on it or were concurrent with it, also when it ended `Unreaped`: they get
/// its result, and only a later call starts a new run. A later call that finds the model
/// `Unreaped` first retries the stop within its own deadline ([`retry_reap`]) and starts a run
/// only on positive evidence; otherwise it is refused with
/// [`WhisperModelError::PreparationUnreaped`].
fn prepare_single_flight(
    host: &dyn WhisperHost,
    call: Deadline,
    entered: Instant,
) -> Result<(), WhisperModelError> {
    let deadline = call.earlier(Deadline::after(PREPARE_BUDGET)?);
    if prepared(host) {
        return Ok(());
    }
    if deadline.remaining().is_none() {
        return Err(WhisperModelError::Timeout);
    }
    let key = whisper_dir_name();
    loop {
        match claim_role(key, entered, deadline) {
            Role::Finished(result) => return result,
            Role::Expired => return Err(WhisperModelError::Timeout),
            Role::Wait(slot) => return wait_for_slot(&slot, deadline),
            Role::Run(slot) => {
                let mut guard = RunGuard {
                    key,
                    slot,
                    published: false,
                };
                let result = prepare_whisper(host, deadline, key);
                guard.publish(result.clone());
                return result;
            }
            Role::Refused(cause) => {
                report_unreaped(host, cause);
                return Err(WhisperModelError::PreparationUnreaped);
            }
            Role::Reap(child) => match retry_reap(key, child, deadline) {
                // Positive evidence: the state is released; look at it again.
                Ok(()) => continue,
                Err((error, cause)) => {
                    report_unreaped(host, cause);
                    return Err(error);
                }
            },
        }
    }
}

/// An engine process without group-empty evidence yet, in the reaper's queue. Dropped with the child
/// still inside (the reaper panicked, or the queue's receiver is gone) it parks the child
/// instead of releasing it: the model stays `Unreaped`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
struct ReapJob {
    key: &'static str,
    child: Option<Box<rightkit_child_process::OwnedChild>>,
}

impl Drop for ReapJob {
    fn drop(&mut self) {
        if let Some(child) = self.child.take() {
            park(self.key, child, UnreapedCause::ReaperLost);
        }
    }
}

/// Outstanding unreaped children the reaper accepts. A model with an unreaped child refuses new
/// preparations, so there is at most one per model; the bound is the channel's capacity.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const MAX_OUTSTANDING_REAPS: usize = 4;

/// How long the reaper waits per attempt to get the group-empty evidence for one child, and how many attempts it
/// makes (one minute in all) before it parks the child for the callers' retry.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const REAP_STEP: Duration = Duration::from_secs(10);
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const REAPER_ATTEMPTS: usize = 6;

/// The module's single long-lived reaper thread, started on first need. `None` if the OS refused
/// the thread (the hand-off then parks the child, see [`hand_to_reaper`]).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn reaper() -> Option<&'static std::sync::mpsc::SyncSender<ReapJob>> {
    static REAPER: OnceLock<Option<std::sync::mpsc::SyncSender<ReapJob>>> = OnceLock::new();
    REAPER
        .get_or_init(|| {
            let (tx, rx) = std::sync::mpsc::sync_channel::<ReapJob>(MAX_OUTSTANDING_REAPS);
            std::thread::Builder::new()
                .name("rightkit-model-reaper".into())
                .spawn(move || {
                    for mut job in rx {
                        reap_job(&mut job);
                    }
                })
                .ok()
                .map(|_| tx)
        })
        .as_ref()
}

/// Stop the job's child within [`REAPER_ATTEMPTS`] bounded attempts. Only [`Stop::Gone`]
/// releases the model; a timeout of every attempt or an error parks the child, which stays
/// `Unreaped`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn reap_job(job: &mut ReapJob) {
    let mut cause = UnreapedCause::ReapTimedOut;
    for _ in 0..REAPER_ATTEMPTS {
        let Some(child) = job.child.as_mut() else {
            return;
        };
        match stop_confirmed(child, REAP_STEP) {
            Ok(Stop::Gone(_)) => {
                job.child = None;
                release_unreaped(job.key);
                return;
            }
            Ok(Stop::NotConfirmed) => {}
            Err(_) => {
                cause = UnreapedCause::ReapFailed;
                break;
            }
        }
    }
    if let Some(child) = job.child.take() {
        park(job.key, child, cause);
    }
}

/// `Unreaped` becomes `Done` with the result and end time of the run that left the process; any
/// other state stays. Called only right after [`Stop::Gone`] (the leader reaped and its group seen empty):
/// the reaper's and [`retry_reap`]'s success paths.
fn release_unreaped(key: &'static str) {
    let mut map = preparations();
    let released = match map.get(key) {
        Some(PrepState::Unreaped { result, ended, .. }) => Some((result.clone(), *ended)),
        _ => None,
    };
    if let Some((result, ended)) = released {
        // The displaced `Unreaped` owns no process here: its child was taken out first.
        map.insert(
            key,
            PrepState::Done {
                result: Err(result),
                at: ended,
            },
        );
    }
}

/// Keep an unconfirmed child for the next preparation attempt's retry; the model stays
/// `Unreaped` with `cause`.
fn park(key: &'static str, child: Box<rightkit_child_process::OwnedChild>, cause: UnreapedCause) {
    let displaced = {
        let mut map = preparations();
        match map.get_mut(key) {
            Some(PrepState::Unreaped {
                cause: current,
                custody,
                ..
            }) => {
                *current = cause;
                Some(std::mem::replace(custody, Custody::Held(child)))
            }
            _ => {
                map.insert(
                    key,
                    PrepState::Unreaped {
                        cause,
                        custody: Custody::Held(child),
                        result: WhisperModelError::Failed(
                            "model preparation stopped unexpectedly".to_string(),
                        ),
                        ended: Instant::now(),
                    },
                );
                None
            }
        }
    };
    // What was displaced (`Reaper` or `Retrying`) owns nothing; it is dropped outside the lock.
    drop(displaced);
}

/// Retry stopping a parked child, as a preparation caller, within that caller's own deadline.
/// `Ok` only on [`Stop::Gone`], after which the model is released. A timeout, an error or an
/// already spent deadline parks the child again and the model stays `Unreaped`.
fn retry_reap(
    key: &'static str,
    child: Box<rightkit_child_process::OwnedChild>,
    deadline: Deadline,
) -> Result<(), (WhisperModelError, UnreapedCause)> {
    // Owns the child for the retry; its drop (also on a panic) parks it again.
    struct Parked {
        key: &'static str,
        child: Option<Box<rightkit_child_process::OwnedChild>>,
        cause: UnreapedCause,
    }
    impl Drop for Parked {
        fn drop(&mut self) {
            if let Some(child) = self.child.take() {
                park(self.key, child, self.cause);
            }
        }
    }
    let mut held = Parked {
        key,
        child: Some(child),
        cause: UnreapedCause::ReapTimedOut,
    };
    let Some(left) = deadline.remaining() else {
        return Err((WhisperModelError::Timeout, held.cause));
    };
    let outcome = held.child.as_mut().map(|child| stop_confirmed(child, left));
    match outcome {
        Some(Ok(Stop::Gone(_))) => {
            held.child = None;
            release_unreaped(key);
            Ok(())
        }
        Some(Ok(Stop::NotConfirmed)) | None => {
            Err((WhisperModelError::PreparationUnreaped, held.cause))
        }
        Some(Err(_)) => {
            held.cause = UnreapedCause::ReapFailed;
            Err((WhisperModelError::PreparationUnreaped, held.cause))
        }
    }
}

/// Hand a child without group-empty evidence to the reaper, recording the result of the run that
/// left it. The model is marked `Unreaped` first, so no caller can start another preparation in
/// the gap. If the reaper cannot take it (the thread could not be started, the queue is full or
/// disconnected) the child is parked ([`park`]) and the model STAYS `Unreaped`: nothing releases
/// it but the group-empty evidence, from the reaper or from a later caller's
/// [`retry_reap`]. No caller thread and no lock is held by the child.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn hand_to_reaper(
    key: &'static str,
    child: rightkit_child_process::OwnedChild,
    cause: UnreapedCause,
    result: WhisperModelError,
) {
    let previous = preparations().insert(
        key,
        PrepState::Unreaped {
            cause,
            custody: Custody::Reaper,
            result,
            ended: Instant::now(),
        },
    );
    drop(previous);
    queue_for_reaper(key, Box::new(child));
}

/// Queue a child for the reaper thread; when the reaper cannot take it the child is parked
/// ([`park`]) and its model stays `Unreaped`. The caller has already marked the model.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn queue_for_reaper(key: &'static str, child: Box<rightkit_child_process::OwnedChild>) {
    let mut job = ReapJob {
        key,
        child: Some(child),
    };
    let why = match reaper() {
        None => UnreapedCause::ReaperUnavailable,
        Some(tx) => match tx.try_send(job) {
            Ok(()) => return,
            Err(std::sync::mpsc::TrySendError::Full(returned)) => {
                job = returned;
                UnreapedCause::ReaperQueueFull
            }
            Err(std::sync::mpsc::TrySendError::Disconnected(returned)) => {
                job = returned;
                UnreapedCause::ReaperDisconnected
            }
        },
    };
    if let Some(child) = job.child.take() {
        park(key, child, why);
    }
}

/// Whether the engine may still be started: there is time before `deadline` minus
/// [`TERMINATE_RESERVE`]. A run that would be cut off before it begins is never spawned.
#[cfg(target_os = "macos")]
fn run_window_open(deadline: Deadline) -> bool {
    deadline
        .minus(TERMINATE_RESERVE)
        .is_some_and(|end| end.remaining().is_some())
}

/// Longest the crate may spend killing and reaping a child it launched but could not tie to this
/// process (the watchdog could not be armed): `OwnedCommand::partial_spawn_cleanup_timeout`.
/// Without it that cleanup waits without a bound (`owned.rs:1719-1722`).
#[cfg(target_os = "macos")]
const PARTIAL_SPAWN_CLEANUP: Duration = Duration::from_secs(5);

/// Numbers spawn attempts, so a spawn thread that finishes late acts only on the state its own
/// attempt left.
#[cfg(target_os = "macos")]
static SPAWN_TICKETS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

#[cfg(target_os = "macos")]
type SpawnOutcome = std::io::Result<rightkit_child_process::OwnedChild>;

/// Whether a failed `OwnedCommand::spawn` launched something. The crate wraps every failure
/// after the child exists in a `RunnerFailure` (`spawn_cleanup_failure`, `owned.rs:1601-1611`:
/// the watchdog could not be armed, the identity could not be proven, the handle could not be
/// made); it carries no pid, group id or child. A failure without one is the standard library's
/// own `Command::spawn` or an argument refusal, after which no child exists.
#[cfg(target_os = "macos")]
fn launched_something(error: &std::io::Error) -> bool {
    rightkit_child_process::RunnerFailure::from_io(error).is_some()
}

/// `Running` becomes `Spawning` for the runner's spawn, under the `PREPARATIONS` lock. `None`
/// when the model is not `Running` (the invariant is broken): nothing is started.
#[cfg(target_os = "macos")]
fn begin_spawn(key: &'static str) -> Option<u64> {
    let mut map = preparations();
    let state = map.get_mut(key)?;
    let PrepState::Running(slot) = state else {
        return None;
    };
    let slot = Arc::clone(slot);
    let ticket = SPAWN_TICKETS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    *state = PrepState::Spawning {
        slot,
        ticket,
        launching: false,
    };
    Some(ticket)
}

/// The spawn thread could not be started, so nothing was launched: `Spawning` goes back to
/// `Running`.
#[cfg(target_os = "macos")]
fn end_spawn_without_launch(key: &'static str, ticket: u64) {
    let mut map = preparations();
    let slot = match map.get(key) {
        Some(PrepState::Spawning {
            slot, ticket: t, ..
        }) if *t == ticket => Arc::clone(slot),
        _ => return,
    };
    drop(map.insert(key, PrepState::Running(slot)));
}

/// A spawn failed after it launched something and no handle reached this module: the model is
/// `Unreaped` with `Custody::Unowned` (no exit in this process). Replaces the run's `Running`.
#[cfg(target_os = "macos")]
fn exclude_after_failed_launch(key: &'static str, result: WhisperModelError) {
    drop(preparations().insert(
        key,
        PrepState::Unreaped {
            cause: UnreapedCause::SpawnFailedAfterLaunch,
            custody: Custody::Unowned,
            result,
            ended: Instant::now(),
        },
    ));
}

/// What the runner got when it stopped waiting for the spawn thread.
#[cfg(target_os = "macos")]
enum Waited {
    Outcome(SpawnOutcome),
    /// The runner's time ran out first. If no launch had started the attempt is cancelled (the
    /// model is `Running` again, nothing launched); if one was in flight the model is `Unreaped`
    /// with `Custody::Spawning` and the spawn thread owns whatever it produces.
    GaveUp,
    /// The spawn thread stopped without an outcome (it panicked); its guard has marked the model.
    Lost,
}

/// The spawn thread's last step before `owned.spawn()`, under the `PREPARATIONS` lock (which is
/// released before the spawn): `true`, with `launching` set, only if the model is still this
/// attempt's `Spawning` (the runner has not given up and nothing superseded it) and the run
/// window (`window_end`, the deadline minus [`TERMINATE_RESERVE`]) is still open. Otherwise
/// nothing is launched and the state is not touched here: the runner's give-up
/// ([`collect_or_abandon`]) has already released the model, or will on seeing this attempt
/// still `Spawning` without `launching`.
#[cfg(target_os = "macos")]
fn begin_launch(key: &'static str, ticket: u64, window_end: Option<Deadline>) -> bool {
    let mut map = preparations();
    match map.get_mut(key) {
        Some(PrepState::Spawning {
            ticket: t,
            launching,
            ..
        }) if *t == ticket && !*launching && window_end.and_then(Deadline::remaining).is_some() => {
            *launching = true;
            true
        }
        _ => false,
    }
}

/// The runner's wait ended without an outcome. Decided under the `PREPARATIONS` lock, the same
/// lock the spawn thread delivers under, so exactly one side wins: if the state is still this
/// attempt's `Spawning` the runner gives up. If the spawn thread has not marked `launching`,
/// the attempt is cancelled (`Running`, nothing launched, the thread will not launch); if it
/// has, the runner hands the spawn thread the custody
/// (`Unreaped`, `SpawnPending`); otherwise the thread has already delivered (before changing
/// the state) and the outcome is read from the channel.
#[cfg(target_os = "macos")]
fn collect_or_abandon(
    key: &'static str,
    ticket: u64,
    rx: &std::sync::mpsc::Receiver<SpawnOutcome>,
) -> Waited {
    {
        let mut map = preparations();
        let (mine, launching, slot) = match map.get(key) {
            Some(PrepState::Spawning {
                slot,
                ticket: t,
                launching,
            }) if *t == ticket => (true, *launching, Some(Arc::clone(slot))),
            _ => (false, false, None),
        };
        if let (true, false, Some(slot)) = (mine, launching, slot) {
            // No launch has started and none will: the spawn thread checks this same state
            // under this same lock as its last step before `owned.spawn()` and finds it gone.
            // The model is released as `Running`, like a spawn thread that never started.
            drop(map.insert(key, PrepState::Running(slot)));
            return Waited::GaveUp;
        }
        if mine {
            drop(map.insert(
                key,
                PrepState::Unreaped {
                    cause: UnreapedCause::SpawnPending,
                    custody: Custody::Spawning(ticket),
                    result: WhisperModelError::Timeout,
                    ended: Instant::now(),
                },
            ));
            return Waited::GaveUp;
        }
    }
    match rx.try_recv() {
        Ok(outcome) => Waited::Outcome(outcome),
        Err(_) => Waited::Lost,
    }
}

/// Start the engine on a module-owned thread and wait for it until the end of the run window
/// (`window_end`, the deadline minus [`TERMINATE_RESERVE`]), so the caller never waits on the
/// crate's unbounded setup. The model is `Spawning` from here until the outcome is known and,
/// when a child exists, until it is in the module's custody.
///
/// - The child arrives in time: it is returned to the runner and the model is `Running` again.
/// - The window ends before the spawn thread has marked `launching` (its last step before
///   `owned.spawn()`, under the lock): the caller gets [`WhisperModelError::Timeout`], the
///   attempt is cancelled (`Running`, nothing launched) and the spawn thread never launches.
/// - The window ends after it has (a launch is in flight): the caller gets
///   [`WhisperModelError::Timeout`], the model becomes `Unreaped` with `Custody::Spawning(ticket)` (`spawn_pending`), and when the spawn thread
///   later returns a child it is handed to the reaper path (`finish_abandoned`), which
///   terminates and parks it; it is never dropped.
/// - The spawn fails after launching something (`launched_something`): the model is `Unreaped`
///   with `Custody::Unowned` (`spawn_failed_after_launch`), whichever side saw the error.
/// - The spawn fails with nothing launched: the run fails; if the caller had already left, the
///   model goes back to `Done` with the run's own result.
///
/// At most one spawn thread per model is outstanding: `Spawning` and `Unreaped` refuse a new
/// preparation of the model. The caller's thread never waits past `window_end`; the thread
/// itself may stay blocked inside the crate, which this module cannot bound (HANDOFF.md 4.6).
#[cfg(target_os = "macos")]
fn spawn_bounded(
    key: &'static str,
    owned: rightkit_child_process::OwnedCommand,
    window_end: Option<Deadline>,
) -> Result<rightkit_child_process::OwnedChild, WhisperModelError> {
    let Some(ticket) = begin_spawn(key) else {
        return Err(WhisperModelError::Failed(
            "model preparation state changed before the engine started".to_string(),
        ));
    };
    let (tx, rx) = std::sync::mpsc::sync_channel::<SpawnOutcome>(1);
    let started = std::thread::Builder::new()
        .name("rightkit-model-spawn".into())
        .spawn(move || run_spawn(key, ticket, owned, window_end, tx));
    if started.is_err() {
        // The thread never ran, so nothing was launched.
        end_spawn_without_launch(key, ticket);
        return Err(WhisperModelError::Failed(
            "prepare (ANE compile) spawn: the spawn thread could not be started".to_string(),
        ));
    }
    let waited = match window_end.and_then(Deadline::remaining) {
        Some(left) => match rx.recv_timeout(left) {
            Ok(outcome) => Waited::Outcome(outcome),
            Err(_) => collect_or_abandon(key, ticket, &rx),
        },
        None => collect_or_abandon(key, ticket, &rx),
    };
    match waited {
        Waited::Outcome(Ok(child)) => Ok(child),
        Waited::Outcome(Err(error)) => {
            let result = WhisperModelError::Failed(format!("prepare (ANE compile) spawn: {error}"));
            if launched_something(&error) {
                exclude_after_failed_launch(key, result.clone());
            }
            Err(result)
        }
        Waited::GaveUp => Err(WhisperModelError::Timeout),
        Waited::Lost => Err(WhisperModelError::Failed(
            "prepare (ANE compile) spawn: the spawn thread stopped unexpectedly".to_string(),
        )),
    }
}

/// The spawn thread's body: the one call that can block inside the crate.
#[cfg(target_os = "macos")]
fn run_spawn(
    key: &'static str,
    ticket: u64,
    owned: rightkit_child_process::OwnedCommand,
    window_end: Option<Deadline>,
    tx: std::sync::mpsc::SyncSender<SpawnOutcome>,
) {
    let report = SpawnReport {
        key,
        ticket,
        delivered: false,
    };
    // The last step before the process can exist: the runner is still this attempt's waiter and
    // the run window is open, checked and marked `launching` under the lock. If not, nothing is
    // launched; the runner's give-up owns the state (it never reads an outcome from this thread
    // in that case, so the guard has nothing to mark).
    if !begin_launch(key, ticket, window_end) {
        let mut report = report;
        report.delivered = true;
        return;
    }
    let mut outcome = owned.spawn();
    if let Ok(child) = outcome.as_mut() {
        // Not even an unwinding panic may add the crate's two-second drop wait to a deadline.
        child.set_drop_reap_timeout(Duration::ZERO);
    }
    report.deliver(&tx, outcome);
}

/// Marks the model if the spawn thread ends without delivering (it panicked inside the spawn):
/// something may have been launched and nothing is known about it.
#[cfg(target_os = "macos")]
struct SpawnReport {
    key: &'static str,
    ticket: u64,
    delivered: bool,
}

#[cfg(target_os = "macos")]
impl SpawnReport {
    /// Deliver the outcome under the `PREPARATIONS` lock. If the state is still this attempt's
    /// `Spawning` the runner is waiting: the outcome goes through the channel and the model is
    /// `Running` again. Otherwise the runner has left (or its receiver is gone) and this thread
    /// owns the outcome ([`finish_abandoned`]).
    fn deliver(mut self, tx: &std::sync::mpsc::SyncSender<SpawnOutcome>, outcome: SpawnOutcome) {
        self.delivered = true;
        let mut outcome = outcome;
        {
            let mut map = preparations();
            let waiting = match map.get(self.key) {
                Some(PrepState::Spawning { slot, ticket, .. }) if *ticket == self.ticket => {
                    Some(Arc::clone(slot))
                }
                _ => None,
            };
            if let Some(slot) = waiting {
                match tx.send(outcome) {
                    Ok(()) => {
                        drop(map.insert(self.key, PrepState::Running(slot)));
                        return;
                    }
                    Err(std::sync::mpsc::SendError(back)) => {
                        // The runner's receiver is gone: take the custody over.
                        outcome = back;
                        drop(map.insert(
                            self.key,
                            PrepState::Unreaped {
                                cause: UnreapedCause::SpawnPending,
                                custody: Custody::Spawning(self.ticket),
                                result: WhisperModelError::Timeout,
                                ended: Instant::now(),
                            },
                        ));
                    }
                }
            }
        }
        finish_abandoned(self.key, self.ticket, outcome);
    }
}

#[cfg(target_os = "macos")]
impl Drop for SpawnReport {
    fn drop(&mut self) {
        if self.delivered {
            return;
        }
        let mut map = preparations();
        let ours = match map.get(self.key) {
            Some(PrepState::Spawning { ticket, .. }) => *ticket == self.ticket,
            Some(PrepState::Unreaped {
                custody: Custody::Spawning(ticket),
                ..
            }) => *ticket == self.ticket,
            _ => false,
        };
        if ours {
            drop(
                map.insert(
                    self.key,
                    PrepState::Unreaped {
                        cause: UnreapedCause::SpawnFailedAfterLaunch,
                        custody: Custody::Unowned,
                        result: WhisperModelError::Failed(
                            "prepare (ANE compile) spawn: the spawn thread stopped unexpectedly"
                                .to_string(),
                        ),
                        ended: Instant::now(),
                    },
                ),
            );
        }
    }
}

/// The spawn thread owns the outcome of an attempt the runner left (state `Unreaped` with this
/// ticket's `Custody::Spawning`):
/// - a child goes to the reaper path (`Custody::Reaper`, then `queue_for_reaper`, which parks it
///   when the reaper cannot take it): terminated by `stop_confirmed`, released only on
///   [`Stop::Gone`], never dropped alive;
/// - an error after a launch leaves the model `Unreaped` with `Custody::Unowned`
///   (`spawn_failed_after_launch`);
/// - an error with nothing launched releases the model to `Done` with the run's own result: the
///   standard library's spawn contract (no child exists after a failed `Command::spawn`) is the
///   evidence, not read from a run on a Mac.
///
/// State edits happen only while the state still carries this ticket.
#[cfg(target_os = "macos")]
fn finish_abandoned(key: &'static str, ticket: u64, outcome: SpawnOutcome) {
    match outcome {
        Ok(child) => {
            {
                let mut map = preparations();
                if let Some(PrepState::Unreaped { cause, custody, .. }) = map.get_mut(key) {
                    if matches!(custody, Custody::Spawning(t) if *t == ticket) {
                        *cause = UnreapedCause::ReapTimedOut;
                        *custody = Custody::Reaper;
                    }
                }
            }
            queue_for_reaper(key, Box::new(child));
        }
        Err(error) if launched_something(&error) => {
            let mut map = preparations();
            if let Some(PrepState::Unreaped { cause, custody, .. }) = map.get_mut(key) {
                if matches!(custody, Custody::Spawning(t) if *t == ticket) {
                    *cause = UnreapedCause::SpawnFailedAfterLaunch;
                    *custody = Custody::Unowned;
                }
            }
        }
        Err(_) => {
            let mut map = preparations();
            let released = match map.get(key) {
                Some(PrepState::Unreaped {
                    custody: Custody::Spawning(t),
                    result,
                    ended,
                    ..
                }) if *t == ticket => Some((result.clone(), *ended)),
                _ => None,
            };
            if let Some((result, ended)) = released {
                drop(map.insert(
                    key,
                    PrepState::Done {
                        result: Err(result),
                        at: ended,
                    },
                ));
            }
        }
    }
}

/// What [`stop_confirmed`] proved.
enum Stop {
    /// The leader was reaped after the group was signalled AND the leader's process group then
    /// had no members at one observation. The only evidence this module accepts as "safe to
    /// start conflicting work on this model" (see [`stop_confirmed`]). It shows the group's
    /// membership at that moment and nothing more: not that a descendant that left the group has
    /// stopped, and not that work an OS daemon does for the engine has finished.
    Gone(
        #[cfg_attr(
            not(target_os = "macos"),
            expect(
                dead_code,
                reason = "only macOS runs the ANE compiler and needs its exit status"
            )
        )]
        std::process::ExitStatus,
    ),
    /// Not proven within the time given.
    NotConfirmed,
}

/// Stop the engine's process group and observe it without members, within `timeout` in all (one
/// absolute deadline for both steps).
///
/// `OwnedChild::terminate_tree_bounded` returns `ReapOutcome::Exited` after the leader was
/// reaped. What that proves differs by platform (`rightkit-child-process` `src/owned.rs`): on
/// Windows it also means every process of the child's job has exited (the job-member wait that
/// follows the reap); on Unix it means the leader was reaped after TERM and then KILL were sent
/// to its process group while the leader still pinned the group id (`start_termination_within`),
/// and nothing more: KILL is delivered asynchronously, so a descendant may still be dying
/// (or be stuck in an uninterruptible state), and a descendant that left the group is never
/// signalled. A reaped leader's group can no longer be signalled by the crate, so nothing sends
/// it another signal later.
///
/// The crate's own observation for the missing half is used here:
/// `rightkit_child_process::wait_process_group_empty` (signal 0 to the group, `ESRCH` means
/// empty, observation only, it never signals). The engine is spawned with the default Unix
/// containment, in a group of its own whose id is its pid (`setpgid(0, 0)`), and while any
/// member lives the kernel does not reuse that id, so "empty" cannot be reported for a group
/// that has members; a zombie member that nobody reaps keeps the group present, which only makes
/// this stricter. Evidence accepted: `Exited` from the termination, then an empty group within
/// what is left of `timeout`. An error from either step is an error (the model stays
/// `Unreaped`). Not covered, by this evidence or by anything the crate offers today: a
/// descendant that moved out of the group, and work an OS daemon does on the engine's behalf
/// (the Core ML compile is staged by `aned`, see `ane_cache_prune`); until the crate does
/// the former (see HANDOFF.md 4.6) those two stay outside the guarantee.
fn stop_confirmed(
    child: &mut rightkit_child_process::OwnedChild,
    timeout: Duration,
) -> std::io::Result<Stop> {
    use rightkit_child_process::ReapOutcome;
    #[cfg(unix)]
    let started = Instant::now();
    let status = match child.terminate_tree_bounded(timeout)? {
        ReapOutcome::Exited(status) => status,
        ReapOutcome::TimedOut => return Ok(Stop::NotConfirmed),
    };
    #[cfg(unix)]
    {
        let Some(group) = std::num::NonZeroU32::new(child.id()) else {
            return Ok(Stop::NotConfirmed);
        };
        let left = timeout.saturating_sub(started.elapsed());
        if !rightkit_child_process::wait_process_group_empty(group, left)? {
            return Ok(Stop::NotConfirmed);
        }
    }
    Ok(Stop::Gone(status))
}

/// Wait, without reaping, until the engine's leader has exited or `end` passes. Each wait is
/// given only what is left.
#[cfg(target_os = "macos")]
fn wait_exited_until(
    handle: &rightkit_child_process::ProcessHandle,
    end: Option<Deadline>,
) -> std::io::Result<bool> {
    let Some(end) = end else {
        return Ok(false);
    };
    while let Some(left) = end.remaining() {
        if handle.wait_exited_timeout(left)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Run the engine's `prepare-whisper` subcommand to trigger + cache the ANE
/// compile, with a "preparing" progress phase. macOS only (ANE).
///
/// Everything is bounded by `deadline`. The engine is started only if a run window is left
/// (time before `deadline` minus [`TERMINATE_RESERVE`]), checked before anything is emitted and
/// again immediately before the spawn; a zero or spent budget returns
/// [`WhisperModelError::Timeout`] and spawns nothing. The engine is waited for without reaping
/// until `deadline` minus [`TERMINATE_RESERVE`]; then, whether the leader exited or is still
/// running, [`stop_confirmed`] signals the group, reaps the leader and observes its process group
/// empty within what is left of the deadline: a leader that exited on its own keeps its own
/// status and its stragglers are killed; a leader still running is killed and the preparation
/// fails with [`WhisperModelError::Timeout`]. A child without the group-empty evidence by the
/// deadline, or whose stop failed, goes to the reaper ([`hand_to_reaper`]) and the model is
/// refused (`Unreaped`) until [`Stop::Gone`]; it is never released otherwise. The child is never
/// dropped alive: every path after the spawn ends in a reaped child or in the reaper's or the
/// parking custody, and the child's drop wait is set to zero (on the spawn thread, before the
/// child is handed over) so that not even an unwinding panic between the spawn and the stop
/// adds the crate's two-second drop budget (`DEFAULT_DROP_REAP_TIMEOUT`, `src/owned.rs`) to the
/// deadline.
///
/// The spawn itself is not run on the caller's thread: `OwnedCommand::spawn` with
/// `kill_on_parent_exit(true)` can block inside `rightkit-child-process` with no bound (the
/// fork and `waitpid(middle, 0)` of the watchdog setup, `process_unix.rs:273-297`, and the
/// unbounded wait of a failed arm's cleanup when no timeout is set, `owned.rs:795-806` and
/// `owned.rs:1717-1722`). [`spawn_bounded`] runs it on a module-owned thread and waits for its
/// outcome only until the end of the run window; what the spawn produces later is the module's
/// to stop, never dropped.
#[cfg(target_os = "macos")]
fn prepare_whisper(
    host: &dyn WhisperHost,
    deadline: Deadline,
    key: &'static str,
) -> Result<(), WhisperModelError> {
    let no_run_window = || {
        let e = WhisperModelError::Timeout;
        emit(host, "error", 0, 0, &e.to_string());
        e
    };
    if !run_window_open(deadline) {
        return Err(no_run_window());
    }
    emit(
        host,
        "preparing",
        0,
        0,
        "Preparing model… one-time, ~3-6 min.",
    );
    let bin = host
        .resolve_engine_bin()
        .ok_or_else(|| "engine binary not found for model prepare".to_string())?;
    let mut owned = rightkit_child_process::OwnedCommand::new(&bin);
    // The engine derives the Whisper directory itself (`<models base>/whisper-multi`); a
    // directory argument would be an argv-selected model path.
    owned.command_mut().arg("prepare-whisper");
    // Lifecycle policy, not retirement proof: an app-owned engine stops with its parent. If
    // this process dies (also by SIGKILL) a watchdog sends the engine's process group TERM and,
    // after a short grace, KILL, unless the leader exited first: the watchdog then exits
    // without the group KILL (`process_unix.rs:479,503-508`), so a same-group helper can
    // survive the parent's death. Best effort. Nothing in this module reads that work or
    // relies on it: a model is released only by `Stop::Gone`, and the watchdog is not an exit
    // from `Unreaped`. On macOS the
    // trigger is the whole process exiting (`owned.rs:504-509`), so the spawn thread's own
    // lifetime does not matter; on Linux the signal is keyed to the spawning thread, so this
    // call must not be enabled there without a spawn thread that lives as long as the child.
    owned.kill_on_parent_exit(true);
    // Bound the crate's cleanup of a child it launched and could not tie to this process
    // (`owned.rs:795-806` goes through `cleanup_direct`, which waits without a bound when no
    // timeout is set, `owned.rs:1719-1722`). A child not reaped in time is handed to the
    // crate's own `rightkit-partial-reap` thread and the spawn fails with a `RunnerFailure`,
    // which `spawn_bounded` treats as "something was launched".
    owned.partial_spawn_cleanup_timeout(PARTIAL_SPAWN_CLEANUP);
    // The last check before the process exists.
    if !run_window_open(deadline) {
        return Err(no_run_window());
    }
    let mut child = spawn_bounded(key, owned, deadline.minus(TERMINATE_RESERVE)).map_err(|e| {
        if e == WhisperModelError::Timeout {
            emit(host, "error", 0, 0, &e.to_string());
        }
        e
    })?;
    let waited = child
        .process_handle()
        .and_then(|handle| wait_exited_until(&handle, deadline.minus(TERMINATE_RESERVE)));
    let stopped = stop_confirmed(&mut child, deadline.remaining().unwrap_or(Duration::ZERO));
    let status = match (stopped, waited) {
        (Ok(Stop::Gone(status)), Ok(true)) => status,
        (Ok(Stop::Gone(_)), Ok(false)) => {
            let e = WhisperModelError::Timeout;
            emit(host, "error", 0, 0, &e.to_string());
            return Err(e);
        }
        (Ok(Stop::Gone(_)), Err(e)) => {
            return Err(format!("prepare (ANE compile) wait: {e}").into());
        }
        (Ok(Stop::NotConfirmed), _) => {
            let e = WhisperModelError::Timeout;
            hand_to_reaper(key, child, UnreapedCause::ReapTimedOut, e.clone());
            emit(host, "error", 0, 0, &e.to_string());
            return Err(e);
        }
        (Err(e), _) => {
            let e = WhisperModelError::Failed(format!("prepare (ANE compile) stop: {e}"));
            hand_to_reaper(key, child, UnreapedCause::ReapFailed, e.clone());
            return Err(e);
        }
    };
    if !status.success() {
        let m = format!("model preparation failed ({status})");
        emit(host, "error", 0, 0, &m);
        return Err(m.into());
    }
    write_prepared_marker(host)?;
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn prepare_whisper(
    host: &dyn WhisperHost,
    _deadline: Deadline,
    _key: &'static str,
) -> Result<(), WhisperModelError> {
    write_prepared_marker(host)?;
    Ok(())
}

fn write_prepared_marker(host: &dyn WhisperHost) -> Result<(), String> {
    let marker = prepared_marker_now(host)?;
    rightkit_fs::atomic_write(
        &prepared_marker(),
        &serde_json::to_vec_pretty(&marker).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("write prepared marker: {e}"))?;
    Ok(())
}

fn emit(host: &dyn WhisperHost, phase: &str, received: u64, total: u64, message: &str) {
    host.emit_progress(phase, received, total, message);
}

/// True only if the Whisper model is fully USABLE — downloaded AND the one-time
/// ANE compile has completed. The UI gates language selection on this so a user
/// can't pick Whisper before it's ready to load instantly.
pub fn whisper_model_installed(host: &dyn WhisperHost) -> bool {
    installed() && prepared(host)
}

pub fn whisper_model_status(host: &dyn WhisperHost) -> WhisperModelStatus {
    let has_files = installed();
    WhisperModelStatus {
        installed: has_files,
        prepared: has_files && prepared(host),
    }
}

/// Longest one download may take on its own, whatever the caller has left.
const DOWNLOAD_CLIENT_BUDGET: Duration = Duration::from_secs(3600);

/// Download the Whisper archive and install it, ending by `deadline`. Called with the model's
/// operation gate held. Returns the bytes received and the archive total.
///
/// The deadline goes down as an absolute [`Instant`] ([`Deadline::instant`]) and no duration is
/// taken from it before a blocking request: the download is started with
/// [`crate::resumable_download::download_resumable_by`], and `rightkit_models::download`
/// derives from that instant, when each starts, the timeout of every HTTP request
/// (`Request::timeout`, which `rightkit-http` applies as the global timeout of the request,
/// connect to last body byte), the pause between attempts, each wait for body bytes (the idle
/// limit is capped at the time left) and the check before each 64 KiB chunk. The download
/// client's own `DOWNLOAD_CLIENT_BUDGET` of one hour is only a cap on one download. The
/// deadline is also the download's `cancelled` hook, polled before the first request and between
/// chunks. A deadline that passes mid-transfer returns [`WhisperModelError::Timeout`]; what is
/// left on disk is the `.part` and its resume record, which the next call resumes
/// (`rightkit_models::download::resume_state`) or discards when the server cannot prove
/// continuity (`cleanup_poisoned`), exactly as after any interrupted download.
///
/// Blocking steps and where each takes its time from: (1) `host.signed_entitlement_token()`,
/// a host call that cannot be interrupted: the deadline is read before it and again after it,
/// before any request; (2) `create_dir_all` and the partial-archive path: file system, not
/// interruptible, deadline read after; (3) the download's own checks, the verified-archive hash
/// (`verified_archive_length`, at most `max_bytes + 1` bytes), a completed `.part`'s hash and
/// commit and the seed copy of a resume: file system, not interruptible, bounded by size and not
/// measured, and counted against the same instant because the request budget is an instant;
/// (4) every HTTP request: `Request::timeout` from the deadline at the moment the request is
/// built (redirect hops inside `rightkit-http` subtract elapsed attempt time from that
/// timeout); (5) every body wait: `min(idle, time left)`
/// from the deadline when the wait starts; (6) the pauses between attempts (none with the
/// client's `retries: 0`): clipped to the time left; (7) the extraction and install: not
/// interruptible (at most [`crate::archive_install::MAX_ARCHIVE_ENTRIES`] members and
/// [`WHISPER_INSTALLED_MAX_BYTES`] bytes; `rightkit_models::archive_core` has no cancel hook),
/// checked before it starts and after it ends; a deadline passed by the extraction returns
/// `Timeout` once the install, which finishes atomically, is in place, and the next call
/// prepares it; (8) `host.emit_progress`, a host call. The hash of an archive already on disk
/// and the final fsync and rename are in (3).
fn fetch_and_install(
    host: &dyn WhisperHost,
    deadline: Deadline,
) -> Result<(u64, u64), WhisperModelError> {
    if !url_configured() {
        return Err(WhisperModelError::Failed(
            "Model download source is not configured yet.".into(),
        ));
    }
    let timed_out = |received: u64, total: u64| {
        let e = WhisperModelError::Timeout;
        emit(host, "error", received, total, &e.to_string());
        e
    };
    // A check, not a snapshot: no duration is taken here. The absolute deadline goes down to
    // the download and each blocking request there derives its timeout from it when it starts.
    if deadline.remaining().is_none() {
        return Err(timed_out(0, 0));
    }

    let models = models_dir();
    fs::create_dir_all(&models).map_err(|e| format!("create models dir: {e}"))?;
    let tmp = partial_archive_path(&models, whisper_dir_name(), WHISPER_ARCHIVE_SHA256)?;

    // --- download (streamed, with progress) ---
    // `DOWNLOAD_CLIENT_BUDGET` only caps one download at an hour from its first request; the
    // caller's deadline is passed as an instant below and is what normally ends it.
    let agent = crate::resumable_download::download_client(DOWNLOAD_CLIENT_BUDGET);
    // Authorize with the user's server-signed Pro entitlement token; the Worker
    // verifies its Ed25519 signature + `pro` tier + expiry. No shared secret.
    let pro_token = host.signed_entitlement_token().ok_or_else(|| {
        let m =
            "Your Pro licence needs a refresh before the model can download — reactivate it in Settings."
                .to_string();
        emit(host, "error", 0, 0, &m);
        m
    })?;
    let authorization = format!("Bearer {pro_token}");
    // The host's token call and the file-system work above cannot be interrupted and may have
    // used the time: the deadline is read again before the download starts.
    if deadline.remaining().is_none() {
        return Err(timed_out(0, 0));
    }
    let mut last_emit: u64 = 0;
    let download = crate::resumable_download::download_resumable_by(
        &agent,
        crate::resumable_download::DownloadRequest {
            url: WHISPER_ARCHIVE_URL,
            part_path: &tmp,
            expected_sha256: WHISPER_ARCHIVE_SHA256,
            authorization: Some(&authorization),
            max_bytes: WHISPER_ARCHIVE_MAX_BYTES,
        },
        |received, total| {
            if received < last_emit {
                last_emit = 0;
            }
            if received.saturating_sub(last_emit) > 4_000_000 {
                emit(host, "downloading", received, total, "Downloading model…");
                last_emit = received;
            }
        },
        &|| deadline.remaining().is_none(),
        deadline.instant(),
    )
    .map_err(|m| {
        // A failure seen at or after the deadline is the deadline's (the engine's budget and
        // cancel messages are not matched on text).
        if deadline.remaining().is_none() {
            timed_out(0, 0)
        } else {
            emit(host, "error", 0, 0, &m);
            WhisperModelError::Failed(m)
        }
    })?;
    let received = download.received;
    let total = download.total;
    // The archive is verified and stays on disk (a later call skips the network); the
    // extraction below cannot be interrupted, so it does not start without time.
    if deadline.remaining().is_none() {
        return Err(timed_out(received, total));
    }

    // --- extract (tar) ---
    emit(host, "extracting", received, total, "Installing model…");
    if let Err(err) = crate::archive_install::install_single_top_level_tar_gz(
        &download.archive_path,
        &models,
        whisper_dir_name(),
        &crate::archive_install::extract_limits(WHISPER_INSTALLED_MAX_BYTES),
    ) {
        emit(host, "error", received, total, &err);
        return Err(err.into());
    }
    crate::resumable_download::discard_partial(&tmp);
    if !installed() {
        let m = "Model archive did not contain the expected files.".to_string();
        emit(host, "error", received, total, &m);
        return Err(m.into());
    }
    // The install is complete and stays; a deadline the extraction ran past is reported.
    if deadline.remaining().is_none() {
        return Err(timed_out(received, total));
    }
    Ok((received, total))
}

/// Download + install the Whisper model from R2 (Pro). Idempotent: a no-op if
/// already present. Emits `whisper-model` progress events throughout. Bounded by
/// [`DOWNLOAD_CALL_BUDGET`]; see [`download_whisper_model_within`] for the typed error.
pub fn download_whisper_model_blocking(host: &dyn WhisperHost) -> Result<(), String> {
    download_whisper_model_within(host, DOWNLOAD_CALL_BUDGET).map_err(|e| e.to_string())
}

/// [`download_whisper_model_blocking`] with the caller's own total budget and a typed error.
///
/// The caller's entry instant is taken first of all, before anything can wait, and fixes both
/// the one absolute deadline and the instant a run must end at or after for this caller to
/// share its result. A zero or already spent budget returns [`WhisperModelError::Timeout`] at
/// once: before the operation gate, before the download, before a runner is chosen and before
/// the engine is spawned (each of those reads the deadline again, see [`lock_until`],
/// [`fetch_and_install`], [`claim_role`], `prepare_whisper`). Waiting for the operation gate,
/// the preparation's admission, the engine's run and its termination are each given only what
/// is left of the deadline; when it passes the caller returns [`WhisperModelError::Timeout`],
/// also in the middle of the download, which is stopped by it ([`fetch_and_install`] says which
/// steps can be interrupted and which are bounded by size only). The operation gate covers the
/// download and the install only; it is released before the one-time compile, which any number
/// of callers share as one preparation. A model whose earlier engine process has no group-empty
/// evidence yet, or whose spawn is outstanding, is refused with
/// [`WhisperModelError::PreparationUnreaped`] (`Unreaped` or `Spawning`, see the preparation
/// state) before an install can write into its directory ([`exclude_unreaped`]) and before a
/// preparation can start.
pub fn download_whisper_model_within(
    host: &dyn WhisperHost,
    budget: Duration,
) -> Result<(), WhisperModelError> {
    // The first statement: before the host is asked anything and before anything can wait.
    let entered = Instant::now();
    let deadline = Deadline::from_entry(entered, budget)?;
    if deadline.remaining().is_none() {
        return Err(WhisperModelError::Timeout);
    }
    if !host.has_pro() {
        return Err(WhisperModelError::Failed(
            "Locked-language transcription is a Pro feature.".into(),
        ));
    }
    let transferred = {
        let operation_lock = model_operation_lock(whisper_dir_name());
        let _operation_guard = lock_until(&operation_lock, deadline)?;
        if installed() && prepared(host) {
            emit(host, "installed", 1, 1, "Already installed.");
            return Ok(());
        }
        // Files already on disk (downloaded earlier) but the ANE cache was never
        // compiled: skip straight to the prepare step.
        if installed() {
            None
        } else {
            // The install writes into the model's directory: not while an engine process of
            // this model has no group-empty evidence yet, or its spawn is outstanding.
            exclude_unreaped(host, whisper_dir_name(), entered, deadline)?;
            Some(fetch_and_install(host, deadline)?)
        }
    };
    let (received, total) = transferred.unwrap_or((1, 1));
    // --- prepare (one-time ANE compile) — runs right after download ---
    prepare_single_flight(host, deadline, entered)?;
    emit(host, "installed", received, total, "Model ready.");
    Ok(())
}

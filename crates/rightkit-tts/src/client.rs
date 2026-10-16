//! Supervises the warm `rightkit-tts-worker` process and drives synthesis.
//!
//! One worker, one request at a time (the GPU is single). Cancelling kills the
//! worker; the next call respawns it (a cold load, a few seconds). The worker holds one engine per
//! model mode, loaded on first use and kept, so a call for another mode does not respawn it; it
//! respawns only when the client's configuration (executable, locations, runtime, idle window)
//! differs from the one it was started with.
use crate::chunk::{self, Chunk};
use crate::mode_location::{resolve_location, ModeLocation, VOCODER_FILE};
use crate::models;
use crate::protocol::{Mode, Reply, Request, REPLY_PREFIX};
use crate::wav;
use crate::worker_args::WorkerArgs;
use crate::SAMPLE_RATE_HZ;
use rightkit_child_process::{OwnedChild, OwnedCommand};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QtsError {
    pub code: &'static str,
    pub message: String,
}

/// Where the worker, its models and ONNX Runtime come from. Empty means the per-user defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Locations {
    data_root: Option<PathBuf>,
    ort_runtime: Option<PathBuf>,
    /// Exact worker executable; beats every environment variable and all discovery.
    worker_exe: Option<PathBuf>,
    /// Default model folder (Base, and every mode with no location of its own).
    model_dir: Option<PathBuf>,
    /// Default vocoder.
    vocoder: Option<PathBuf>,
    /// Per-mode locations; at most one per mode.
    modes: Vec<ModeLocation>,
    /// Explicit idle window; `None` leaves the worker on its environment default.
    idle_secs: Option<u64>,
}

impl Locations {
    /// Default folder and vocoder: the explicit setters, else `RIGHTKIT_TTS_MODEL_DIR` (legacy
    /// `GENRIGHT_QTS_MODEL_DIR`), else `<data>/models/qts`.
    fn defaults(&self) -> Result<(PathBuf, PathBuf), QtsError> {
        let dir = match &self.model_dir {
            Some(dir) => dir.clone(),
            None => models::model_dir_in(self.data_root.as_deref())
                .map_err(|e| QtsError::new("models_missing", e.to_string()))?,
        };
        let vocoder = self
            .vocoder
            .clone()
            .unwrap_or_else(|| dir.join(VOCODER_FILE));
        Ok((dir, vocoder))
    }

    /// Model folder and vocoder `mode` runs from.
    fn resolve(&self, mode: Mode) -> Result<(PathBuf, PathBuf), QtsError> {
        let (dir, vocoder) = self.defaults()?;
        Ok(resolve_location(&dir, &vocoder, &self.modes, mode))
    }

    /// Everything a worker is started with except its backend and starting mode. Two equal specs
    /// mean the running worker already has what a call needs; unequal specs respawn it.
    fn spawn_spec(&self) -> Result<SpawnSpec, QtsError> {
        let mut locations = Vec::new();
        for mode in [Mode::Base, Mode::CustomVoice, Mode::VoiceDesign] {
            let (model_dir, vocoder) = self.resolve(mode)?;
            locations.push(ModeLocation {
                mode,
                model_dir,
                vocoder: Some(vocoder),
            });
        }
        Ok(SpawnSpec {
            worker_exe: self.worker_exe.clone(),
            data_root: self.data_root.clone(),
            ort_runtime: self.ort_runtime.clone(),
            idle_secs: self.idle_secs,
            locations,
        })
    }
}

/// The client's configuration as a worker receives it. The executable and runtime are kept as
/// configured and resolved when a worker starts.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SpawnSpec {
    worker_exe: Option<PathBuf>,
    data_root: Option<PathBuf>,
    ort_runtime: Option<PathBuf>,
    idle_secs: Option<u64>,
    /// One resolved location per mode, vocoder always set, so the worker guesses nothing.
    locations: Vec<ModeLocation>,
}

impl SpawnSpec {
    fn location(&self, mode: Mode) -> &ModeLocation {
        self.locations
            .iter()
            .find(|l| l.mode == mode)
            .expect("a location for every mode")
    }

    /// The worker's command line: the starting `mode` and its folder, every mode's location, the
    /// resolved runtime and the idle window when one was set explicitly.
    fn worker_args(&self, backend: &str, mode: Mode, ort: Option<PathBuf>) -> WorkerArgs {
        let start = self.location(mode);
        WorkerArgs {
            model_dir: Some(start.model_dir.clone()),
            backend: Some(backend.to_string()),
            mode,
            vocoder: start.vocoder.clone(),
            data_root: self.data_root.clone(),
            ort_runtime: ort,
            mode_locations: self.locations.clone(),
            idle_secs: self.idle_secs,
        }
    }
}

/// Instance-facing QTS client. The worker remains shared process-wide so two
/// app calls cannot compete for one GPU; each client owns its idle policy,
/// its data root, model locations, executable and runtime choice, and drops its worker on
/// shutdown/drop.
#[derive(Debug, Clone)]
pub struct QtsClient {
    idle_secs: u64,
    locations: Locations,
}

impl Default for QtsClient {
    fn default() -> Self {
        Self::new(600)
    }
}

impl QtsClient {
    pub fn new(idle_secs: u64) -> Self {
        if std::env::var_os("RIGHTKIT_TTS_IDLE_SECS").is_none()
            && std::env::var_os("GENRIGHT_QTS_IDLE_SECS").is_none()
        {
            std::env::set_var("RIGHTKIT_TTS_IDLE_SECS", idle_secs.max(1).to_string());
        }
        Self {
            idle_secs: idle_secs.max(1),
            locations: Locations::default(),
        }
    }

    /// Resolve models, voices and the worker's runtime search under `root` (absolute) instead of
    /// the per-user data root. `RIGHTKIT_TTS_MODEL_DIR` still overrides the model folder.
    pub fn with_data_root(mut self, root: PathBuf) -> Self {
        self.locations.data_root = Some(root);
        self
    }

    /// Use this ONNX Runtime library for the worker in every build; the path must be a file when
    /// synthesis starts. Release builds never read `ORT_DYLIB_PATH`, with or without this.
    pub fn with_ort_runtime(mut self, path: PathBuf) -> Self {
        self.locations.ort_runtime = Some(path);
        self
    }

    /// Run exactly this worker executable. It beats `RIGHTKIT_TTS_WORKER`, `GENRIGHT_QTS_WORKER`
    /// and every discovery location, and is never replaced by one of them: when the file is
    /// absent, [`QtsClient::worker_exe`] and every call that would start a worker fail with
    /// `worker_missing`.
    pub fn with_worker_exe(mut self, path: PathBuf) -> Self {
        self.locations.worker_exe = Some(path);
        self
    }

    /// Default model folder for Base and for every mode without its own location. Beats
    /// `RIGHTKIT_TTS_MODEL_DIR` and the data root.
    pub fn with_model_dir(mut self, dir: PathBuf) -> Self {
        self.locations.model_dir = Some(dir);
        self
    }

    /// Default shared vocoder, for modes with no vocoder of their own. Without it the vocoder is
    /// `<model folder>/qwen3-tts-vocoder.onnx`.
    pub fn with_vocoder(mut self, path: PathBuf) -> Self {
        self.locations.vocoder = Some(path);
        self
    }

    /// Where one mode lives (for example the 1.7B exports apart from the 0.6B Base). Replaces an
    /// earlier location for the same mode. Every location reaches the worker before it reports
    /// ready, and the worker keeps one engine per mode.
    pub fn with_mode_location(mut self, location: ModeLocation) -> Self {
        self.locations.modes.retain(|l| l.mode != location.mode);
        self.locations.modes.push(location);
        self
    }

    /// Shorthand for [`QtsClient::with_mode_location`] that sets only the folder and keeps a
    /// vocoder already registered for the mode.
    pub fn with_mode_model_dir(mut self, mode: Mode, dir: PathBuf) -> Self {
        match self.locations.modes.iter_mut().find(|l| l.mode == mode) {
            Some(existing) => existing.model_dir = dir,
            None => self.locations.modes.push(ModeLocation::new(mode, dir)),
        }
        self
    }

    /// Idle window the worker is started with, in seconds. Always wins over
    /// `RIGHTKIT_TTS_IDLE_SECS` and `GENRIGHT_QTS_IDLE_SECS`. Passed through unchanged: `0` is the
    /// worker's zero-length window (it exits as soon as no request is waiting), as with the
    /// environment variables today. A client that never calls this leaves the environment in charge.
    pub fn with_idle_secs(mut self, secs: u64) -> Self {
        self.locations.idle_secs = Some(secs);
        self.idle_secs = secs;
        self
    }

    pub fn idle_secs(&self) -> u64 {
        self.idle_secs
    }

    /// The worker executable a launch from this client would use, resolved by the same function
    /// the launch uses, so a status check and a real start cannot disagree.
    pub fn worker_exe(&self) -> Result<PathBuf, QtsError> {
        resolve_worker_exe(
            self.locations.worker_exe.as_deref(),
            self.locations.data_root.as_deref(),
        )
    }

    /// Backend and vocoder labels reported by the live worker, started from this client's
    /// configuration if needed (see the free [`engine_info`]).
    pub fn engine_info(&self, backend: Option<&str>) -> Result<(String, String), QtsError> {
        engine_info_at(&self.locations, backend)
    }

    pub fn synthesize(&self, text: &str, options: &SynthOptions) -> Result<SynthResult, QtsError> {
        synthesize_at(&self.locations, text, options)
    }

    pub fn clone_from_wav(&self, wav_path: &Path, out: &Path) -> Result<PathBuf, QtsError> {
        clone_from_wav_at(&self.locations, wav_path, out)
    }

    /// Clone into this client's voice library folder (see [`clone_into_library`]).
    pub fn clone_into_library(&self, wav_path: &Path, voice_id: &str) -> Result<PathBuf, QtsError> {
        clone_into_library_at(&self.locations, wav_path, voice_id)
    }

    /// Resolve a voice record inside this client's voice library folder.
    pub fn resolve_voice_record(&self, path: &str) -> Result<PathBuf, QtsError> {
        resolve_voice_record_at(&self.locations, path)
    }

    pub fn worker_pid(&self) -> Option<u32> {
        worker_pid()
    }

    pub fn shutdown(&self) {
        shutdown();
    }
}

impl Drop for QtsClient {
    fn drop(&mut self) {
        shutdown();
    }
}

impl QtsError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for QtsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

/// Process-wide cancel flag for the synthesis in flight. The job runner calls
/// [`cancel_active`] from `job.cancel`; it takes effect between chunks and,
/// by killing the worker, mid-chunk.
static CANCEL: AtomicBool = AtomicBool::new(false);
static REQ_ID: AtomicU64 = AtomicU64::new(1);

pub fn cancel_active() {
    CANCEL.store(true, Ordering::SeqCst);
}

struct Worker {
    // OwnedChild kills the whole process tree on drop — that is the cancel
    // path for a mid-chunk synthesis.
    child: OwnedChild,
    stdin: ChildStdin,
    rx: Receiver<Reply>,
    ready: Option<Reply>,
    /// The configuration this worker was started with; a different one respawns it.
    spec: SpawnSpec,
}

static WORKER: OnceLock<Mutex<Option<Worker>>> = OnceLock::new();

fn slot() -> &'static Mutex<Option<Worker>> {
    WORKER.get_or_init(|| Mutex::new(None))
}

/// PID of the warm worker, if one is running (for diagnostics and benchmarks).
pub fn worker_pid() -> Option<u32> {
    slot()
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|w| w.child.id()))
}

/// Stop the warm worker (frees its GPU/RAM). Safe to call any time.
pub fn shutdown() {
    if let Ok(mut g) = slot().lock() {
        if let Some(w) = g.as_mut() {
            let _ = send(w, &Request::Quit);
        }
        *g = None;
    }
}

pub fn worker_exe() -> Result<PathBuf, QtsError> {
    resolve_worker_exe(None, None)
}

/// The one worker-executable resolver, shared by [`worker_exe`], [`QtsClient::worker_exe`] and the
/// launch. An `explicit` path is exact: it beats every environment variable and all discovery, and
/// when the file is absent the answer is `worker_missing`, never a fall back to discovery.
fn resolve_worker_exe(
    explicit: Option<&Path>,
    data_root: Option<&Path>,
) -> Result<PathBuf, QtsError> {
    if let Some(path) = explicit {
        return if path.is_file() {
            Ok(path.to_path_buf())
        } else {
            Err(QtsError::new(
                "worker_missing",
                format!("worker {} not found", path.display()),
            ))
        };
    }
    // The shipped binary is `rightkit-tts-worker`; the pre-rename name stays a fallback for old bundles.
    let names: [&str; 2] = if cfg!(windows) {
        ["rightkit-tts-worker.exe", "genright-qts-worker.exe"]
    } else {
        ["rightkit-tts-worker", "genright-qts-worker"]
    };
    let name = names[0];
    if let Some(p) = std::env::var_os("RIGHTKIT_TTS_WORKER")
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var_os("GENRIGHT_QTS_WORKER").filter(|v| !v.is_empty()))
    {
        let p = PathBuf::from(p);
        return if p.is_file() {
            Ok(p)
        } else {
            Err(QtsError::new(
                "worker_missing",
                format!("worker override {} not found", p.display()),
            ))
        };
    }
    let mut dirs = Vec::new();
    if let Some(d) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
    {
        dirs.push(d.clone());
        // `cargo test` binaries live in target/<profile>/deps.
        if let Some(p) = d.parent() {
            dirs.push(p.to_path_buf());
        }
    }
    if let Ok(root) = models::data_root_in(data_root) {
        dirs.push(root.join("runtime").join("qts"));
    }
    dirs.into_iter()
        .flat_map(|d| names.iter().map(move |n| d.join(n)))
        .find(|p| p.is_file())
        .ok_or_else(|| {
            QtsError::new(
                "worker_missing",
                format!("{name} not found next to the app or in <data>/runtime/qts"),
            )
        })
}

fn send(w: &mut Worker, req: &Request) -> std::io::Result<()> {
    let mut line = serde_json::to_string(req).map_err(std::io::Error::other)?;
    line.push('\n');
    w.stdin.write_all(line.as_bytes())?;
    w.stdin.flush()
}

/// The ONNX Runtime a worker should load. A caller-supplied path must exist; otherwise the
/// installed-runtime search applies (see `models::find_onnxruntime_in`), anchored on the Base
/// model folder.
fn ort_runtime_for(spec: &SpawnSpec) -> Result<Option<PathBuf>, QtsError> {
    match &spec.ort_runtime {
        Some(path) if path.is_file() => Ok(Some(path.clone())),
        Some(path) => Err(QtsError::new(
            "runtime_missing",
            format!("ONNX Runtime {} not found", path.display()),
        )),
        None => Ok(models::find_onnxruntime_in(
            spec.data_root.as_deref(),
            Some(&spec.location(Mode::Base).model_dir),
        )),
    }
}

fn spawn(spec: &SpawnSpec, backend: &str, mode: Mode) -> Result<Worker, QtsError> {
    let exe = resolve_worker_exe(spec.worker_exe.as_deref(), spec.data_root.as_deref())?;
    let ort = ort_runtime_for(spec)?;
    let mut cmd = Command::new(exe);
    cmd.args(spec.worker_args(backend, mode, ort).to_command_line());
    if !cfg!(feature = "dev-overrides") {
        // Without `dev-overrides`: an inherited ORT_DYLIB_PATH never reaches the worker.
        cmd.env_remove("ORT_DYLIB_PATH");
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut owned = OwnedCommand::from_command(cmd);
    owned.windows_hide(); // CREATE_NO_WINDOW on Windows
    let mut child = owned
        .spawn()
        .map_err(|e| QtsError::new("worker_spawn", format!("cannot start the QTS worker: {e}")))?;
    let stdin = child
        .take_stdin()
        .ok_or_else(|| QtsError::new("worker_spawn", "no worker stdin"))?;
    let stdout = child
        .take_stdout()
        .ok_or_else(|| QtsError::new("worker_spawn", "no worker stdout"))?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some(json) = line.strip_prefix(REPLY_PREFIX) {
                if let Ok(r) = serde_json::from_str::<Reply>(json) {
                    if tx.send(r).is_err() {
                        break;
                    }
                }
            }
        }
    });
    Ok(Worker {
        child,
        stdin,
        rx,
        ready: None,
        spec: spec.clone(),
    })
}

/// Wait for the next reply, polling the cancel flag and worker liveness.
fn wait_reply(w: &mut Worker, limit: Duration) -> Result<Reply, QtsError> {
    let start = Instant::now();
    loop {
        if CANCEL.load(Ordering::SeqCst) {
            return Err(QtsError::new("cancelled", "cancelled"));
        }
        match w.rx.recv_timeout(Duration::from_millis(50)) {
            Ok(r) => return Ok(r),
            Err(RecvTimeoutError::Disconnected) => {
                return Err(QtsError::new(
                    "worker_died",
                    "the QTS worker exited unexpectedly (see its stderr)",
                ))
            }
            Err(RecvTimeoutError::Timeout) => {
                if start.elapsed() > limit {
                    return Err(QtsError::new(
                        "timeout",
                        format!("QTS worker did not reply within {}s", limit.as_secs()),
                    ));
                }
            }
        }
    }
}

/// A worker reports `model_missing: <mode>: ...` when a model file is absent, at start or when a
/// later request loads another mode. That is `models_missing`, the class a pre-spawn check
/// reports, not `default`.
fn reply_error_code(message: &str, default: &'static str) -> &'static str {
    if message.starts_with("model_missing:") {
        "models_missing"
    } else {
        default
    }
}

/// Pre-spawn check that `mode`'s files exist (`models_missing`) and pass the manifest
/// (`models_invalid`).
fn check_mode_files(dir: &Path, vocoder: &Path, mode: Mode) -> Result<(), QtsError> {
    let missing = models::missing_for_mode(dir, vocoder, mode);
    if !missing.is_empty() {
        let list: Vec<String> = missing.iter().map(|p| p.display().to_string()).collect();
        return Err(QtsError::new(
            "models_missing",
            format!(
                "QTS {} model files not found in {}: {}",
                mode.as_str(),
                dir.display(),
                list.join(", ")
            ),
        ));
    }
    if mode == Mode::Base {
        models::verify_base(dir, vocoder, false)
    } else {
        models::verify_mode(dir, mode, false)
    }
    .map_err(|m| QtsError::new("models_invalid", m))
}

/// Run `f` with a ready worker for `mode`, (re)spawning as needed. A failed or cancelled call
/// discards the worker so the next call starts clean.
fn with_worker<T>(
    loc: &Locations,
    backend: &str,
    mode: Mode,
    f: impl FnOnce(&mut Worker) -> Result<T, QtsError>,
) -> Result<T, QtsError> {
    let (dir, vocoder) = loc.resolve(mode)?;
    check_mode_files(&dir, &vocoder, mode)?;
    let spec = loc.spawn_spec()?;
    let mut guard = slot().lock().unwrap_or_else(|p| p.into_inner());
    if let Some(w) = guard.as_mut() {
        if matches!(w.child.try_wait(), Ok(Some(_))) {
            *guard = None;
        }
    }
    if guard.as_ref().is_some_and(|w| w.spec != spec) {
        if let Some(w) = guard.as_mut() {
            let _ = send(w, &Request::Quit);
        }
        *guard = None;
    }
    if guard.is_none() {
        let mut w = spawn(&spec, backend, mode)?;
        match wait_reply(&mut w, Duration::from_secs(180))? {
            r @ Reply::Ready { .. } => w.ready = Some(r),
            Reply::Error { message, .. } => {
                return Err(QtsError::new(
                    reply_error_code(&message, "engine_load"),
                    message,
                ))
            }
            other => {
                return Err(QtsError::new(
                    "protocol",
                    format!("unexpected first reply {other:?}"),
                ))
            }
        }
        *guard = Some(w);
    }
    let result = f(guard.as_mut().expect("worker present"));
    if result.is_err() {
        *guard = None; // kills the process: also how cancel interrupts a running chunk
    }
    result
}

/// Backend/vocoder labels reported by the live worker (loads it if needed), with the per-user
/// defaults. [`QtsClient::engine_info`] uses a client's own configuration.
pub fn engine_info(backend: Option<&str>) -> Result<(String, String), QtsError> {
    engine_info_at(&Locations::default(), backend)
}

fn engine_info_at(loc: &Locations, backend: Option<&str>) -> Result<(String, String), QtsError> {
    with_worker(
        loc,
        backend.unwrap_or(models::default_backend()),
        Mode::Base,
        |w| match &w.ready {
            Some(Reply::Ready {
                backend, vocoder, ..
            }) => Ok((backend.clone(), vocoder.clone())),
            _ => Err(QtsError::new("protocol", "worker not ready")),
        },
    )
}

#[derive(Debug, Clone)]
pub struct SynthOptions {
    /// Path of an x-vector voice prompt (`.pb`) from [`clone_from_wav`]. Base mode only.
    pub voice_prompt: Option<PathBuf>,
    /// Codec language id; see [`language_id`].
    pub language_id: i32,
    pub temperature: f32,
    pub top_p: f32,
    pub top_k: i32,
    pub max_chunk_chars: usize,
    /// `metal`, `vulkan`, `cpu`, or `auto` (default for the platform).
    pub backend: Option<String>,
    /// Model mode. `Base` (default) is the 0.6B voice-clone path; `CustomVoice` and `VoiceDesign`
    /// need the 1.7B exports. See [`SynthOptions::validate`].
    pub mode: Mode,
    /// Preset speaker name for `CustomVoice` (e.g. `"vivian"`).
    pub speaker: Option<String>,
    /// Style instruction: required for `VoiceDesign`, optional for `CustomVoice`.
    pub instruct: Option<String>,
    /// Shorten every interior silent run longer than this many milliseconds to exactly this long
    /// (edge silence is trimmed separately). `None` keeps the model's own pauses.
    pub max_interior_silence_ms: Option<usize>,
}

impl Default for SynthOptions {
    fn default() -> Self {
        Self {
            voice_prompt: None,
            language_id: 2050,
            temperature: 0.9,
            top_p: 1.0,
            top_k: 50,
            max_chunk_chars: chunk::DEFAULT_MAX_CHARS,
            backend: None,
            mode: Mode::Base,
            speaker: None,
            instruct: None,
            max_interior_silence_ms: None,
        }
    }
}

impl SynthOptions {
    /// Check the options against their mode before any worker starts.
    ///
    /// - `Base`: no speaker and no instruct; a voice prompt is optional.
    /// - `CustomVoice`: a speaker is required; no voice prompt.
    /// - `VoiceDesign`: a non-empty instruct is required; no speaker, no voice prompt.
    ///
    /// Speaker names are checked against [`crate::PRESET_SPEAKERS`] in every build.
    pub fn validate(&self) -> Result<(), QtsError> {
        let invalid = |message: &str| Err(QtsError::new("invalid_request", message));
        match self.mode {
            Mode::Base => {
                if self.speaker.is_some() || self.instruct.is_some() {
                    return invalid("base mode does not accept a speaker or instruct");
                }
            }
            Mode::CustomVoice => {
                let Some(speaker) = self.speaker.as_deref() else {
                    return invalid("custom_voice needs a preset speaker");
                };
                check_speaker(speaker)?;
                if self.voice_prompt.is_some() {
                    return invalid("custom_voice does not accept a voice prompt");
                }
            }
            Mode::VoiceDesign => {
                if self
                    .instruct
                    .as_deref()
                    .is_none_or(|text| text.trim().is_empty())
                {
                    return invalid("voice_design needs a non-empty instruct");
                }
                if self.speaker.is_some() {
                    return invalid("voice_design does not accept a speaker");
                }
                if self.voice_prompt.is_some() {
                    return invalid("voice_design does not accept a voice prompt");
                }
            }
        }
        Ok(())
    }
}

fn check_speaker(name: &str) -> Result<(), QtsError> {
    if name.trim().is_empty() {
        return Err(QtsError::new("invalid_request", "speaker name is empty"));
    }
    if !crate::protocol::is_preset_speaker(name) {
        return Err(QtsError::new(
            "invalid_request",
            format!("unknown preset speaker {name}"),
        ));
    }
    Ok(())
}

/// Qwen3-TTS codec language ids.
pub fn language_id(code: &str) -> Option<i32> {
    Some(match code.trim().to_ascii_lowercase().as_str() {
        "en" | "english" => 2050,
        "de" | "german" => 2053,
        "es" | "spanish" => 2054,
        "zh" | "chinese" => 2055,
        "ja" | "japanese" => 2058,
        "fr" | "french" => 2061,
        "ko" | "korean" => 2064,
        "ru" | "russian" => 2069,
        "it" | "italian" => 2070,
        "pt" | "portuguese" => 2071,
        _ => return None,
    })
}

#[derive(Debug, Clone)]
pub struct SynthResult {
    /// Complete mono PCM16 WAV at 24 kHz.
    pub wav: Vec<u8>,
    pub audio_seconds: f64,
    pub wall_seconds: f64,
    /// Time spent inside the engine only (excludes worker cold load).
    pub gen_seconds: f64,
    pub chunks: usize,
    pub backend: String,
    pub vocoder: String,
}

impl SynthResult {
    /// Real-time factor of generation: engine seconds per audio second.
    pub fn rtf(&self) -> f64 {
        self.gen_seconds / self.audio_seconds.max(1e-9)
    }
}

/// Synthesize `text` (any length, chunked). Clears the cancel flag on entry.
pub fn synthesize(text: &str, opts: &SynthOptions) -> Result<SynthResult, QtsError> {
    synthesize_at(&Locations::default(), text, opts)
}

fn synthesize_at(
    loc: &Locations,
    text: &str,
    opts: &SynthOptions,
) -> Result<SynthResult, QtsError> {
    if text.trim().is_empty() {
        return Err(QtsError::new("invalid_request", "text is empty"));
    }
    opts.validate()?;
    let chunks: Vec<Chunk> = chunk::split_text(text, opts.max_chunk_chars);
    if chunks.is_empty() {
        return Err(QtsError::new(
            "invalid_request",
            "text has no speakable content",
        ));
    }
    if let Some(vp) = &opts.voice_prompt {
        if !vp.is_file() {
            return Err(QtsError::new(
                "voice_missing",
                format!("voice prompt {} not found", vp.display()),
            ));
        }
    }
    CANCEL.store(false, Ordering::SeqCst);
    let started = Instant::now();
    let tmp = std::env::temp_dir().join(format!(
        "genright-qts-{}-{}",
        std::process::id(),
        REQ_ID.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&tmp).map_err(|e| QtsError::new("io", e.to_string()))?;
    let backend = opts
        .backend
        .clone()
        .unwrap_or_else(|| models::default_backend().to_string());
    let out = with_worker(loc, &backend, opts.mode, |w| {
        let (be, voc) = match &w.ready {
            Some(Reply::Ready {
                backend, vocoder, ..
            }) => (backend.clone(), vocoder.clone()),
            _ => (String::new(), String::new()),
        };
        let mut pcm: Vec<i16> = Vec::new();
        let mut gen_ms = 0u64;
        for (i, c) in chunks.iter().enumerate() {
            if CANCEL.load(Ordering::SeqCst) {
                return Err(QtsError::new("cancelled", "cancelled"));
            }
            let id = REQ_ID.fetch_add(1, Ordering::SeqCst);
            let out_pcm = tmp.join(format!("chunk-{i}.pcm"));
            send(
                w,
                &Request::Synth {
                    id,
                    text: c.text.clone(),
                    voice_prompt: opts.voice_prompt.as_ref().map(|p| p.display().to_string()),
                    mode: Some(opts.mode),
                    instruct: opts.instruct.clone(),
                    speaker: opts.speaker.clone(),
                    language_id: opts.language_id,
                    max_frames: chunk::frame_budget(c.text.chars().count()),
                    temperature: opts.temperature,
                    top_p: opts.top_p,
                    top_k: opts.top_k,
                    out_pcm: out_pcm.display().to_string(),
                },
            )
            .map_err(|e| {
                QtsError::new("worker_died", format!("cannot reach the QTS worker: {e}"))
            })?;
            match wait_reply(w, Duration::from_secs(600))? {
                Reply::Done {
                    id: rid,
                    samples,
                    gen_ms: g,
                    ..
                } if rid == id => {
                    gen_ms += g;
                    let raw =
                        std::fs::read(&out_pcm).map_err(|e| QtsError::new("io", e.to_string()))?;
                    let _ = std::fs::remove_file(&out_pcm);
                    if raw.len() != samples * 2 {
                        return Err(QtsError::new("protocol", "chunk size mismatch"));
                    }
                    let mut cp: Vec<i16> = raw
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|b| i16::from_le_bytes([b[0], b[1]]))
                        .collect();
                    trim_edge_silence(&mut cp);
                    if let Some(ms) = opts.max_interior_silence_ms {
                        cap_interior_silence(&mut cp, interior_cap_samples(ms));
                    }
                    pcm.extend_from_slice(&cp);
                    pcm.extend(wav::silence(c.pause_ms));
                }
                Reply::Error { message, .. } => {
                    return Err(QtsError::new(
                        reply_error_code(&message, "synthesis"),
                        message,
                    ))
                }
                other => {
                    return Err(QtsError::new(
                        "protocol",
                        format!("unexpected reply {other:?}"),
                    ))
                }
            }
        }
        Ok((pcm, gen_ms, be, voc))
    });
    let _ = std::fs::remove_dir(&tmp);
    let (pcm, gen_ms, backend, vocoder) = out?;
    Ok(SynthResult {
        audio_seconds: pcm.len() as f64 / SAMPLE_RATE_HZ as f64,
        wav: wav::encode_pcm16(&pcm, SAMPLE_RATE_HZ),
        wall_seconds: started.elapsed().as_secs_f64(),
        gen_seconds: gen_ms as f64 / 1000.0,
        chunks: chunks.len(),
        backend,
        vocoder,
    })
}

/// Peak magnitude at or below which a PCM16 sample counts as silence (about 1% of full scale).
const SILENCE_FLOOR: i16 = 327;

/// Remove leading/trailing silence a chunk emits around its speech. The model
/// pads each utterance with dead air (observed up to ~2 s), which stacked with
/// the explicit join pause produced gaps well over 300 ms. Trims to a fixed
/// noise floor, keeping a short pad so joins are not abrupt.
fn trim_edge_silence(pcm: &mut Vec<i16>) {
    let pad = (crate::SAMPLE_RATE_HZ as usize) * 10 / 1000; // 10 ms kept edge pad
    let first = pcm
        .iter()
        .position(|s| s.abs() > SILENCE_FLOOR)
        .unwrap_or(pcm.len());
    let last = pcm
        .iter()
        .rposition(|s| s.abs() > SILENCE_FLOOR)
        .map(|i| i + 1)
        .unwrap_or(0);
    if first >= last {
        pcm.clear();
        return;
    }
    let start = first.saturating_sub(pad);
    let end = (last + pad).min(pcm.len());
    pcm.drain(end..);
    pcm.drain(..start);
    // Short fades kill hard onsets/offsets at the trimmed edges (join clicks).
    let fade = (crate::SAMPLE_RATE_HZ as usize / 1000 * 8).min(pcm.len() / 4);
    for i in 0..fade {
        let g = i as f32 / fade as f32;
        pcm[i] = (pcm[i] as f32 * g) as i16;
        let j = pcm.len() - 1 - i;
        pcm[j] = (pcm[j] as f32 * g) as i16;
    }
}

/// Samples in `ms` of mono audio at the output rate.
fn interior_cap_samples(ms: usize) -> usize {
    ms.saturating_mul(SAMPLE_RATE_HZ as usize) / 1000
}

/// Shorten every interior silent run longer than `max` samples to exactly `max`, keeping its first
/// `max / 2` and last `max - max / 2` samples. A run touching either end of the chunk is edge
/// silence and is left alone. Runs of `max` or fewer samples are untouched.
fn cap_interior_silence(pcm: &mut Vec<i16>, max: usize) {
    let n = pcm.len();
    let mut out = Vec::with_capacity(n);
    let mut i = 0;
    while i < n {
        if pcm[i].abs() > SILENCE_FLOOR {
            out.push(pcm[i]);
            i += 1;
            continue;
        }
        let start = i;
        while i < n && pcm[i].abs() <= SILENCE_FLOOR {
            i += 1;
        }
        let end = i;
        let interior = start > 0 && end < n;
        if interior && end - start > max {
            let head = max / 2;
            let tail = max - head;
            out.extend_from_slice(&pcm[start..start + head]);
            out.extend_from_slice(&pcm[end - tail..end]);
        } else {
            out.extend_from_slice(&pcm[start..end]);
        }
    }
    *pcm = out;
}

/// Instant x-vector cloning: encode a short reference WAV (3 to 30 s of clean
/// speech works best) into a voice-clone prompt file at `out`. Returns `out`.
pub fn clone_from_wav(wav_path: &Path, out: &Path) -> Result<PathBuf, QtsError> {
    clone_from_wav_at(&Locations::default(), wav_path, out)
}

fn clone_from_wav_at(loc: &Locations, wav_path: &Path, out: &Path) -> Result<PathBuf, QtsError> {
    if !wav_path.is_file() {
        return Err(QtsError::new(
            "invalid_request",
            format!("reference audio {} not found", wav_path.display()),
        ));
    }
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| QtsError::new("io", e.to_string()))?;
    }
    CANCEL.store(false, Ordering::SeqCst);
    let id = REQ_ID.fetch_add(1, Ordering::SeqCst);
    with_worker(loc, models::default_backend(), Mode::Base, |w| {
        send(
            w,
            &Request::Clone {
                id,
                wav: wav_path.display().to_string(),
                out: out.display().to_string(),
            },
        )
        .map_err(|e| QtsError::new("worker_died", e.to_string()))?;
        match wait_reply(w, Duration::from_secs(120))? {
            Reply::Done { .. } => Ok(()),
            Reply::Error { message, .. } => {
                Err(QtsError::new(reply_error_code(&message, "clone"), message))
            }
            other => Err(QtsError::new(
                "protocol",
                format!("unexpected reply {other:?}"),
            )),
        }
    })?;
    Ok(out.to_path_buf())
}

/// Clone into the voice library folder: writes `<data>/voices/local/<id>.xvector.pb`.
pub fn clone_into_library(wav_path: &Path, voice_id: &str) -> Result<PathBuf, QtsError> {
    clone_into_library_at(&Locations::default(), wav_path, voice_id)
}

fn clone_into_library_at(
    loc: &Locations,
    wav_path: &Path,
    voice_id: &str,
) -> Result<PathBuf, QtsError> {
    if voice_id.is_empty()
        || !voice_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(QtsError::new(
            "invalid_request",
            "voice id must be [A-Za-z0-9_-]+",
        ));
    }
    let voices = models::try_voices_dir_in(loc.data_root.as_deref())
        .map_err(|e| QtsError::new("voice_missing", e.to_string()))?;
    clone_from_wav_at(
        loc,
        wav_path,
        &voices.join(format!("{voice_id}.xvector.pb")),
    )
}

/// A voice record path is acceptable only inside the voices folder (the
/// provider param must not read arbitrary files). Returns the canonical path.
pub fn resolve_voice_record(path: &str) -> Result<PathBuf, QtsError> {
    resolve_voice_record_at(&Locations::default(), path)
}

fn resolve_voice_record_at(loc: &Locations, path: &str) -> Result<PathBuf, QtsError> {
    let voices = models::try_voices_dir_in(loc.data_root.as_deref())
        .map_err(|e| QtsError::new("voice_missing", e.to_string()))?;
    resolve_voice_record_in(&voices, path)
}

pub fn resolve_voice_record_in(root: &Path, path: &str) -> Result<PathBuf, QtsError> {
    let root = root.canonicalize().map_err(|_| {
        QtsError::new(
            "voice_missing",
            format!("voices folder {} does not exist", root.display()),
        )
    })?;
    let p = PathBuf::from(path);
    let p = if p.is_absolute() { p } else { root.join(p) };
    let p = p
        .canonicalize()
        .map_err(|_| QtsError::new("voice_missing", format!("voice record {path} not found")))?;
    if !p.starts_with(&root) {
        return Err(QtsError::new(
            "invalid_request",
            "voice_prompt must be inside <data>/voices/local/",
        ));
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_record_must_live_in_voices_folder() {
        let root = std::env::temp_dir().join(format!("gqts-voices-{}", std::process::id()));
        let voices = root.join("voices").join("local");
        std::fs::create_dir_all(&voices).unwrap();
        let outside = root.join("outside.pb");
        std::fs::write(&outside, b"x").unwrap();
        std::fs::write(voices.join("a.xvector.pb"), b"x").unwrap();
        assert!(resolve_voice_record_in(&voices, "a.xvector.pb").is_ok());
        assert_eq!(
            resolve_voice_record_in(&voices, outside.to_str().unwrap())
                .unwrap_err()
                .code,
            "invalid_request"
        );
        assert_eq!(
            resolve_voice_record_in(&voices, "../../outside.pb")
                .unwrap_err()
                .code,
            "invalid_request"
        );
        assert_eq!(
            resolve_voice_record_in(&voices, "missing.pb")
                .unwrap_err()
                .code,
            "voice_missing"
        );
    }

    #[test]
    fn languages() {
        assert_eq!(language_id("EN"), Some(2050));
        assert_eq!(language_id("xx"), None);
    }

    #[test]
    fn edge_silence_is_trimmed_to_a_short_pad() {
        let sr = crate::SAMPLE_RATE_HZ as usize;
        let mut pcm = vec![0i16; sr]; // 1 s of dead air
        pcm.extend(vec![1000i16; sr / 2]); // 0.5 s of speech
        pcm.extend(vec![0i16; sr]); // 1 s more dead air
        trim_edge_silence(&mut pcm);
        let keep = sr * 10 / 1000;
        assert_eq!(pcm.len(), sr / 2 + 2 * keep);
        assert!(pcm.iter().all(|s| s.abs() <= 1000));
        let mut silent = vec![0i16; 100];
        trim_edge_silence(&mut silent);
        assert!(silent.is_empty());
    }

    /// Speech samples used around silent runs; loud enough to never count as silence.
    fn speech(n: usize) -> Vec<i16> {
        vec![1000i16; n]
    }

    #[test]
    fn interior_silence_cap_converts_milliseconds_at_the_output_rate() {
        assert_eq!(interior_cap_samples(450), 10_800);
        assert_eq!(interior_cap_samples(0), 0);
    }

    #[test]
    fn interior_run_longer_than_max_keeps_first_and_last_halves() {
        // 2000 silent samples, each distinct but below the floor, so the kept halves are checkable.
        let run: Vec<i16> = (0..2000).map(|i| (i % 300) as i16).collect();
        let mut pcm = speech(100);
        pcm.extend(&run);
        pcm.extend(speech(100));
        cap_interior_silence(&mut pcm, 450);
        let mut expect = speech(100);
        expect.extend(&run[..225]);
        expect.extend(&run[2000 - 225..]);
        expect.extend(speech(100));
        assert_eq!(pcm, expect);
        assert_eq!(pcm.len(), 100 + 450 + 100);
    }

    #[test]
    fn odd_max_gives_the_extra_sample_to_the_tail() {
        let run: Vec<i16> = (0..9).map(|i| i as i16).collect();
        let mut pcm = speech(10);
        pcm.extend(&run);
        pcm.extend(speech(10));
        cap_interior_silence(&mut pcm, 5);
        // head = 2, tail = 3
        let mut expect = speech(10);
        expect.extend(&run[..2]);
        expect.extend(&run[6..]);
        expect.extend(speech(10));
        assert_eq!(pcm, expect);
    }

    #[test]
    fn interior_runs_at_or_below_max_are_untouched() {
        let mut pcm = speech(10);
        pcm.extend(vec![0i16; 450]); // exactly max
        pcm.extend(speech(10));
        pcm.extend(vec![200i16; 300]); // shorter than max, below the floor
        pcm.extend(speech(10));
        let before = pcm.clone();
        cap_interior_silence(&mut pcm, 450);
        assert_eq!(pcm, before);
    }

    #[test]
    fn edge_silence_is_not_capped_by_the_interior_rule() {
        let mut pcm = vec![0i16; 2000];
        pcm.extend(speech(50));
        pcm.extend(vec![0i16; 2000]);
        let before = pcm.clone();
        cap_interior_silence(&mut pcm, 450);
        assert_eq!(pcm, before);
        let mut all_silent = vec![0i16; 2000];
        cap_interior_silence(&mut all_silent, 450);
        assert_eq!(all_silent.len(), 2000);
    }

    fn base() -> SynthOptions {
        SynthOptions::default()
    }

    #[test]
    fn validate_accepts_each_mode_with_its_inputs() {
        assert!(base().validate().is_ok());
        let custom = SynthOptions {
            mode: Mode::CustomVoice,
            speaker: Some("vivian".into()),
            ..base()
        };
        assert!(custom.validate().is_ok());
        let design = SynthOptions {
            mode: Mode::VoiceDesign,
            instruct: Some("a calm narrator".into()),
            ..base()
        };
        assert!(design.validate().is_ok());
    }

    #[test]
    fn validate_rejects_inputs_that_belong_to_another_mode() {
        let cases = [
            SynthOptions {
                speaker: Some("vivian".into()),
                ..base()
            },
            SynthOptions {
                instruct: Some("hi".into()),
                ..base()
            },
            SynthOptions {
                mode: Mode::CustomVoice,
                ..base()
            },
            SynthOptions {
                mode: Mode::CustomVoice,
                speaker: Some("   ".into()),
                ..base()
            },
            SynthOptions {
                mode: Mode::CustomVoice,
                speaker: Some("vivian".into()),
                voice_prompt: Some(PathBuf::from("/x.pb")),
                ..base()
            },
            SynthOptions {
                mode: Mode::VoiceDesign,
                ..base()
            },
            SynthOptions {
                mode: Mode::VoiceDesign,
                instruct: Some("   ".into()),
                ..base()
            },
            SynthOptions {
                mode: Mode::VoiceDesign,
                instruct: Some("hi".into()),
                speaker: Some("vivian".into()),
                ..base()
            },
            SynthOptions {
                mode: Mode::VoiceDesign,
                instruct: Some("hi".into()),
                voice_prompt: Some(PathBuf::from("/x.pb")),
                ..base()
            },
        ];
        for options in cases {
            let err = options.validate().unwrap_err();
            assert_eq!(err.code, "invalid_request", "{options:?}");
        }
    }

    #[test]
    fn validate_checks_speaker_names_against_the_preset_list() {
        let unknown = SynthOptions {
            mode: Mode::CustomVoice,
            speaker: Some("nobody".into()),
            ..base()
        };
        assert_eq!(unknown.validate().unwrap_err().code, "invalid_request");
    }
}

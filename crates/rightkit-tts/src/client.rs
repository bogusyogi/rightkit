//! Supervises the warm `genright-qts-worker` process and drives synthesis.
//!
//! One worker, one request at a time (the GPU is single). Cancelling kills the
//! worker; the next call respawns it (a cold load, a few seconds).
use crate::chunk::{self, Chunk};
use crate::models;
use crate::protocol::{Reply, Request, REPLY_PREFIX};
use crate::wav;
use crate::SAMPLE_RATE_HZ;
use rightkit_process::{OwnedChild, OwnedCommand};
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

/// Instance-facing QTS client. The worker remains shared process-wide so two
/// app calls cannot compete for one GPU; each client owns its idle policy and
/// drops its worker on shutdown/drop.
#[derive(Debug, Clone)]
pub struct QtsClient {
    idle_secs: u64,
}

impl Default for QtsClient {
    fn default() -> Self { Self::new(600) }
}

impl QtsClient {
    pub fn new(idle_secs: u64) -> Self {
        if std::env::var_os("RIGHTKIT_TTS_IDLE_SECS").is_none() && std::env::var_os("GENRIGHT_QTS_IDLE_SECS").is_none() {
            std::env::set_var("RIGHTKIT_TTS_IDLE_SECS", idle_secs.max(1).to_string());
        }
        Self { idle_secs: idle_secs.max(1) }
    }

    pub fn idle_secs(&self) -> u64 { self.idle_secs }

    pub fn synthesize(&self, text: &str, options: &SynthOptions) -> Result<SynthResult, QtsError> {
        synthesize(text, options)
    }

    pub fn clone_from_wav(&self, wav_path: &Path, out: &Path) -> Result<PathBuf, QtsError> {
        clone_from_wav(wav_path, out)
    }

    pub fn worker_pid(&self) -> Option<u32> { worker_pid() }

    pub fn shutdown(&self) { shutdown(); }
}

impl Drop for QtsClient {
    fn drop(&mut self) { shutdown(); }
}

impl QtsError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
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
}

static WORKER: OnceLock<Mutex<Option<Worker>>> = OnceLock::new();

fn slot() -> &'static Mutex<Option<Worker>> {
    WORKER.get_or_init(|| Mutex::new(None))
}

/// PID of the warm worker, if one is running (for diagnostics and benchmarks).
pub fn worker_pid() -> Option<u32> {
    slot().lock().ok().and_then(|g| g.as_ref().map(|w| w.child.id()))
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
    let name = if cfg!(windows) { "genright-qts-worker.exe" } else { "genright-qts-worker" };
    if let Some(p) = std::env::var_os("RIGHTKIT_TTS_WORKER").filter(|v| !v.is_empty()).or_else(|| std::env::var_os("GENRIGHT_QTS_WORKER").filter(|v| !v.is_empty())) {
        let p = PathBuf::from(p);
        return if p.is_file() { Ok(p) } else { Err(QtsError::new("worker_missing", format!("GENRIGHT_QTS_WORKER={} not found", p.display()))) };
    }
    let mut dirs = Vec::new();
    if let Some(d) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        dirs.push(d.clone());
        // `cargo test` binaries live in target/<profile>/deps.
        if let Some(p) = d.parent() {
            dirs.push(p.to_path_buf());
        }
    }
    dirs.push(models::data_root().join("runtime").join("qts"));
    dirs.into_iter()
        .map(|d| d.join(name))
        .find(|p| p.is_file())
        .ok_or_else(|| QtsError::new("worker_missing", format!("{name} not found next to the app or in <data>/runtime/qts")))
}

fn send(w: &mut Worker, req: &Request) -> std::io::Result<()> {
    let mut line = serde_json::to_string(req).map_err(std::io::Error::other)?;
    line.push('\n');
    w.stdin.write_all(line.as_bytes())?;
    w.stdin.flush()
}

fn spawn(model_dir: &Path, backend: &str) -> Result<Worker, QtsError> {
    let exe = worker_exe()?;
    let mut cmd = Command::new(exe);
    cmd.arg("--model-dir").arg(model_dir).arg("--backend").arg(backend);
    if let Some(ort) = models::find_onnxruntime() {
        cmd.env("ORT_DYLIB_PATH", ort);
    }
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit());
    let mut owned = OwnedCommand::from_command(cmd);
    owned.windows_hide(); // CREATE_NO_WINDOW on Windows
    let mut child = owned.spawn().map_err(|e| QtsError::new("worker_spawn", format!("cannot start the QTS worker: {e}")))?;
    let stdin = child.take_stdin().ok_or_else(|| QtsError::new("worker_spawn", "no worker stdin"))?;
    let stdout = child.take_stdout().ok_or_else(|| QtsError::new("worker_spawn", "no worker stdout"))?;
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
    Ok(Worker { child, stdin, rx, ready: None })
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
                return Err(QtsError::new("worker_died", "the QTS worker exited unexpectedly (see its stderr)"))
            }
            Err(RecvTimeoutError::Timeout) => {
                if start.elapsed() > limit {
                    return Err(QtsError::new("timeout", format!("QTS worker did not reply within {}s", limit.as_secs())));
                }
            }
        }
    }
}

/// Run `f` with a ready worker, (re)spawning as needed. A failed or cancelled
/// call discards the worker so the next call starts clean.
fn with_worker<T>(
    backend: &str,
    f: impl FnOnce(&mut Worker) -> Result<T, QtsError>,
) -> Result<T, QtsError> {
    let dir = models::model_dir();
    if !models::files_present(&dir) {
        return Err(QtsError::new("models_missing", format!("QTS model files not found in {}", dir.display())));
    }
    models::verify(&dir, false).map_err(|m| QtsError::new("models_invalid", m))?;
    let mut guard = slot().lock().unwrap_or_else(|p| p.into_inner());
    if let Some(w) = guard.as_mut() {
        if matches!(w.child.try_wait(), Ok(Some(_))) {
            *guard = None;
        }
    }
    if guard.is_none() {
        let mut w = spawn(&dir, backend)?;
        match wait_reply(&mut w, Duration::from_secs(180))? {
            r @ Reply::Ready { .. } => w.ready = Some(r),
            Reply::Error { message, .. } => return Err(QtsError::new("engine_load", message)),
            other => return Err(QtsError::new("protocol", format!("unexpected first reply {other:?}"))),
        }
        *guard = Some(w);
    }
    let result = f(guard.as_mut().expect("worker present"));
    if result.is_err() {
        *guard = None; // kills the process: also how cancel interrupts a running chunk
    }
    result
}

/// Backend/vocoder labels reported by the live worker (loads it if needed).
pub fn engine_info(backend: Option<&str>) -> Result<(String, String), QtsError> {
    with_worker(backend.unwrap_or(models::default_backend()), |w| match &w.ready {
        Some(Reply::Ready { backend, vocoder, .. }) => Ok((backend.clone(), vocoder.clone())),
        _ => Err(QtsError::new("protocol", "worker not ready")),
    })
}

#[derive(Debug, Clone)]
pub struct SynthOptions {
    /// Path of an x-vector voice prompt (`.pb`) from [`clone_from_wav`].
    pub voice_prompt: Option<PathBuf>,
    /// Codec language id; see [`language_id`].
    pub language_id: i32,
    pub temperature: f32,
    pub top_p: f32,
    pub top_k: i32,
    pub max_chunk_chars: usize,
    /// `metal`, `vulkan`, `cpu`, or `auto` (default for the platform).
    pub backend: Option<String>,
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
        }
    }
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
    if text.trim().is_empty() {
        return Err(QtsError::new("invalid_request", "text is empty"));
    }
    let chunks: Vec<Chunk> = chunk::split_text(text, opts.max_chunk_chars);
    if chunks.is_empty() {
        return Err(QtsError::new("invalid_request", "text has no speakable content"));
    }
    if let Some(vp) = &opts.voice_prompt {
        if !vp.is_file() {
            return Err(QtsError::new("voice_missing", format!("voice prompt {} not found", vp.display())));
        }
    }
    CANCEL.store(false, Ordering::SeqCst);
    let started = Instant::now();
    let tmp = std::env::temp_dir().join(format!("genright-qts-{}-{}", std::process::id(), REQ_ID.fetch_add(1, Ordering::SeqCst)));
    std::fs::create_dir_all(&tmp).map_err(|e| QtsError::new("io", e.to_string()))?;
    let backend = opts.backend.clone().unwrap_or_else(|| models::default_backend().to_string());
    let out = with_worker(&backend, |w| {
        let (be, voc) = match &w.ready {
            Some(Reply::Ready { backend, vocoder, .. }) => (backend.clone(), vocoder.clone()),
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
                    language_id: opts.language_id,
                    max_frames: chunk::frame_budget(c.text.chars().count()),
                    temperature: opts.temperature,
                    top_p: opts.top_p,
                    top_k: opts.top_k,
                    out_pcm: out_pcm.display().to_string(),
                },
            )
            .map_err(|e| QtsError::new("worker_died", format!("cannot reach the QTS worker: {e}")))?;
            match wait_reply(w, Duration::from_secs(600))? {
                Reply::Done { id: rid, samples, gen_ms: g, .. } if rid == id => {
                    gen_ms += g;
                    let raw = std::fs::read(&out_pcm).map_err(|e| QtsError::new("io", e.to_string()))?;
                    let _ = std::fs::remove_file(&out_pcm);
                    if raw.len() != samples * 2 {
                        return Err(QtsError::new("protocol", "chunk size mismatch"));
                    }
                    let mut cp: Vec<i16> = raw.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
                    trim_edge_silence(&mut cp);
                    pcm.extend_from_slice(&cp);
                    pcm.extend(wav::silence(c.pause_ms));
                }
                Reply::Error { message, .. } => return Err(QtsError::new("synthesis", message)),
                other => return Err(QtsError::new("protocol", format!("unexpected reply {other:?}"))),
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

/// Remove leading/trailing silence a chunk emits around its speech. The model
/// pads each utterance with dead air (observed up to ~2 s), which stacked with
/// the explicit join pause produced gaps well over 300 ms. Trims to a fixed
/// noise floor, keeping a short pad so joins are not abrupt.
fn trim_edge_silence(pcm: &mut Vec<i16>) {
    const FLOOR: i16 = 327; // ~1% of full scale at PCM16
    let pad = (crate::SAMPLE_RATE_HZ as usize) * 10 / 1000; // 10 ms kept edge pad
    let first = pcm.iter().position(|s| s.abs() > FLOOR).unwrap_or(pcm.len());
    let last = pcm.iter().rposition(|s| s.abs() > FLOOR).map(|i| i + 1).unwrap_or(0);
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

/// Instant x-vector cloning: encode a short reference WAV (3 to 30 s of clean
/// speech works best) into a voice-clone prompt file at `out`. Returns `out`.
pub fn clone_from_wav(wav_path: &Path, out: &Path) -> Result<PathBuf, QtsError> {
    if !wav_path.is_file() {
        return Err(QtsError::new("invalid_request", format!("reference audio {} not found", wav_path.display())));
    }
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| QtsError::new("io", e.to_string()))?;
    }
    CANCEL.store(false, Ordering::SeqCst);
    let id = REQ_ID.fetch_add(1, Ordering::SeqCst);
    with_worker(models::default_backend(), |w| {
        send(w, &Request::Clone { id, wav: wav_path.display().to_string(), out: out.display().to_string() })
            .map_err(|e| QtsError::new("worker_died", e.to_string()))?;
        match wait_reply(w, Duration::from_secs(120))? {
            Reply::Done { .. } => Ok(()),
            Reply::Error { message, .. } => Err(QtsError::new("clone", message)),
            other => Err(QtsError::new("protocol", format!("unexpected reply {other:?}"))),
        }
    })?;
    Ok(out.to_path_buf())
}

/// Clone into the voice library folder: writes `<data>/voices/local/<id>.xvector.pb`.
pub fn clone_into_library(wav_path: &Path, voice_id: &str) -> Result<PathBuf, QtsError> {
    if voice_id.is_empty() || !voice_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(QtsError::new("invalid_request", "voice id must be [A-Za-z0-9_-]+"));
    }
    clone_from_wav(wav_path, &models::voices_dir().join(format!("{voice_id}.xvector.pb")))
}

/// A voice record path is acceptable only inside the voices folder (the
/// provider param must not read arbitrary files). Returns the canonical path.
pub fn resolve_voice_record(path: &str) -> Result<PathBuf, QtsError> {
    resolve_voice_record_in(&models::voices_dir(), path)
}

pub fn resolve_voice_record_in(root: &Path, path: &str) -> Result<PathBuf, QtsError> {
    let root = root
        .canonicalize()
        .map_err(|_| QtsError::new("voice_missing", format!("voices folder {} does not exist", root.display())))?;
    let p = PathBuf::from(path);
    let p = if p.is_absolute() { p } else { root.join(p) };
    let p = p.canonicalize().map_err(|_| QtsError::new("voice_missing", format!("voice record {path} not found")))?;
    if !p.starts_with(&root) {
        return Err(QtsError::new("invalid_request", "voice_prompt must be inside <data>/voices/local/"));
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
        assert_eq!(resolve_voice_record_in(&voices, outside.to_str().unwrap()).unwrap_err().code, "invalid_request");
        assert_eq!(resolve_voice_record_in(&voices, "../../outside.pb").unwrap_err().code, "invalid_request");
        assert_eq!(resolve_voice_record_in(&voices, "missing.pb").unwrap_err().code, "voice_missing");
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
}

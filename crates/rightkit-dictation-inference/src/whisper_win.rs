// Windows Whisper engine (Track W-Win) — decodes against a model-resident
// whisper.cpp context loaded once via FFI from the staged `whisper.dll`
// (see `whisper_win_ffi.rs`, ABI-pinned to whisper.cpp v1.9.3), falling back
// to the original per-utterance `whisper-cli.exe` spawn if the FFI path is
// unavailable or a decode fails. Windows-only; the macOS equivalent is the
// in-process CoreML/ANE engine in `whisper_coreml.rs`.
//
// Why FFI: the CLI path reloads the ~550MB q5_k GGUF into the Vulkan runtime
// on every utterance, which dominates finalization latency. The resident
// context pays that cost once per process.
#![cfg(target_os = "windows")]

use crate::whisper_win_ffi::WhisperFfiEngine;
use rightkit_child_process::{run_command_capture, CaptureOptions, OwnedCommand};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const SAMPLE_RATE: u32 = 16_000;
// Floor on the decode deadline, scaled up by audio length below
// (`decode_timeout`) so long utterances are not killed outright. Applies to
// both the FFI path (cooperative abort via `whisper_full_params::abort_callback`)
// and the CLI fallback (process kill).
const WHISPER_TIMEOUT_FLOOR: Duration = Duration::from_secs(10);
/// Multiple of real-time audio duration granted before the deadline trips.
/// Vulkan decode of a short utterance is well under 1x real-time warm, but
/// the first decode after a cold GPU/driver, or a long dictation, needs
/// headroom — 6x keeps a 30s utterance bounded at 180s.
const WHISPER_TIMEOUT_MULTIPLE: u32 = 6;
const WHISPER_CLI_STDOUT_CAP: usize = 8 * 1024 * 1024;
const WHISPER_CLI_STDERR_CAP: usize = 1024 * 1024;
/// After this many consecutive FFI decode failures, stop retrying FFI for
/// the rest of the process lifetime and use the CLI path only — avoids
/// paying FFI-attempt overhead every utterance once the DLL/model is known
/// bad. A single failure still falls back to CLI for that call only, since a
/// transient Vulkan hiccup should not demote the whole session.
const WHISPER_FFI_MAX_CONSECUTIVE_FAILURES: u32 = 2;
const WHISPER_LANGUAGES: &[&str] = &[
    "en", "zh", "de", "es", "ru", "ko", "fr", "ja", "pt", "tr", "pl", "ca", "nl", "ar", "sv", "it",
    "id", "hi", "fi", "vi", "he", "uk", "el", "ms", "cs", "ro", "da", "hu", "ta", "no", "th", "ur",
    "hr", "bg", "lt", "la", "mi", "ml", "cy", "sk", "te", "fa", "lv", "bn", "sr", "az", "sl", "kn",
    "et", "mk", "br", "eu", "is", "hy", "ne", "mn", "bs", "kk", "sq", "sw", "gl", "mr", "pa", "si",
    "km", "sn", "yo", "so", "af", "oc", "ka", "be", "tg", "sd", "gu", "am", "yi", "lo", "uz", "fo",
    "ht", "ps", "tk", "nn", "mt", "sa", "lb", "my", "bo", "tl", "mg", "as", "tt", "haw", "ln",
    "ha", "ba", "jw", "su", "yue",
];

fn decode_timeout(samples: &[f32]) -> Duration {
    let audio = Duration::from_secs_f32(samples.len() as f32 / SAMPLE_RATE as f32);
    (audio * WHISPER_TIMEOUT_MULTIPLE).max(WHISPER_TIMEOUT_FLOOR)
}

enum FfiState {
    /// Not yet attempted — lazy-init on first decode.
    Untried,
    Ready(WhisperFfiEngine),
    /// FFI failed to load/init (version pin, missing export, model load), or
    /// decode failed `WHISPER_FFI_MAX_CONSECUTIVE_FAILURES` times in a row.
    /// Carries no reason: both construction sites already log it (a warn on
    /// load failure, an error on repeated decode failure), and nothing reads it
    /// back, so storing it was dead state.
    Unavailable,
}

pub struct WhisperWin {
    /// Resolved path to `whisper-cli.exe`.
    cli: PathBuf,
    /// Resolved path to the GGUF model file.
    model: PathBuf,
    /// Directory `whisper.dll` is staged alongside `whisper-cli.exe` in.
    dll_dir: PathBuf,
    /// Model-resident FFI engine, started lazily on first decode. Decode
    /// calls are serialized on the worker thread already; the lock makes
    /// lazy init race-safe if that ever changes.
    ffi: Mutex<FfiState>,
    /// Consecutive FFI decode failures; reset on success.
    ffi_consecutive_failures: std::sync::atomic::AtomicU32,
}

impl WhisperWin {
    /// Locate the staged whisper.cpp Vulkan CLI + the downloaded GGUF model.
    /// `models_dir` is the app-data models base the engine was given at boot
    /// (`crate::asr::models_base`); the Whisper GGUF lives under
    /// `<app-data>/models/whisper-win` (see `whisper_model.rs` on the shell
    /// side, which places it there after a Pro-gated download).
    pub fn load(models_dir: &Path) -> Result<Self, String> {
        let cli = resolve_whisper_cli()
            .ok_or_else(|| "whisper-cli.exe not found (staged runtime or dev build)".to_string())?;
        let model = resolve_whisper_model(models_dir).ok_or_else(|| {
            "Whisper GGUF model not found — download it from Settings first".to_string()
        })?;
        let dll_dir = cli
            .parent()
            .map(|d| d.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        tracing::info!(
            "Windows Whisper engine loaded: cli={} model={}",
            cli.display(),
            model.display()
        );
        Ok(Self {
            cli,
            model,
            dll_dir,
            ffi: Mutex::new(FfiState::Untried),
            ffi_consecutive_failures: std::sync::atomic::AtomicU32::new(0),
        })
    }

    /// Transcribe 16 kHz mono f32 samples (already `condition_for_asr`-processed
    /// by the shared worker pipeline — this engine does NOT re-condition).
    /// `lang` is a Whisper language code (`"auto"` or e.g. `"hi"`, `"en"`).
    ///
    /// Tries the model-resident FFI engine first (loaded lazily on the first
    /// call), falling back to the per-utterance CLI spawn if FFI is
    /// unavailable or the decode itself fails.
    pub fn transcribe_lang(&self, samples: &[f32], lang: &str) -> Result<String, String> {
        self.transcribe_lang_resolved(samples, lang)
            .map(|(text, _)| text)
    }

    /// Transcribe and return whisper.cpp's resolved language id from either
    /// resident FFI or the CLI's structured result.
    pub fn transcribe_lang_resolved(
        &self,
        samples: &[f32],
        lang: &str,
    ) -> Result<(String, Option<i32>), String> {
        let mut guard = self.ffi.lock().unwrap_or_else(|e| e.into_inner());

        if matches!(*guard, FfiState::Untried) {
            match WhisperFfiEngine::load(&self.dll_dir, &self.model) {
                Ok(engine) => {
                    tracing::info!("Windows Whisper FFI engine ready (model-resident)");
                    *guard = FfiState::Ready(engine);
                }
                Err(e) => {
                    tracing::warn!("Windows Whisper FFI unavailable, using CLI path: {e}");
                    *guard = FfiState::Unavailable;
                }
            }
        }

        // Model/GPU initialization is deliberately outside decode budget. The
        // worker owns startup warmup lifecycle; only actual transcription gets
        // the audio-scaled limit.
        let deadline = Instant::now() + decode_timeout(samples);
        if let FfiState::Ready(engine) = &*guard {
            let started = Instant::now();
            match engine.transcribe_resolved(samples, lang, deadline) {
                Ok((text, resolved_lang)) => {
                    self.ffi_consecutive_failures
                        .store(0, std::sync::atomic::Ordering::Relaxed);
                    tracing::info!(
                        "Windows Whisper transcribe (FFI): lang={} secs={:.1} ms={} chars={}",
                        lang,
                        samples.len() as f32 / SAMPLE_RATE as f32,
                        started.elapsed().as_millis(),
                        text.len()
                    );
                    return Ok((text, Some(resolved_lang)));
                }
                Err(e) => {
                    tracing::error!("Windows Whisper FFI decode failed, falling back to CLI: {e}");
                    let failures = self
                        .ffi_consecutive_failures
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                        + 1;
                    if failures >= WHISPER_FFI_MAX_CONSECUTIVE_FAILURES {
                        tracing::error!(
                            "Windows Whisper FFI failed {failures}x consecutively; CLI-only for this session"
                        );
                        *guard = FfiState::Unavailable;
                    }
                }
            }
        }
        drop(guard);

        // An aborted FFI decode must not hand an already-expired deadline to
        // CLI fallback. Give fallback its own bounded decode budget.
        let fallback_deadline = Instant::now() + decode_timeout(samples);
        self.transcribe_lang_cli(samples, lang, fallback_deadline)
    }

    /// Use a previously resolved whisper.cpp language id for subsequent
    /// windows in one recording, including when resident FFI falls back to CLI.
    pub fn transcribe_lang_locked(
        &self,
        samples: &[f32],
        configured_lang: &str,
        locked_lang: Option<i32>,
    ) -> Result<(String, Option<i32>), String> {
        let effective = if let Some(lang_id) = locked_lang {
            let guard = self.ffi.lock().unwrap_or_else(|e| e.into_inner());
            match &*guard {
                FfiState::Ready(engine) => engine
                    .language_code(lang_id)
                    .or_else(|| whisper_language_code(lang_id).map(str::to_owned))
                    .unwrap_or_else(|| configured_lang.to_string()),
                _ => whisper_language_code(lang_id)
                    .map(str::to_owned)
                    .unwrap_or_else(|| configured_lang.to_string()),
            }
        } else {
            configured_lang.to_string()
        };
        self.transcribe_lang_resolved(samples, &effective)
    }

    /// Original per-utterance CLI path — used as a fallback when the FFI
    /// engine cannot be loaded or a decode call fails. `deadline` bounds the
    /// spawned child the same way the old flat `WHISPER_CLI_TIMEOUT` did,
    /// now scaled to audio length by the caller (`decode_timeout`).
    fn transcribe_lang_cli(
        &self,
        samples: &[f32],
        lang: &str,
        deadline: Instant,
    ) -> Result<(String, Option<i32>), String> {
        let started = Instant::now();
        let wav_path = write_temp_wav(samples)?;
        let _guard = TmpGuard(wav_path.clone());
        let json_base = wav_path.with_extension("result");
        let json_path = json_base.with_extension("result.json");
        let _json_guard = TmpGuard(json_path.clone());

        // RightKit OwnedCommand hides the console window on Windows (the engine
        // sidecar has no console, so a plain spawn would flash one per utterance)
        // and owns the child's process tree.
        let mut owned = OwnedCommand::new(&self.cli);
        owned
            .command_mut()
            .arg("-m")
            .arg(&self.model)
            .arg("-l")
            .arg(lang)
            .arg("-nt") // no timestamps — clean text
            .arg("-np") // no diagnostic prints beyond the result line
            .arg("-oj") // expose resolved language for AUTO fallback locking
            .arg("-of")
            .arg(&json_base)
            .arg("-f")
            .arg(&wav_path);
        // The owner's capture runner drains both pipes on their own threads (a child that fills a
        // pipe buffer cannot deadlock), enforces the deadline by terminating and reaping the whole
        // tree, and bounds its own cleanup.
        let options = CaptureOptions::default()
            .timeout(deadline.saturating_duration_since(Instant::now()))
            .stdout_cap(WHISPER_CLI_STDOUT_CAP)
            .stderr_cap(WHISPER_CLI_STDERR_CAP);
        let output = run_command_capture(owned, None, &options)
            .map_err(|e| format!("run whisper-cli.exe: {e}"))?;
        if output.timed_out {
            let elapsed = started.elapsed();
            tracing::error!("whisper-cli.exe exceeded {:?} deadline; killed", elapsed);
            return Err(format!("whisper-cli.exe timed out after {elapsed:?}"));
        }
        let status = output.status;
        let stdout_bytes = output.stdout.bytes;
        let stderr_bytes = output.stderr.bytes;
        let elapsed_ms = started.elapsed().as_millis();

        if !status.success() {
            let stderr = String::from_utf8_lossy(&stderr_bytes);
            return Err(format!(
                "whisper-cli.exe exited {}: {}",
                status,
                stderr.trim()
            ));
        }
        let text = parse_whisper_cli_stdout(&String::from_utf8_lossy(&stdout_bytes));
        let resolved = std::fs::read_to_string(&json_path)
            .ok()
            .and_then(|json| parse_whisper_cli_language(&json))
            .and_then(whisper_language_id);
        tracing::info!(
            "Windows Whisper transcribe: lang={} samples={} secs={:.1} ms={} chars={}",
            lang,
            samples.len(),
            samples.len() as f32 / SAMPLE_RATE as f32,
            elapsed_ms,
            text.len()
        );
        Ok((text, resolved))
    }
}

/// `whisper-cli.exe -np` still prints the Vulkan device-enumeration banner and
/// blank lines to stdout ahead of the transcript (see the CLI's own startup
/// log) — collect all non-empty, non-banner lines and join them. The CLI's
/// plain-text result mode prints one line per line; blank input yields no
/// output line at all (empty transcript is valid, not an error).
fn parse_whisper_cli_stdout(stdout: &str) -> String {
    stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| !line.starts_with("ggml_vulkan:"))
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

fn parse_whisper_cli_language(json: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()?
        .get("result")?
        .get("language")?
        .as_str()
        .map(str::to_owned)
}

fn whisper_language_code(lang_id: i32) -> Option<&'static str> {
    WHISPER_LANGUAGES.get(lang_id as usize).copied()
}

fn whisper_language_id(code: String) -> Option<i32> {
    WHISPER_LANGUAGES
        .iter()
        .position(|candidate| *candidate == code)
        .map(|id| id as i32)
}

/// Resolve `whisper-cli.exe`. Order: packaged app resources (`<exe_dir>/runtime/`,
/// the production layout `package.ps1` stages into `resources/runtime`), then the
/// dev fallback under the repo's local whisper.cpp Vulkan build. Mirrors the
/// ONNX runtime DLL resolution order in `main.rs::configure_ort_dylib`.
fn resolve_whisper_cli() -> Option<PathBuf> {
    if let Some(env) = rightkit_dictation_core::dev_var_os!("RIGHTKIT_WHISPER_CLI") {
        let p = PathBuf::from(env);
        if p.exists() {
            return Some(p);
        }
    }
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()));
    if let Some(dir) = &exe_dir {
        let exe_sibling = dir.join("whisper-cli.exe");
        if exe_sibling.exists() {
            return Some(exe_sibling);
        }
        let runtime_sibling = dir.join("runtime").join("whisper-cli.exe");
        if runtime_sibling.exists() {
            return Some(runtime_sibling);
        }
        let packaged_resource = dir
            .join("resources")
            .join("runtime")
            .join("whisper-cli.exe");
        if packaged_resource.exists() {
            return Some(packaged_resource);
        }
    }
    None
}

/// Resolve the model file: `<models_dir>/whisper-win/*.{bin,gguf}` — first
/// match found. whisper.cpp models are GGML `.bin` files (`ggml-*.bin`);
/// `.gguf` is accepted too in case the published archive ever switches
/// formats. `RIGHTKIT_WHISPER_GGUF` overrides for QA/dev.
fn resolve_whisper_model(models_dir: &Path) -> Option<PathBuf> {
    if let Some(env) = rightkit_dictation_core::dev_var_os!("RIGHTKIT_WHISPER_GGUF") {
        let p = PathBuf::from(env);
        if p.exists() {
            return Some(p);
        }
    }
    let dir = models_dir.join("whisper-win");
    let entries = std::fs::read_dir(&dir).ok()?;
    entries.filter_map(|e| e.ok()).map(|e| e.path()).find(|p| {
        matches!(
            p.extension().and_then(|s| s.to_str()),
            Some("bin") | Some("gguf")
        )
    })
}

/// Write 16 kHz mono f32 samples to a temp WAV (16-bit PCM — whisper-cli reads
/// both, but PCM16 keeps the file small and matches the format whisper.cpp's
/// own examples use). Unique per-call filename so concurrent utterances never
/// collide (the worker is single-threaded per session, but a stale temp file
/// from a crashed prior run must never be silently reused).
fn write_temp_wav(samples: &[f32]) -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join("hr_whisper_win");
    std::fs::create_dir_all(&dir).map_err(|e| format!("temp dir: {e}"))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = dir.join(format!("utt_{}_{stamp}.wav", std::process::id()));

    rightkit_audio::wav::write_wav_mono_i16(&path, SAMPLE_RATE, samples)
        .map_err(|e| format!("write wav: {e}"))?;
    Ok(path)
}

struct TmpGuard(PathBuf);

impl Drop for TmpGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

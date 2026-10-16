//! In-worker engine loop (feature `engine`). Runs on the process main thread
//! because `Qwen3TtsEngine` owns GGML pointers and is not `Send`. The engine
//! is leaked and the process exits via `exit()`, because QTS teardown can hang
//! (VoiceRight ADR-4 finding).
//!
//! `--mode` names the mode loaded before the worker reports ready. Other modes load on their
//! first request, from the location the client delivered at start (see [`ModeLocation`]), and
//! stay loaded: switching mode never respawns the worker. Base loads the 0.6B voice-clone model;
//! CustomVoice and VoiceDesign load the 1.7B exports. Voice cloning always runs on the Base
//! engine, whichever mode the worker started for.
use crate::mode_location::{resolve_location, EngineCache, ModeLocation};
use crate::models;
use crate::protocol::{Mode, Reply, Request, REPLY_PREFIX};
use crate::wav;
use qts::{
    ModeModelPaths, ModeSynthesizeRequest, ModelPaths, PresetSpeaker, QtsMode, Qwen3TtsEngine,
    SynthesizeRequest, TensorF32, VoiceClonePromptV2, VOICE_CLONE_PROMPT_V2_SCHEMA_VERSION,
};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// What the worker needs to start. The binary fills defaults; see `worker_args`.
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// Model folder of `mode`, and of any mode without a location of its own.
    pub model_dir: PathBuf,
    pub backend: String,
    /// Mode loaded before the worker reports ready.
    pub mode: Mode,
    /// Vocoder of `mode`, and the default for modes whose folder holds none.
    pub vocoder: PathBuf,
    pub data_root: Option<PathBuf>,
    pub ort_runtime: Option<PathBuf>,
    /// Per-mode locations; an entry overrides `model_dir` and `vocoder` for its mode.
    pub mode_locations: Vec<ModeLocation>,
    /// How long the worker waits for a request before it exits. Zero is a zero-length window: the
    /// worker exits as soon as no request is waiting. Callers with no explicit value take
    /// `worker_args::env_idle_timeout()`.
    pub idle_timeout: Duration,
}

impl WorkerConfig {
    /// Register where `location.mode` lives; replaces an earlier entry for the same mode.
    pub fn with_mode_location(mut self, location: ModeLocation) -> Self {
        self.mode_locations.retain(|l| l.mode != location.mode);
        self.mode_locations.push(location);
        self
    }

    pub fn with_idle_timeout(mut self, idle_timeout: Duration) -> Self {
        self.idle_timeout = idle_timeout;
        self
    }

    /// Model folder and vocoder the worker loads `mode` from.
    pub fn location(&self, mode: Mode) -> (PathBuf, PathBuf) {
        resolve_location(&self.model_dir, &self.vocoder, &self.mode_locations, mode)
    }
}

fn reply(r: &Reply) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(
        out,
        "{REPLY_PREFIX}{}",
        serde_json::to_string(r).unwrap_or_default()
    );
    let _ = out.flush();
}

/// Terminate without running `exit()` handlers. The leaked engine's GGML/Metal
/// teardown asserts during libc `exit()` (ggml_metal_device_free residency
/// sets); every reply and output file is already flushed, so skip atexit.
fn quit(code: i32) -> ! {
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    #[cfg(unix)]
    unsafe {
        libc::_exit(code)
    }
    #[cfg(not(unix))]
    std::process::exit(code)
}

fn err(id: u64, e: impl std::fmt::Display) -> Reply {
    Reply::Error {
        id,
        message: e.to_string(),
    }
}

fn qts_mode(mode: Mode) -> QtsMode {
    match mode {
        Mode::Base => QtsMode::Base,
        Mode::CustomVoice => QtsMode::CustomVoice,
        Mode::VoiceDesign => QtsMode::VoiceDesign,
    }
}

/// Load the engine for `mode` from its location. Errors are complete reply messages; a missing
/// model file is `model_missing: <mode>: <paths>`.
fn load_engine(cfg: &WorkerConfig, mode: Mode) -> Result<Qwen3TtsEngine, String> {
    let (model_dir, vocoder) = cfg.location(mode);
    match mode {
        Mode::Base => {
            let main = ModelPaths::from_model_dir(&model_dir).main_gguf;
            let missing: Vec<PathBuf> = [main.clone(), vocoder.clone()]
                .into_iter()
                .filter(|p| !p.is_file())
                .collect();
            if !missing.is_empty() {
                return Err(model_missing(mode, &missing));
            }
            Qwen3TtsEngine::load(ModelPaths::new(main, vocoder))
                .map_err(|e| format!("engine load failed: {e}"))
        }
        Mode::CustomVoice | Mode::VoiceDesign => {
            let qmode = qts_mode(mode);
            let paths =
                ModeModelPaths::from_model_dir(&model_dir, qmode).with_vocoder(vocoder.clone());
            let missing: Vec<PathBuf> = [
                paths.talker_gguf.clone(),
                paths.code_predictor_gguf.clone(),
                paths.tokenizer_gguf.clone(),
                paths.vocoder_onnx.clone(),
            ]
            .into_iter()
            .filter(|p| !p.is_file())
            .collect();
            if !missing.is_empty() {
                return Err(model_missing(mode, &missing));
            }
            Qwen3TtsEngine::from_model_dir_for_mode_with_vocoder(&model_dir, &vocoder, qmode)
                .map_err(|e| format!("engine load failed: {e}"))
        }
    }
}

fn model_missing(mode: Mode, missing: &[PathBuf]) -> String {
    let paths: Vec<String> = missing.iter().map(|p| p.display().to_string()).collect();
    format!("model_missing: {}: {}", mode.as_str(), paths.join(", "))
}

/// Run until `quit`, stdin EOF (parent gone), or idle timeout.
pub fn run(cfg: &WorkerConfig) -> ! {
    // Read by qts/ggml/ORT during engine initialisation; the worker is single-threaded here.
    std::env::set_var("QWEN3_TTS_BACKEND", &cfg.backend);
    for (k, v) in [
        ("QWEN3_TTS_BACKEND_FALLBACK", "cpu"),
        ("QWEN3_TTS_VOCODER_EP", "cpu"),
        ("QWEN3_TTS_VOCODER_EP_FALLBACK", "cpu"),
    ] {
        if std::env::var_os(k).is_none() {
            std::env::set_var(k, v);
        }
    }
    models::bind_ort_runtime(cfg.ort_runtime.as_deref());
    // Searched after the explicit binding; in release the inherited variable is already gone.
    let mut dirs: Vec<PathBuf> = Vec::new();
    let (base_dir, _) = cfg.location(Mode::Base);
    if let Some(parent) = base_dir.parent() {
        dirs.push(parent.join("runtime"));
    }
    if let Ok(root) = models::data_root_in(cfg.data_root.as_deref()) {
        dirs.push(root.join("runtime").join("qts"));
    }
    rightkit_onnx::export_discovered_runtime(dirs);

    let t = Instant::now();
    // Held for the life of the process, which ends in `quit` (no teardown, no drop).
    let mut engines: EngineCache<Qwen3TtsEngine> = EngineCache::default();
    match engines.get_or_load(cfg.mode, || load_engine(cfg, cfg.mode)) {
        Ok(engine) => reply(&Reply::Ready {
            backend: format!("{:?}", engine.primary_backend_kind()).to_ascii_lowercase(),
            vocoder: engine.vocoder_backend_label().to_string(),
            load_ms: t.elapsed().as_millis() as u64,
            version: env!("CARGO_PKG_VERSION").to_string(),
        }),
        Err(message) => {
            reply(&err(0, message));
            quit(2);
        }
    }

    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let threads = std::thread::available_parallelism()
        .map(|n| n.get().clamp(2, 8))
        .unwrap_or(4);
    loop {
        let line = match rx.recv_timeout(cfg.idle_timeout) {
            Ok(l) => l,
            Err(_) => quit(0), // idle, or stdin closed
        };
        let req: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                reply(&err(0, format!("bad request: {e}")));
                continue;
            }
        };
        match req {
            Request::Quit => quit(0),
            Request::Clone {
                id,
                wav: wav_path,
                out,
            } => {
                // Cloning needs the Base speaker encoder, whichever mode the worker started for.
                reply(
                    &match engines.get_or_load(Mode::Base, || load_engine(cfg, Mode::Base)) {
                        Ok(engine) => clone(engine, id, &wav_path, &out),
                        Err(message) => err(id, message),
                    },
                );
            }
            Request::Synth {
                id,
                text,
                voice_prompt,
                mode,
                instruct,
                speaker,
                language_id,
                max_frames,
                temperature,
                top_p,
                top_k,
                out_pcm,
            } => {
                let started = Instant::now();
                let requested = mode.unwrap_or_default();
                let r = match engines.get_or_load(requested, || load_engine(cfg, requested)) {
                    Err(message) => Err(message),
                    Ok(engine) => synth(
                        engine,
                        requested,
                        voice_prompt.as_deref(),
                        instruct.as_deref(),
                        speaker.as_deref(),
                        SynthesizeRequest {
                            text,
                            max_audio_frames: max_frames,
                            language_id,
                            temperature,
                            top_p,
                            top_k,
                            thread_count: threads,
                            ..SynthesizeRequest::default()
                        },
                    ),
                };
                reply(&match r {
                    Ok(pcm) => {
                        let i16s = wav::f32_to_i16(&pcm);
                        let bytes: Vec<u8> = i16s.iter().flat_map(|s| s.to_le_bytes()).collect();
                        match std::fs::write(&out_pcm, bytes) {
                            Ok(()) => Reply::Done {
                                id,
                                samples: i16s.len(),
                                sample_rate: crate::SAMPLE_RATE_HZ,
                                gen_ms: started.elapsed().as_millis() as u64,
                            },
                            Err(e) => err(id, format!("cannot write {out_pcm}: {e}")),
                        }
                    }
                    Err(e) => err(id, e),
                });
            }
        }
    }
}

fn synth(
    engine: &Qwen3TtsEngine,
    mode: Mode,
    prompt: Option<&str>,
    instruct: Option<&str>,
    speaker: Option<&str>,
    req: SynthesizeRequest,
) -> Result<Vec<f32>, String> {
    match mode {
        Mode::Base => {
            let result = match prompt {
                Some(p) => {
                    let bytes = std::fs::read(p)
                        .map_err(|e| format!("cannot read voice prompt {p}: {e}"))?;
                    let prompt = engine
                        .decode_voice_clone_prompt(&bytes)
                        .map_err(|e| e.to_string())?;
                    engine.synthesize_with_voice_clone_prompt(&req, &prompt)
                }
                None => engine.synthesize(&req),
            };
            result.map(|r| r.pcm_f32).map_err(|e| e.to_string())
        }
        Mode::CustomVoice | Mode::VoiceDesign => {
            if prompt.is_some() {
                return Err("voice_prompt is only accepted in base mode".into());
            }
            let speaker = match speaker {
                Some(name) => Some(
                    PresetSpeaker::parse(name)
                        .ok_or_else(|| format!("unknown preset speaker {name}"))?,
                ),
                None => None,
            };
            let request = ModeSynthesizeRequest {
                synthesis: req,
                mode: qts_mode(mode),
                instruct: instruct.map(str::to_owned),
                speaker,
                voice_clone_prompt: None,
                speaker_embedding: None,
            };
            engine
                .synthesize_mode(&request)
                .map(|r| r.pcm_f32)
                .map_err(|e| e.to_string())
        }
    }
}

fn clone(engine: &Qwen3TtsEngine, id: u64, wav_path: &str, out: &str) -> Reply {
    let started = Instant::now();
    let go = || -> Result<(), String> {
        let wav_bytes =
            std::fs::read(wav_path).map_err(|e| format!("cannot read {wav_path}: {e}"))?;
        let embedding = engine
            .encode_reference_speaker(&wav_bytes)
            .map_err(|e| e.to_string())?;
        let prompt = VoiceClonePromptV2 {
            schema_version: VOICE_CLONE_PROMPT_V2_SCHEMA_VERSION,
            source: Path::new(wav_path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            model_id: "qwen3-tts-0.6b".to_string(),
            speaker_encoder_sample_rate_hz: qts::SAMPLE_RATE_HZ,
            x_vector_only_mode: true,
            icl_mode: false,
            ref_text: String::new(),
            ref_code: None,
            ref_spk_embedding: Some(TensorF32 {
                shape: vec![embedding.len() as u32],
                values: embedding,
            }),
        };
        let bytes = prompt.to_protobuf_vec().map_err(|e| e.to_string())?;
        std::fs::write(out, bytes).map_err(|e| format!("cannot write {out}: {e}"))
    };
    match go() {
        Ok(()) => Reply::Done {
            id,
            samples: 0,
            sample_rate: crate::SAMPLE_RATE_HZ,
            gen_ms: started.elapsed().as_millis() as u64,
        },
        Err(e) => err(id, e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::PRESET_SPEAKERS;

    #[test]
    fn preset_speaker_list_matches_the_engine_enum() {
        let engine: Vec<&str> = PresetSpeaker::ALL.iter().map(|s| s.as_str()).collect();
        assert_eq!(engine, PRESET_SPEAKERS);
    }

    #[test]
    fn mode_model_files_name_what_the_engine_loads() {
        let dir = std::env::temp_dir().join(format!("rk-mode-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (mode, qmode) in [
            (Mode::CustomVoice, QtsMode::CustomVoice),
            (Mode::VoiceDesign, QtsMode::VoiceDesign),
        ] {
            let paths = ModeModelPaths::from_model_dir(&dir, qmode);
            let want = vec![
                paths.talker_gguf.clone(),
                paths.code_predictor_gguf.clone(),
                paths.tokenizer_gguf.clone(),
            ];
            assert_eq!(
                models::mode_model_files(&dir, mode),
                want,
                "{mode:?} empty folder"
            );
        }
        // A q8_0 export, then a bundled one, resolve the way the engine resolves them.
        let stem = "qwen3-tts-1.7b-voicedesign";
        for name in ["talker-q8_0", "code-predictor-q8_0"] {
            std::fs::write(dir.join(format!("{stem}-{name}.gguf")), b"x").unwrap();
        }
        std::fs::write(dir.join(format!("{stem}-tokenizer.gguf")), b"x").unwrap();
        let paths = ModeModelPaths::from_model_dir(&dir, QtsMode::VoiceDesign);
        assert_eq!(
            models::mode_model_files(&dir, Mode::VoiceDesign),
            vec![
                paths.talker_gguf,
                paths.code_predictor_gguf,
                paths.tokenizer_gguf
            ]
        );
        std::fs::write(dir.join(format!("{stem}-q4_k.gguf")), b"x").unwrap();
        let paths = ModeModelPaths::from_model_dir(&dir, QtsMode::VoiceDesign);
        assert_eq!(
            models::mode_model_files(&dir, Mode::VoiceDesign),
            vec![paths.talker_gguf]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn config() -> WorkerConfig {
        WorkerConfig {
            model_dir: "/base".into(),
            backend: "cpu".into(),
            mode: Mode::VoiceDesign,
            vocoder: "/base/v.onnx".into(),
            data_root: None,
            ort_runtime: None,
            mode_locations: Vec::new(),
            idle_timeout: Duration::from_secs(600),
        }
    }

    #[test]
    fn with_mode_location_replaces_the_same_mode_and_resolves_per_mode() {
        let cfg = config()
            .with_mode_location(ModeLocation::new(Mode::Base, "/old"))
            .with_mode_location(ModeLocation::new(Mode::Base, "/b6").with_vocoder("/b6/v.onnx"))
            .with_mode_location(
                ModeLocation::new(Mode::CustomVoice, "/cv").with_vocoder("/cv/v.onnx"),
            );
        assert_eq!(cfg.mode_locations.len(), 2);
        assert_eq!(
            cfg.location(Mode::Base),
            ("/b6".into(), "/b6/v.onnx".into())
        );
        assert_eq!(
            cfg.location(Mode::CustomVoice),
            ("/cv".into(), "/cv/v.onnx".into())
        );
        // The started mode, with no entry of its own, keeps the config folder and vocoder.
        assert_eq!(
            cfg.location(Mode::VoiceDesign),
            ("/base".into(), "/base/v.onnx".into())
        );
    }
}

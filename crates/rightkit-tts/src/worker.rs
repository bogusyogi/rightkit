//! In-worker engine loop (feature `engine`). Runs on the process main thread
//! because `Qwen3TtsEngine` owns GGML pointers and is not `Send`. The engine
//! is leaked and the process exits via `exit()`, because QTS teardown can hang
//! (VoiceRight ADR-4 finding).
use crate::protocol::{Reply, Request, REPLY_PREFIX};
use crate::{models, wav};
use qts::{
    Qwen3TtsEngine, SynthesizeRequest, TensorF32, VoiceClonePromptV2,
    VOICE_CLONE_PROMPT_V2_SCHEMA_VERSION,
};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

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

fn idle_limit() -> Duration {
    let secs = std::env::var("RIGHTKIT_TTS_IDLE_SECS")
        .or_else(|_| std::env::var("GENRIGHT_QTS_IDLE_SECS"))
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(600u64);
    Duration::from_secs(secs)
}

/// Run until `quit`, stdin EOF (parent gone), or idle timeout.
pub fn run(model_dir: &Path, backend: &str) -> ! {
    // Read by qts/ggml/ORT during engine initialisation; the worker is single-threaded here.
    std::env::set_var("QWEN3_TTS_BACKEND", backend);
    for (k, v) in [
        ("QWEN3_TTS_BACKEND_FALLBACK", "cpu"),
        ("QWEN3_TTS_VOCODER_EP", "cpu"),
        ("QWEN3_TTS_VOCODER_EP_FALLBACK", "cpu"),
    ] {
        if std::env::var_os(k).is_none() {
            std::env::set_var(k, v);
        }
    }
    // Binds ORT_DYLIB_PATH and PATH to the discovered runtime unless the parent already set it.
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(parent) = models::model_dir().parent() {
        dirs.push(parent.join("runtime"));
    }
    dirs.push(models::data_root().join("runtime").join("qts"));
    rightkit_ort::export_discovered_runtime(dirs);

    let t = Instant::now();
    let engine: &'static Qwen3TtsEngine = match Qwen3TtsEngine::from_model_dir(model_dir) {
        Ok(e) => Box::leak(Box::new(e)),
        Err(e) => {
            reply(&err(0, format!("engine load failed: {e}")));
            quit(2);
        }
    };
    reply(&Reply::Ready {
        backend: format!("{:?}", engine.primary_backend_kind()).to_ascii_lowercase(),
        vocoder: engine.vocoder_backend_label().to_string(),
        load_ms: t.elapsed().as_millis() as u64,
        version: env!("CARGO_PKG_VERSION").to_string(),
    });

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
        let line = match rx.recv_timeout(idle_limit()) {
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
            } => reply(&clone(engine, id, &wav_path, &out)),
            Request::Synth {
                id,
                text,
                voice_prompt,
                language_id,
                max_frames,
                temperature,
                top_p,
                top_k,
                out_pcm,
            } => {
                let started = Instant::now();
                let r = synth(
                    engine,
                    &text,
                    voice_prompt.as_deref(),
                    SynthesizeRequest {
                        text: text.clone(),
                        max_audio_frames: max_frames,
                        language_id,
                        temperature,
                        top_p,
                        top_k,
                        thread_count: threads,
                        ..SynthesizeRequest::default()
                    },
                );
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
    _text: &str,
    prompt: Option<&str>,
    req: SynthesizeRequest,
) -> Result<Vec<f32>, String> {
    let result = match prompt {
        Some(p) => {
            let bytes =
                std::fs::read(p).map_err(|e| format!("cannot read voice prompt {p}: {e}"))?;
            let prompt = engine
                .decode_voice_clone_prompt(&bytes)
                .map_err(|e| e.to_string())?;
            engine.synthesize_with_voice_clone_prompt(&req, &prompt)
        }
        None => engine.synthesize(&req),
    };
    result.map(|r| r.pcm_f32).map_err(|e| e.to_string())
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

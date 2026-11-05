//! Native ASR via a whisper.cpp CLI binary (`whisper-cli`). The binary is
//! resolved by `media::tools()` (env/bundled/PATH); models are resolved from
//! an explicit path, `RIGHTKIT_WHISPER_MODEL`, a store-installed model
//! (`asr_provision`, a free Hugging Face download through `rightkit_models`
//! into `<data_dir>/models/whisper`), or a flat `*.bin` in that folder. When no
//! binary or model is available the command reports `asr_unavailable` — it
//! never fakes a transcript.

use std::path::{Path, PathBuf};

use rightkit_models::{ErrorCode, Event, FileSpec, InstallOptions, ModelSpec, RevisionSpec, Store};
use serde_json::{json, Value};

use rightkit_local_gen::model_store;

use super::media;
use crate::{Context, GenError, GenResult};

fn s(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Pinned upstream blobs (ggerganov/whisper.cpp @ main): file name, bytes,
/// sha256. sha256 recorded from the HF LFS x-linked-etag; ggml-tiny.en.bin
/// additionally verified end-to-end against a downloaded copy.
const WHISPER_MODELS: &[(&str, u64, &str)] = &[
    (
        "ggml-tiny.en.bin",
        77704715,
        "921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f",
    ),
    (
        "ggml-tiny.bin",
        77691713,
        "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21",
    ),
    (
        "ggml-base.en.bin",
        147964211,
        "a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002",
    ),
    (
        "ggml-base.bin",
        147951465,
        "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
    ),
    (
        "ggml-small.en.bin",
        487614201,
        "c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d",
    ),
];

/// Store id of an allowlisted ggml file, e.g. `ggml-tiny.en.bin` → `whisper-tiny.en`.
fn whisper_id(name: &str) -> String {
    format!(
        "whisper-{}",
        name.trim_start_matches("ggml-").trim_end_matches(".bin")
    )
}

/// Store root for whisper models: the flat `<data_dir>/models/whisper` folder.
fn whisper_root(data_dir: &Path) -> PathBuf {
    data_dir.join("models").join("whisper")
}

/// One-file store spec for an allowlisted ggml model, pinned by size and sha256.
fn whisper_spec(name: &str, bytes: u64, sha256: &str) -> ModelSpec {
    ModelSpec::new(
        whisper_id(name),
        vec![RevisionSpec {
            revision: "main".into(),
            quant: None,
            files: vec![FileSpec {
                path: name.into(),
                url: format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{name}"),
                bytes: Some(bytes),
                sha256: Some(sha256.into()),
                ..Default::default()
            }],
            ..Default::default()
        }],
    )
    .with_label(format!("Whisper {name}"))
}

/// Parse SRT into segment records. Line endings are normalised and a UTF-8 BOM
/// is dropped. Each blank-line-separated block is parsed on its own, and a block
/// that does not parse is skipped without failing the file. Surviving cues are
/// numbered in order.
pub fn parse_srt(text: &str) -> Vec<Value> {
    let text = text
        .trim_start_matches('\u{feff}')
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let mut out = Vec::new();
    for block in text.split("\n\n") {
        let Ok(cues) = rightkit_transcript::parse_srt(block) else {
            continue;
        };
        for cue in cues {
            let index = out.len() + 1;
            out.push(json!({
                "index": index,
                "start_s": cue.start_ms as f64 / 1000.0,
                "end_s": cue.end_ms as f64 / 1000.0,
                "text": cue.lines.join(" "),
            }));
        }
    }
    out
}

/// `studio.pipelines.transcribe` `{audio_path, model?, lang?, out_dir?}` →
/// whisper.cpp → `{engine, model, language, segments, srt_path, srt}`.
/// Real timestamps from the ASR engine; nothing estimated.
pub fn transcribe(ctx: &Context, input: &Value) -> GenResult<Value> {
    let t = media::tools();
    let whisper = t.whisper.clone().ok_or_else(|| {
        GenError::new(
            "asr_unavailable",
            "no whisper.cpp binary (set RIGHTKIT_WHISPER or install whisper-cli)",
        )
    })?;
    let model = media::whisper_model(&ctx.data_dir, input.get("model").and_then(Value::as_str))
        .ok_or_else(|| GenError::new("asr_unavailable", "no whisper model (set RIGHTKIT_WHISPER_MODEL or provision one into <data>/models/whisper)"))?;
    let audio = PathBuf::from(s(input, "audio_path"));
    if !audio.is_file() {
        return Err(GenError::new(
            "invalid_input",
            format!("audio not found: {}", audio.display()),
        ));
    }
    let out_dir = PathBuf::from(if s(input, "out_dir").is_empty() {
        ctx.data_dir
            .join("pipelines")
            .join("asr")
            .to_string_lossy()
            .into_owned()
    } else {
        s(input, "out_dir")
    });
    std::fs::create_dir_all(&out_dir)
        .map_err(|e| GenError::new("io", format!("cannot create {}: {e}", out_dir.display())))?;
    let base = out_dir.join(format!("asr-{}", crate::pipeline_workflows::stamp()));
    let mut args = vec![
        "-m".into(),
        model.to_string_lossy().into_owned(),
        "-f".into(),
        audio.to_string_lossy().into_owned(),
        "-osrt".into(),
        "-of".into(),
        base.to_string_lossy().into_owned(),
    ];
    let lang = s(input, "lang");
    if !lang.is_empty() {
        args.extend(["-l".into(), lang.clone()]);
    }
    let (ok, _, stderr) = media::run(&whisper, &args, "whisper")?;
    let srt_path = base.with_extension("srt");
    if !ok && !srt_path.is_file() {
        return Err(GenError::new(
            "asr_failed",
            format!(
                "whisper exited {}: {}",
                ok,
                stderr.trim().chars().take(600).collect::<String>()
            ),
        ));
    }
    let srt = std::fs::read_to_string(&srt_path)
        .map_err(|e| GenError::new("asr_failed", format!("whisper produced no srt: {e}")))?;
    let segments = parse_srt(&srt);
    Ok(json!({
        "engine": "whisper.cpp",
        "binary": whisper.to_string_lossy(),
        "model": model.to_string_lossy(),
        "language": if lang.is_empty() { json!("auto") } else { json!(lang) },
        "audio_path": audio.to_string_lossy(),
        "segments": segments,
        "segment_count": srt.split("\n\n").filter(|b| b.contains("-->")).count(),
        "srt": srt,
        "srt_path": srt_path.to_string_lossy(),
    }))
}

/// `studio.pipelines.asr_provision`: install a free ggml whisper model into the
/// `<data_dir>/models/whisper` store (Hugging Face, no key). A pin-matching
/// flat file already in that folder is adopted without a download. The binary
/// itself is provisioned externally (bundled, package manager, or built
/// whisper.cpp) — we never build at runtime.
pub fn asr_provision(ctx: &Context, input: &Value) -> GenResult<Value> {
    let name = s(input, "model");
    let name = if name.is_empty() {
        "ggml-tiny.en.bin".to_string()
    } else {
        name
    };
    let Some(&(_, want_bytes, want_sha)) = WHISPER_MODELS.iter().find(|(n, _, _)| *n == name)
    else {
        return Err(GenError::new(
            "invalid_input",
            format!(
                "model must be one of: {}",
                WHISPER_MODELS
                    .iter()
                    .map(|p| p.0)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    };
    let dir = whisper_root(&ctx.data_dir);
    std::fs::create_dir_all(&dir)
        .map_err(|e| GenError::new("io", format!("cannot create {}: {e}", dir.display())))?;
    let url = format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{name}");
    let spec = whisper_spec(&name, want_bytes, want_sha);
    let store = Store::new(&dir, model_store::store_app());
    let mut downloaded = false;
    let outcome = store
        .adopt_then_install(
            &spec,
            &InstallOptions::default(),
            |f| Some(dir.join(&f.path)),
            |ev| {
                // Byte progress is only reported by a transfer; adopted files emit none.
                if matches!(ev, Event::Progress { .. }) {
                    downloaded = true;
                }
            },
        )
        .map_err(|e| {
            let code = match e.code {
                ErrorCode::Download => "download_failed".to_string(),
                ErrorCode::HashMismatch | ErrorCode::SizeMismatch => "model_corrupt".to_string(),
                _ => model_store::error_code(&e),
            };
            GenError::new(code, format!("{url}: {}", e.message))
        })?;
    let model = outcome.dir.join(&name);
    if downloaded {
        Ok(
            json!({ "model": model.to_string_lossy(), "already_present": false, "bytes": want_bytes, "sha256": want_sha, "url": url }),
        )
    } else {
        Ok(
            json!({ "model": model.to_string_lossy(), "already_present": true, "sha256": want_sha, "bytes": want_bytes }),
        )
    }
}

/// Largest allowlisted whisper model installed in the store, resolved to its
/// file inside the active revision. `None` when the store has none.
pub fn installed_whisper_model(data_dir: &Path) -> Option<PathBuf> {
    let store = Store::new(whisper_root(data_dir), model_store::store_app());
    WHISPER_MODELS
        .iter()
        .filter_map(|&(name, bytes, _)| {
            let rev = store.resolve(&whisper_id(name))?;
            let file = rev.dir.join(name);
            file.is_file().then_some((bytes, file))
        })
        .max_by_key(|(bytes, _)| *bytes)
        .map(|(_, file)| file)
}

/// `studio.pipelines.asr_status`: binary + model resolution, for the panel.
pub fn asr_status(ctx: &Context) -> Value {
    let t = media::tools();
    json!({
        "binary": t.whisper.as_ref().map(|p| p.to_string_lossy()),
        "model": media::whisper_model(&ctx.data_dir, None).map(|p| p.to_string_lossy().into_owned()),
        "ready": t.whisper.is_some() && media::whisper_model(&ctx.data_dir, None).is_some(),
    })
}

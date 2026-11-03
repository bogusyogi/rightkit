//! On-screen-text moment search: `moments.ocr_index` samples a source at a fixed interval
//! through the typed ffmpeg boundary, reads each sampled frame with `rightkit-ocr`, and stores
//! the recognised text with source timestamps under `<project>/analysis/ocr-<source>.json`.
//! `moments.search` matches a query against those indexes with `rightkit-search` and returns
//! hits at the source time of the frame that showed the text.
//!
//! The index records the BLAKE3 of the source bytes. Relinking the same bytes reuses it;
//! changed bytes make it stale, so it is rebuilt. A corrupt index is treated as absent.
//! The model directory comes from `model_dir`; the native
//! backend (Apple Vision / Windows OCR) needs none.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use rightkit_ffmpeg::FfmpegCommand;
use rightkit_ocr::{BackendPreference, Ocr, OcrArtifactManifest, OcrOptions, OnnxConfig};
use rightkit_search::{Query, TextMatcher};
use rightkit_video_project::worker::{run_ffmpeg, RunOptions};
use rightkit_video_project::{Access, Project, ProjectError};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use rightkit_engine_host::env::Env;
use rightkit_engine_host::project::{analysis_dir, bad, find_source, io_err, media_tools};

/// Index layout version. A different version is rebuilt, never read.
const INDEX_VERSION: u32 = 1;
const DEFAULT_INTERVAL_MS: i64 = 1_000;
const MIN_INTERVAL_MS: i64 = 250;
const MAX_INTERVAL_MS: i64 = 10_000;
const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 500;
/// Frame extraction and OCR run for a whole source; one call may take a while on long media.
const INDEX_TIMEOUT: Duration = Duration::from_secs(900);
#[derive(Debug, Serialize, Deserialize)]
struct OcrIndex {
    version: u32,
    source: String,
    /// BLAKE3 hex of the source bytes this index was built from.
    content_hash: String,
    interval_ms: i64,
    /// Frames sampled, including those with no text.
    sampled: usize,
    /// Frames that produced text, in time order.
    frames: Vec<OcrFrame>,
}

#[derive(Debug, Serialize, Deserialize)]
struct OcrFrame {
    /// Source time of the sampled frame, in milliseconds.
    at_ms: i64,
    /// Recognised lines joined with newlines.
    text: String,
}

fn index_path(project: &Path, source_id: &str) -> PathBuf {
    analysis_dir(project).join(format!("ocr-{source_id}.json"))
}

/// Missing, unreadable, corrupt or other-version indexes all read as absent.
fn read_index(path: &Path) -> Option<OcrIndex> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice::<OcrIndex>(&bytes)
        .ok()
        .filter(|index| index.version == INDEX_VERSION)
}

fn write_index(path: &Path, index: &OcrIndex) -> Result<(), ProjectError> {
    let body = serde_json::to_vec_pretty(index)
        .map_err(|e| bad(format!("encoding the OCR index: {e}")))?;
    rightkit_fs::atomic_write_with(
        path,
        rightkit_fs::AtomicOptions {
            create_parents: true,
            ..Default::default()
        },
        |file| file.write_all(&body),
    )
    .map_err(|e| ProjectError::io(format!("storing {}", path.display()), e.into()))?;
    Ok(())
}

fn ocr_options(model_dir: Option<PathBuf>) -> OcrOptions {
    let options = OcrOptions::new(BackendPreference::Auto);
    match model_dir {
        Some(dir) => options.with_onnx(OnnxConfig::new(dir, OcrArtifactManifest::pp_ocr_v6())),
        None => options,
    }
}

/// Extracts one frame every `interval` ms into `frames_dir` (one ffmpeg call), then reads
/// each frame in time order. Returns (frames sampled, frames with text, backend name).
fn sample_and_read(
    ffmpeg: &Path,
    media: &Path,
    frames_dir: &Path,
    interval: i64,
    ocr: &mut Ocr,
) -> Result<(usize, Vec<OcrFrame>, String), ProjectError> {
    let mut cmd = FfmpegCommand::default();
    let rate = format!("fps=1000/{interval}");
    cmd.texts(["-hide_banner", "-nostdin", "-y", "-loglevel", "error", "-i"]);
    cmd.path(media);
    // Frame n (from 0) is the frame at n * interval; PNG keeps the glyph edges for OCR.
    cmd.texts(["-an", "-vf", &rate, "-start_number", "0", "-f", "image2"]);
    cmd.path(frames_dir.join("frame-%06d.png"));
    run_ffmpeg(
        ffmpeg,
        &cmd,
        RunOptions {
            timeout: INDEX_TIMEOUT,
            cancel: Arc::new(AtomicBool::new(false)),
            on_progress: &mut |_| {},
        },
    )?;

    let listing = std::fs::read_dir(frames_dir)
        .map_err(io_err(format!("reading {}", frames_dir.display())))?;
    let mut files: Vec<PathBuf> = listing
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|x| x.to_str()) == Some("png"))
        .collect();
    files.sort();

    let mut frames = Vec::new();
    let mut backend = String::new();
    for (index, file) in files.iter().enumerate() {
        let result = ocr
            .recognize_file(file)
            .map_err(|e| bad(format!("OCR failed on frame {index}: {e:?}")))?;
        backend = format!("{:?}", result.backend);
        let text = result.text();
        if !text.trim().is_empty() {
            frames.push(OcrFrame {
                at_ms: index as i64 * interval,
                text,
            });
        }
    }
    Ok((files.len(), frames, backend))
}

pub fn m_moments_ocr_index(
    dir: &Path,
    params: &Value,
    env: &Env,
    dry_run: bool,
) -> Result<Value, ProjectError> {
    #[derive(Deserialize)]
    struct P {
        /// Source id or file name.
        source: String,
        /// Sampling interval; default 1000 ms.
        interval_ms: Option<i64>,
        /// Absolute ONNX OCR model directory.
        model_dir: Option<String>,
    }
    let q: P =
        serde_json::from_value(params.clone()).map_err(|e| bad(format!("invalid params: {e}")))?;
    let interval = q.interval_ms.unwrap_or(DEFAULT_INTERVAL_MS);
    if !(MIN_INTERVAL_MS..=MAX_INTERVAL_MS).contains(&interval) {
        return Err(bad(format!(
            "interval_ms must be {MIN_INTERVAL_MS}..={MAX_INTERVAL_MS}"
        )));
    }
    let model_dir = q.model_dir.map(PathBuf::from);
    if model_dir.as_ref().is_some_and(|m| !m.is_absolute()) {
        return Err(bad("OCR model_dir must be an absolute path"));
    }

    let p = Project::open(dir, Access::Read)?;
    let record = find_source(&p, &q.source)?.clone();
    let media = p.source_path(&record);
    if !media.is_file() {
        return Err(bad(format!("media is missing: {}", media.display())));
    }
    let hash = rightkit_fs::digest::blake3_file(&media)
        .map_err(io_err(format!("hashing {}", media.display())))?;
    let out = index_path(&p.dir, record.id.as_str());

    // Same bytes at the same interval: the stored index is still valid, so nothing reruns.
    if let Some(existing) = read_index(&out) {
        if existing.content_hash == hash && existing.interval_ms == interval {
            return Ok(json!({
                "status": "reused",
                "source": record.id.as_str(),
                "interval_ms": interval,
                "sampled": existing.sampled,
                "frames_with_text": existing.frames.len(),
                "index": out.display().to_string(),
            }));
        }
    }
    if dry_run {
        return Ok(json!({
            "dry_run": true,
            "would": {
                "command": "moments.ocr_index",
                "source": record.id.as_str(),
                "interval_ms": interval,
                "index": out.display().to_string(),
            },
        }));
    }

    let tools = media_tools(env)?;
    let mut ocr = Ocr::new(ocr_options(model_dir));
    let frames_dir = p.dir.join(".cache").join("ocr-frames").join(format!(
        "{}-{}",
        record.id.as_str(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    std::fs::create_dir_all(&frames_dir)
        .map_err(io_err(format!("creating {}", frames_dir.display())))?;
    let sampled = sample_and_read(&tools.ffmpeg, &media, &frames_dir, interval, &mut ocr);
    // Frames are scratch: they go whether or not OCR succeeded.
    let _ = std::fs::remove_dir_all(&frames_dir);
    let (sampled_count, frames, backend) = sampled?;

    let frames_with_text = frames.len();
    let index = OcrIndex {
        version: INDEX_VERSION,
        source: record.id.as_str().to_string(),
        content_hash: hash,
        interval_ms: interval,
        sampled: sampled_count,
        frames,
    };
    write_index(&out, &index)?;
    Ok(json!({
        "status": "ok",
        "source": record.id.as_str(),
        "interval_ms": interval,
        "backend": backend,
        "sampled": sampled_count,
        "frames_with_text": frames_with_text,
        "index": out.display().to_string(),
    }))
}

pub fn m_moments_search(dir: &Path, params: &Value) -> Result<Value, ProjectError> {
    #[derive(Deserialize)]
    struct P {
        query: String,
        /// Restrict to one source id or file name; default every source.
        source: Option<String>,
        limit: Option<usize>,
    }
    let q: P =
        serde_json::from_value(params.clone()).map_err(|e| bad(format!("invalid params: {e}")))?;
    let text = q.query.trim();
    if text.is_empty() {
        return Err(bad("empty query"));
    }
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let p = Project::open(dir, Access::Read)?;
    let ids: Vec<String> = match &q.source {
        Some(s) => vec![find_source(&p, s)?.id.as_str().to_string()],
        None => p
            .meta
            .sources
            .iter()
            .map(|r| r.id.as_str().to_string())
            .collect(),
    };

    let matcher = TextMatcher::new(&Query::literal(text))
        .map_err(|e| bad(format!("invalid query: {e:?}")))?;
    let mut hits = Vec::new();
    let mut unindexed = Vec::new();
    'sources: for id in &ids {
        let Some(index) = read_index(&index_path(&p.dir, id)) else {
            unindexed.push(id.clone());
            continue;
        };
        for frame in &index.frames {
            if matcher.matches_with(&[frame.text.as_str()], Vec::new) {
                hits.push(json!({
                    "source": id,
                    "at_ms": frame.at_ms,
                    "text": frame.text,
                }));
                if hits.len() >= limit {
                    break 'sources;
                }
            }
        }
    }
    Ok(json!({
        "status": "ok",
        "query": text,
        "hits": hits,
        "unindexed": unindexed,
    }))
}

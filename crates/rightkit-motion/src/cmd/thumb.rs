//! `media.thumbnail`: JPEG frames of a source or a render for the UI (cards,
//! drawers, timeline thumbnail strips). Frames are extracted through the typed
//! ffmpeg boundary and cached under `<project>/.cache/thumbs`, a rightkit-cache
//! `DiskCache` keyed by media identity, time and width, so repeated calls are free.

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use rightkit_cache::{Config as CacheConfig, DiskCache, Key as CacheKey, PutOutcome};
use rightkit_ffmpeg::FfmpegCommand;
use rightkit_video_project::worker::{run_ffmpeg, RunOptions};
use rightkit_video_project::{Access, Project, ProjectError};
use serde::Deserialize;
use serde_json::{json, Value};

use rightkit_engine_host::env::Env;
use rightkit_engine_host::project::{bad, find_source, io_err, media_tools};

/// Looks up the colour chain a project saved for a source: the ffmpeg filter and its signature, or
/// `None` to show the frame ungraded. The host supplies it (its colour command owns the cache).
pub type GradeLookup<'a> = &'a dyn Fn(&Project, &str) -> Option<(String, String)>;

/// Most frames one call may request (a timeline strip).
const MAX_FRAMES: usize = 64;

/// Thumbnail cache size cap. Frames regenerate, so eviction only costs a re-extract.
const THUMB_CACHE_MAX_BYTES: u64 = 256 * 1024 * 1024;

/// Temp files older than this are swept when the cache opens, even when their owner pid is alive.
/// A frame extract is killed after 30 s, so an older reservation is a crash leftover. Younger
/// temp files of live processes are another process's in-flight frame and are kept.
const THUMB_STALE_TMP_AFTER: Duration = Duration::from_secs(10 * 60);

/// Root of the thumbnail cache under a project directory. `storage.status` sizes and clears it.
pub use rightkit_engine_host::project::thumb_cache_root;

/// Key for one frame: grouped by media key, then time and width.
fn thumb_key(media_key: &str, at_ms: i64, width: u32) -> CacheKey {
    let at = at_ms.to_string();
    let width = width.to_string();
    CacheKey::grouped(&[media_key], &[at.as_str(), width.as_str()])
}

pub fn m_media_thumbnail(
    dir: &Path,
    params: &Value,
    env: &Env,
    dry_run: bool,
    grade: GradeLookup<'_>,
) -> Result<Value, ProjectError> {
    #[derive(Deserialize)]
    struct P {
        /// Source id or file name.
        source: Option<String>,
        /// Render file name inside `renders/` (e.g. `final.mp4`).
        render: Option<String>,
        /// One frame at this time; default 20% into the media.
        at_ms: Option<i64>,
        /// Several frames (timeline strip); overrides `at_ms`.
        times_ms: Option<Vec<i64>>,
        /// Output width in px (height keeps aspect); default 320.
        width: Option<u32>,
    }
    let q: P =
        serde_json::from_value(params.clone()).map_err(|e| bad(format!("invalid params: {e}")))?;
    let width = q.width.unwrap_or(320).clamp(32, 1920);
    let p = Project::open(dir, Access::Read)?;

    // A source frame shows the grade the render applies, once `colour.chain` has built it.
    let chain = q
        .source
        .as_deref()
        .and_then(|s| find_source(&p, s).ok())
        .and_then(|record| grade(&p, record.id.as_str()));
    let (key, media, duration_ms) = match (&q.source, &q.render) {
        (Some(s), None) => {
            let record = find_source(&p, s)?.clone();
            let media = p.source_path(&record);
            let duration_ms = p
                .timeline()
                .source(&record.id)
                .map(|i| i.duration.to_units(1000))
                .unwrap_or(0);
            let key = match &chain {
                Some((_, signature)) => format!("{}-g{}", record.id.as_str(), &signature[..16]),
                None => record.id.as_str().to_string(),
            };
            (key, media, duration_ms)
        }
        (None, Some(r)) => {
            if r.contains('/') || r.contains('\\') || r.contains("..") {
                return Err(bad("render must be a file name inside renders/"));
            }
            let media = p.dir.join("renders").join(r);
            if !media.is_file() {
                return Err(bad(format!("no render {r:?} in this project")));
            }
            let meta = std::fs::metadata(&media)
                .map_err(|e| ProjectError::io(format!("reading {}", media.display()), e))?;
            let stamp = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            let key = format!("render-{}-{}-{stamp}", r.replace('.', "_"), meta.len());
            (key, media, 0)
        }
        _ => return Err(bad("pass exactly one of source | render")),
    };
    if !media.is_file() {
        return Err(bad(format!("media is missing: {}", media.display())));
    }

    let times: Vec<i64> = match (q.times_ms, q.at_ms) {
        (Some(ts), _) => {
            if ts.is_empty() || ts.len() > MAX_FRAMES {
                return Err(bad(format!("times_ms needs 1..={MAX_FRAMES} entries")));
            }
            ts
        }
        (None, Some(t)) => vec![t],
        (None, None) => vec![duration_ms / 5],
    };
    if times.iter().any(|t| *t < 0) {
        return Err(bad("times must be >= 0"));
    }

    let cache_root = thumb_cache_root(&p.dir);
    if dry_run {
        // Read-only: count frames already cached, creating nothing.
        let cached = DiskCache::open_read_only(&cache_root)
            .map(|cache| {
                times
                    .iter()
                    .filter(|t| cache.contains(&thumb_key(&key, **t, width)))
                    .count()
            })
            .unwrap_or(0);
        return Ok(json!({
            "dry_run": true,
            "would": {"command": "media.thumbnail", "frames": times.len(), "width": width, "cached": cached},
        }));
    }
    let tools = media_tools(env)?;
    let config = CacheConfig::new(THUMB_CACHE_MAX_BYTES).stale_tmp_after(THUMB_STALE_TMP_AFTER);
    let cache = DiskCache::open(&cache_root, config)
        .map_err(io_err(format!("opening {}", cache_root.display())))?;
    let mut frames = Vec::with_capacity(times.len());
    for t in times {
        let slot = thumb_key(&key, t, width);
        let path = match cache.get_path(&slot) {
            Some(path) => path,
            None => {
                // ffmpeg writes into the reservation; a failed extract drops it and leaves no entry.
                let reservation = cache.reserve_path(&slot, "jpg").map_err(io_err(format!(
                    "reserving a frame in {}",
                    cache_root.display()
                )))?;
                extract(
                    &tools.ffmpeg,
                    &media,
                    t,
                    width,
                    chain.as_ref().map(|(filter, _)| filter.as_str()),
                    reservation.path(),
                )?;
                match reservation.commit().map_err(io_err(format!(
                    "storing a frame in {}",
                    cache_root.display()
                )))? {
                    PutOutcome::Stored => {}
                    PutOutcome::TooLarge => {
                        return Err(bad(format!(
                            "frame at {t} ms is larger than the thumbnail cache allows"
                        )))
                    }
                }
                cache.get_path(&slot).ok_or_else(|| {
                    ProjectError::io(
                        format!("storing a frame in {}", cache_root.display()),
                        std::io::Error::other("entry missing after commit"),
                    )
                })?
            }
        };
        frames.push(json!({"at_ms": t, "path": path.display().to_string()}));
    }
    Ok(json!({"status": "ok", "width": width, "frames": frames}))
}

fn extract(
    ffmpeg: &Path,
    media: &Path,
    at_ms: i64,
    width: u32,
    grade: Option<&str>,
    out: &Path,
) -> Result<(), ProjectError> {
    let mut cmd = FfmpegCommand::default();
    let ss = format!("{}.{:03}", at_ms / 1000, at_ms % 1000);
    let vf = match grade {
        Some(chain) => format!("{chain},scale={width}:-2"),
        None => format!("scale={width}:-2"),
    };
    cmd.texts([
        "-hide_banner",
        "-nostdin",
        "-y",
        "-loglevel",
        "error",
        "-ss",
        &ss,
        "-i",
    ]);
    cmd.path(media);
    // `-update 1` writes a single image; past the end ffmpeg emits nothing,
    // which is reported below rather than committing an empty entry.
    cmd.texts([
        "-an",
        "-frames:v",
        "1",
        "-vf",
        &vf,
        "-q:v",
        "4",
        "-update",
        "1",
        "-f",
        "image2",
    ]);
    cmd.path(out);
    run_ffmpeg(
        ffmpeg,
        &cmd,
        RunOptions {
            timeout: Duration::from_secs(30),
            cancel: Arc::new(AtomicBool::new(false)),
            on_progress: &mut |_| {},
        },
    )?;
    if !out.is_file() {
        return Err(bad(format!(
            "no frame at {at_ms} ms in {}",
            media.display()
        )));
    }
    Ok(())
}

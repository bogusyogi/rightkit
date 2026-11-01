//! `contactsheet.make {dir, revision, every_s | count, render?, width?, dry_run?}`: a grid PNG of
//! programme frames for agents to review without a player, bound to one timeline revision.
//!
//! The frames come from the render recorded for that revision (`renders/<name>.render.json`
//! whose `origin.timeline_head_hash` equals `revision`), extracted through the cached typed
//! FFmpeg helper behind `media.thumbnail`, then tiled with ffmpeg's `tile` filter. A `revision`
//! that is not the project's current revision is refused as `stale_revision`.
//!
//! Output, under `analysis/contactsheets/`: `<name>.png` and a `<name>.json` sidecar
//! (`cutright.contactsheet.v1`) binding revision, timeline hash, render, frame times and PNG hash.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use rightkit_ffmpeg::FfmpegCommand;
use rightkit_video_project::worker::{run_ffmpeg, RunOptions};
use rightkit_video_project::{Access, Project, ProjectError};
use serde::Deserialize;
use serde_json::{json, Value};

use rightkit_engine_host::env::Env;
use rightkit_engine_host::project::{bad, io_err, media_tools};

use super::thumb::m_media_thumbnail;

/// Most tiles one sheet may hold.
const MAX_TILES: usize = 64;

/// Tile width in px when the caller gives none; height keeps the programme's aspect.
const DEFAULT_WIDTH: u32 = 320;

/// Gap in px between tiles.
const TILE_PADDING: u32 = 4;

/// Schema of the JSON sidecar written next to each sheet.
pub const SHEET_SCHEMA: &str = "cutright.contactsheet.v1";

pub fn m_contactsheet_make(
    dir: &Path,
    params: &Value,
    env: &Env,
    dry_run: bool,
) -> Result<Value, ProjectError> {
    #[derive(Deserialize)]
    struct P {
        revision: String,
        every_s: Option<f64>,
        count: Option<usize>,
        render: Option<String>,
        width: Option<u32>,
    }
    let q: P =
        serde_json::from_value(params.clone()).map_err(|e| bad(format!("invalid params: {e}")))?;
    let width = q.width.unwrap_or(DEFAULT_WIDTH).clamp(32, 640);
    let p = Project::open(dir, Access::Read)?;

    let head = p.revision_summary()["head_hash"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    if q.revision != head {
        return Err(bad(format!(
            "stale_revision: {} is not the current revision {head}; request a sheet for the current revision",
            q.revision
        )));
    }
    let cursor = p.cursor();

    let renders = p.dir.join("renders");
    let (render_name, record) = bound_render(&renders, q.render.as_deref(), &head)?;
    let duration_ms = record["duration_ms"]
        .as_i64()
        .filter(|d| *d > 0)
        .ok_or_else(|| bad(format!("render {render_name} records no duration")))?;
    let times = frame_times(duration_ms, q.every_s, q.count)?;

    if dry_run {
        return Ok(json!({
            "dry_run": true,
            "would": {
                "command": "contactsheet.make",
                "revision": head,
                "render": render_name,
                "frame_times_ms": times,
                "width": width,
            },
        }));
    }

    // Frames reuse the cached typed-FFmpeg extraction of `media.thumbnail`.
    let frames = m_media_thumbnail(
        &p.dir,
        &json!({"render": render_name, "times_ms": times, "width": width}),
        env,
        false,
        &|_, _| None,
    )?;
    let paths: Vec<PathBuf> = frames["frames"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| f["path"].as_str())
        .map(PathBuf::from)
        .collect();
    if paths.len() != times.len() {
        return Err(bad(format!(
            "expected {} frames from {render_name}, extracted {}",
            times.len(),
            paths.len()
        )));
    }

    let tiles = paths.len();
    let columns = ((tiles as f64).sqrt().ceil() as usize).clamp(1, 8);
    let rows = tiles.div_ceil(columns);

    let spec = json!({
        "revision": head,
        "render": render_name,
        "frame_times_ms": times,
        "width": width,
        "columns": columns,
    });
    let key = rightkit_fs::digest::blake3_bytes(&serde_json::to_vec(&spec).unwrap_or_default());
    let name = format!(
        "contactsheet-{}-{}",
        &head[..head.len().min(12)],
        &key[..12]
    );

    let out_dir = p.dir.join("analysis").join("contactsheets");
    std::fs::create_dir_all(&out_dir).map_err(io_err(format!("creating {}", out_dir.display())))?;
    let list = out_dir.join(format!(".{name}.frames.txt"));
    let list_body: String = paths
        .iter()
        .map(|f| {
            format!(
                "file '{}'\n",
                f.display().to_string().replace('\'', "'\\''")
            )
        })
        .collect();
    rightkit_fs::atomic_write(&list, list_body.as_bytes())
        .map_err(|e| ProjectError::io(format!("writing {}", list.display()), e.into()))?;

    let png = out_dir.join(format!("{name}.png"));
    let partial = out_dir.join(format!(".{name}.partial.png"));
    let tools = media_tools(env)?;
    let mut cmd = FfmpegCommand::default();
    for item in [
        "-hide_banner",
        "-nostdin",
        "-y",
        "-loglevel",
        "error",
        "-f",
        "concat",
        "-safe",
        "0",
        "-i",
    ] {
        cmd.text(item);
    }
    cmd.path(&list);
    let vf = format!("tile={columns}x{rows}:padding={TILE_PADDING}:margin=0:color=black");
    for item in ["-vf", vf.as_str(), "-frames:v", "1"] {
        cmd.text(item);
    }
    cmd.path(&partial);
    let ran = run_ffmpeg(
        &tools.ffmpeg,
        &cmd,
        RunOptions {
            timeout: Duration::from_secs(120),
            cancel: Arc::new(AtomicBool::new(false)),
            on_progress: &mut |_| {},
        },
    );
    let _ = std::fs::remove_file(&list);
    ran?;
    if !partial.is_file() {
        return Err(bad(format!(
            "ffmpeg wrote no contact sheet for {render_name}"
        )));
    }
    let bytes =
        std::fs::read(&partial).map_err(io_err(format!("reading {}", partial.display())))?;
    let Some((width_px, height_px)) = png_size(&bytes) else {
        let _ = std::fs::remove_file(&partial);
        return Err(bad("the contact sheet output is not a PNG"));
    };
    rightkit_fs::publish_file(&partial, &png)
        .map_err(|e| ProjectError::io(format!("storing {}", png.display()), e.into()))?;
    let png_hash = rightkit_fs::digest::blake3_bytes(&bytes);

    let sidecar = json!({
        "schema": SHEET_SCHEMA,
        "name": name,
        "revision": head,
        "timeline_hash": head,
        "cursor": cursor,
        "render": {
            "file": render_name,
            "output_blake3": record["output_blake3"],
            "timeline_head_hash": head,
        },
        "duration_ms": duration_ms,
        "sampling": if q.every_s.is_some() { "every_s" } else { "count" },
        "every_s": q.every_s,
        "count": q.count,
        "frame_times_ms": times,
        "tiles": tiles,
        "columns": columns,
        "rows": rows,
        "tile_width_px": width,
        "padding_px": TILE_PADDING,
        "png": {
            "file": format!("{name}.png"),
            "bytes": bytes.len(),
            "blake3": png_hash,
            "width_px": width_px,
            "height_px": height_px,
        },
        "created_at": chrono::Utc::now().to_rfc3339(),
    });
    let side_path = out_dir.join(format!("{name}.json"));
    let side_bytes =
        serde_json::to_vec_pretty(&sidecar).map_err(|e| bad(format!("sidecar: {e}")))?;
    rightkit_fs::atomic_write(&side_path, &side_bytes)
        .map_err(|e| ProjectError::io(format!("storing {}", side_path.display()), e.into()))?;

    Ok(json!({
        "status": "ok",
        "name": name,
        "revision": head,
        "render": render_name,
        "png": png.display().to_string(),
        "sidecar": side_path.display().to_string(),
        "png_blake3": png_hash,
        "tiles": tiles,
        "columns": columns,
        "rows": rows,
        "width_px": width_px,
        "height_px": height_px,
        "frame_times_ms": times,
    }))
}

/// Finds the render bound to `head` (the named one, else the newest matching one) and checks
/// its output still matches the hash recorded when it was rendered.
fn bound_render(
    renders: &Path,
    want: Option<&str>,
    head: &str,
) -> Result<(String, Value), ProjectError> {
    let (name, record) = match want {
        Some(name) => {
            if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
                return Err(bad("render must be a file name inside renders/"));
            }
            let record_path = renders.join(format!("{name}.render.json"));
            if !record_path.is_file() {
                return Err(bad(format!("no render record for {name}; render it first")));
            }
            (name.to_string(), read_json(&record_path)?)
        }
        None => {
            let mut matches: Vec<(String, String, Value)> = Vec::new();
            if renders.is_dir() {
                for entry in std::fs::read_dir(renders)
                    .map_err(io_err("reading renders".into()))?
                    .flatten()
                {
                    let file = entry.file_name().to_string_lossy().into_owned();
                    let Some(stem) = file.strip_suffix(".render.json") else {
                        continue;
                    };
                    let record = read_json(&entry.path())?;
                    if record["origin"]["timeline_head_hash"].as_str() != Some(head) {
                        continue;
                    }
                    let created = record["created_at"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    matches.push((created, stem.to_string(), record));
                }
            }
            matches.sort_by(|a, b| (a.0.as_str(), a.1.as_str()).cmp(&(b.0.as_str(), b.1.as_str())));
            let (_, stem, record) = matches.pop().ok_or_else(|| {
                bad(format!(
                    "no render of revision {head}; render this revision first"
                ))
            })?;
            (stem, record)
        }
    };
    let made = record["origin"]["timeline_head_hash"]
        .as_str()
        .unwrap_or_default();
    if made != head {
        return Err(bad(format!(
            "render {name} was made from revision {made}, not {head}; render this revision first"
        )));
    }
    let output = renders.join(&name);
    if !output.is_file() {
        return Err(bad(format!("render {name} is missing from renders/")));
    }
    let got = rightkit_fs::digest::blake3_file(&output)
        .map_err(io_err(format!("hashing {}", output.display())))?;
    if record["output_blake3"].as_str() != Some(got.as_str()) {
        return Err(bad(format!(
            "render {name} changed after it was recorded; render this revision again"
        )));
    }
    Ok((name, record))
}

fn read_json(path: &Path) -> Result<Value, ProjectError> {
    let bytes = std::fs::read(path).map_err(io_err(format!("reading {}", path.display())))?;
    serde_json::from_slice(&bytes)
        .map_err(|e| ProjectError::Corrupt(format!("{}: {e}", path.display())))
}

/// Frame times in ms: `every_s` steps from 0, or `count` midpoints of equal segments.
fn frame_times(
    duration_ms: i64,
    every_s: Option<f64>,
    count: Option<usize>,
) -> Result<Vec<i64>, ProjectError> {
    let times: Vec<i64> = match (every_s, count) {
        (Some(every), None) => {
            if !every.is_finite() || every <= 0.0 {
                return Err(bad("every_s must be greater than 0"));
            }
            let step = (every * 1000.0).round() as i64;
            if step <= 0 {
                return Err(bad("every_s is below 1 ms"));
            }
            (0i64..)
                .map(|i| i * step)
                .take_while(|t| *t < duration_ms)
                .take(MAX_TILES + 1)
                .collect()
        }
        (None, Some(n)) => {
            if n == 0 || n > MAX_TILES {
                return Err(bad(format!("count must be 1..={MAX_TILES}")));
            }
            let n = n as i64;
            (0..n)
                .map(|i| duration_ms * (2 * i + 1) / (2 * n))
                .collect()
        }
        _ => return Err(bad("pass exactly one of every_s | count")),
    };
    if times.is_empty() || times.len() > MAX_TILES {
        return Err(bad(format!(
            "sampling gives {} frames; allowed 1..={MAX_TILES}: raise every_s or pass count",
            times.len()
        )));
    }
    Ok(times)
}

/// Width and height from a PNG's IHDR chunk.
fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || bytes[..8] != *b"\x89PNG\r\n\x1a\n" || bytes[12..16] != *b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    Some((w, h))
}

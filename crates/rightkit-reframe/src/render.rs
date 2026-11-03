//! Approval-gated 9:16 render: ffmpeg `crop` driven by per-frame `sendcmd` positions
//! interpolated from the plan's keyframes, then scaled to the delivery size.

use crate::error::{ReframeError, Result};
use crate::media::{blake3_file, probe, run_error, Tools};
use crate::plan::{PlanMode, ReframePlan};
use rightkit_ffmpeg::runner::run_capture_with;
use rightkit_ffmpeg::{CancellationToken, FfmpegCommand, RunOptions};
use rightkit_fs::{atomic_write, publish_file};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

pub const RECEIPT_SCHEMA: &str = "cutright.reframe.render-receipt.v1";
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct RenderOptions {
    /// Output frame rate for the interpolated path (default: the source's).
    pub fps: Option<f64>,
    pub video_args: Vec<String>,
    pub audio_args: Vec<String>,
    /// Re-hash the source and refuse if it changed since analysis (locked invariant).
    pub verify_source: bool,
}

/// The host system's own H.264 encoder at high quality, with no check that this ffmpeg
/// lists it. The engine replaces these with the encoder its selection point resolves.
fn host_video_args() -> &'static [&'static str] {
    if cfg!(target_os = "macos") {
        &[
            "-c:v",
            "h264_videotoolbox",
            "-profile:v",
            "high",
            "-q:v",
            "80",
        ]
    } else {
        &[
            "-c:v",
            "h264_mf",
            "-rate_control",
            "quality",
            "-quality",
            "95",
        ]
    }
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            fps: None,
            video_args: host_video_args().iter().map(|a| a.to_string()).collect(),
            audio_args: ["-c:a", "copy"].map(String::from).to_vec(),
            verify_source: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderReceipt {
    pub schema: String,
    pub plan_hash: String,
    pub source_blake3: String,
    pub approver: String,
    pub approved_at: String,
    pub mode: PlanMode,
    pub output: String,
    pub output_blake3: String,
    pub output_width: u32,
    pub output_height: u32,
    pub fps: f64,
    pub crop_commands: usize,
    /// True when an identical earlier render was found and reused (idempotent retry).
    #[serde(default)]
    pub reused: bool,
}

/// Crop positions at output-frame times: `(t_seconds, x, y)`, emitted only when the integer
/// position changes. Linear between keyframes, held across a cut.
pub fn crop_commands(plan: &ReframePlan, fps: f64) -> Vec<(f64, u32, u32)> {
    let ks = &plan.keyframes;
    let mut out = Vec::new();
    if ks.is_empty() {
        return out;
    }
    let frames = ((plan.duration_ms as f64 / 1000.0) * fps).ceil() as u64;
    let mut last: Option<(u32, u32)> = None;
    let mut seg = 0usize;
    for i in 0..frames {
        let t_ms = i as f64 * 1000.0 / fps;
        while seg + 1 < ks.len() && (ks[seg + 1].t_ms as f64) <= t_ms {
            seg += 1;
        }
        let a = &ks[seg];
        let (x, y) = match ks.get(seg + 1) {
            Some(b) if t_ms > a.t_ms as f64 && !b.shot_boundary => {
                let f = ((t_ms - a.t_ms as f64) / (b.t_ms - a.t_ms).max(1) as f64).clamp(0.0, 1.0);
                (
                    a.x as f64 + (b.x as f64 - a.x as f64) * f,
                    a.y as f64 + (b.y as f64 - a.y as f64) * f,
                )
            }
            _ => (a.x as f64, a.y as f64),
        };
        let max_x = (plan.source_width - plan.crop_width) as f64;
        let max_y = (plan.source_height - plan.crop_height) as f64;
        let ev = |v: f64, max: f64| ((((v.clamp(0.0, max)) / 2.0).round() as i64) * 2) as u32;
        let pos = (
            ev(x, max_x).min(max_x as u32),
            ev(y, max_y).min(max_y as u32),
        );
        if last != Some(pos) {
            out.push((t_ms / 1000.0, pos.0, pos.1));
            last = Some(pos);
        }
    }
    out
}

pub fn command_file_text(cmds: &[(f64, u32, u32)]) -> String {
    let mut s = String::new();
    for (t, x, y) in cmds {
        s.push_str(&format!("{t:.4} crop@rf x {x}, crop@rf y {y};\n"));
    }
    s
}

/// The video filter chain (`sendcmd` only when the path actually moves).
pub fn filter_chain(plan: &ReframePlan, cmd_file_name: Option<&str>, x0: u32, y0: u32) -> String {
    let mut chain = Vec::new();
    if let Some(name) = cmd_file_name {
        chain.push(format!("sendcmd=f={name}"));
    }
    chain.push(format!(
        "crop@rf=w={}:h={}:x={x0}:y={y0}",
        plan.crop_width, plan.crop_height
    ));
    chain.push(format!(
        "scale={}:{}:flags=bicubic",
        plan.output_width, plan.output_height
    ));
    chain.push("setsar=1".into());
    chain.push("format=yuv420p".into());
    chain.join(",")
}

fn receipt_path(out: &Path) -> PathBuf {
    let mut p = out.as_os_str().to_owned();
    p.push(".reframe-receipt.json");
    PathBuf::from(p)
}

/// Render the 9:16 delivery. Refuses unless the plan is approved (plan + every anchor, hash
/// intact) and the source is still the media that was analysed. Idempotent: an identical
/// existing output (same plan hash, source and output bytes) is reused.
pub fn render(
    tools: &Tools,
    source: &Path,
    plan: &ReframePlan,
    out: &Path,
    opts: &RenderOptions,
) -> Result<RenderReceipt> {
    plan.require_approved()?;
    let approval = plan.approval.as_ref().expect("checked by require_approved");
    let plan_hash = plan.plan_hash();
    if opts.verify_source {
        let found = blake3_file(source)?;
        if found != plan.source_blake3 {
            return Err(ReframeError::SourceChanged {
                expected: plan.source_blake3.clone(),
                found,
            });
        }
    }
    let info = probe(tools, source)?;
    if (info.width, info.height) != (plan.source_width, plan.source_height) {
        return Err(ReframeError::Invalid(format!(
            "source is {}x{} but the plan was made for {}x{}",
            info.width, info.height, plan.source_width, plan.source_height
        )));
    }
    let fps = opts.fps.unwrap_or(info.fps).clamp(1.0, 240.0);

    // Idempotent retry.
    let rpath = receipt_path(out);
    if out.is_file() {
        if let Ok(text) = std::fs::read_to_string(&rpath) {
            if let Ok(mut prev) = serde_json::from_str::<RenderReceipt>(&text) {
                if prev.plan_hash == plan_hash
                    && prev.source_blake3 == plan.source_blake3
                    && (prev.fps - fps).abs() < 1e-9
                    && blake3_file(out)? == prev.output_blake3
                {
                    prev.reused = true;
                    return Ok(prev);
                }
            }
        }
    }

    let dir = out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&dir)?;
    rightkit_fs::sweep_stale_temps(&dir, Duration::ZERO).map_err(std::io::Error::from)?;
    let stem = out
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("reframe");
    let ext = out
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("mp4")
        .to_ascii_lowercase();
    let muxer = match ext.as_str() {
        "mkv" => "matroska",
        "m4v" => "mp4",
        ext => ext,
    };
    let leaf = out
        .file_name()
        .ok_or_else(|| ReframeError::Invalid("output needs a file name".into()))?;
    let mut temp_name = std::ffi::OsString::from(".");
    temp_name.push(leaf);
    temp_name.push(format!(
        ".rktmp-{}-{}-0",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let part = dir.join(temp_name);
    let cmd_name = format!("{stem}.reframe.cmd");
    let cmd_path = dir.join(&cmd_name);

    let cmds = crop_commands(plan, fps);
    let (x0, y0) = cmds
        .first()
        .map(|c| (c.1, c.2))
        .unwrap_or_else(|| (plan.keyframes[0].x, plan.keyframes[0].y));
    let moving = cmds.len() > 1;
    if moving {
        atomic_write(&cmd_path, command_file_text(&cmds).as_bytes())
            .map_err(std::io::Error::from)?;
    }
    let filter = filter_chain(plan, moving.then_some(cmd_name.as_str()), x0, y0);

    let mut cmd = FfmpegCommand::new();
    cmd.texts(["-v", "error", "-nostdin", "-y", "-i"])
        .path(source)
        .texts(["-map", "0:v:0", "-map", "0:a?", "-vf"])
        .text(filter)
        .texts(opts.video_args.iter().cloned())
        .texts(opts.audio_args.iter().cloned())
        .texts(["-movflags", "+faststart", "-f", muxer])
        .path(&part);
    // The child runs in `dir`, which keeps the sendcmd path relative: no drive-letter escaping
    // on Windows. Stdin is null and both streams are captured, as with `Command::output`.
    let run = RunOptions::unbounded(CancellationToken::new())
        .with_kill_grace(Duration::ZERO)
        .with_current_dir(&dir);
    let result = rightkit_ffmpeg::verify_ffmpeg_handle(
        &tools.ffmpeg,
        rightkit_ffmpeg::EXPORT_ASS_ZIMG_VARIANT,
        Duration::from_secs(30),
    )
    .or_else(|_| {
        rightkit_ffmpeg::verify_ffmpeg_handle(
            &tools.ffmpeg,
            rightkit_ffmpeg::EXPORT_ASS_VARIANT,
            Duration::from_secs(30),
        )
    })
    .map_err(|e| ReframeError::Ffmpeg(e.to_string()))
    .and_then(|binary| run_capture_with(&binary, &cmd, run).map_err(run_error));
    let _ = std::fs::remove_file(&cmd_path);
    let output = result?;
    if !output.status.success() {
        let _ = std::fs::remove_file(&part);
        return Err(ReframeError::Ffmpeg(output.stderr.trim().to_string()));
    }
    publish_file(&part, out).map_err(std::io::Error::from)?;

    let receipt = RenderReceipt {
        schema: RECEIPT_SCHEMA.into(),
        plan_hash,
        source_blake3: plan.source_blake3.clone(),
        approver: approval.approver.clone(),
        approved_at: approval.approved_at.clone(),
        mode: plan.mode,
        output: out.display().to_string(),
        output_blake3: blake3_file(out)?,
        output_width: plan.output_width,
        output_height: plan.output_height,
        fps,
        crop_commands: cmds.len(),
        reused: false,
    };
    atomic_write(&rpath, &serde_json::to_vec_pretty(&receipt)?).map_err(std::io::Error::from)?;
    Ok(receipt)
}

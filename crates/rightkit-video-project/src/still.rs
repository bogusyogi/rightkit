//! Still images as timeline sources. The timeline has no still-image source kind
//! yet, so a still is materialised once into an N-frame H.264 clip under `media/`
//! (ffmpeg `-loop 1 -frames:v N`) and registered like any other video source.
//! Decided (decisions.md 2026-10-16, #25): no native still source; the materialised,
//! BLAKE3-registered clip is the still path.

use std::fs;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use rightkit_ffmpeg::encoders::IntermediateFormat;
use rightkit_ffmpeg::FfmpegCommand;
use rightkit_timeline::Fps;

use crate::error::ProjectError;
use crate::tools::MediaTools;
use crate::worker::{run_ffmpeg, RunOptions};

/// Renders `image` into `out` as exactly `frames` frames at `fps`. Writes through a
/// sweepable RightKit temp file and publishes, so `out` is complete or absent.
pub fn still_to_clip(
    tools: &MediaTools,
    image: &Path,
    out: &Path,
    fps: Fps,
    frames: i64,
    cancel: Arc<AtomicBool>,
) -> Result<(), ProjectError> {
    if frames < 1 {
        return Err(ProjectError::BadInput(
            "a still needs at least 1 frame".into(),
        ));
    }
    let dir = out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    rightkit_fs::sweep_stale_temps(dir, Duration::ZERO)
        .map_err(|e| ProjectError::io(format!("sweeping {}", dir.display()), e.into()))?;
    let encode = tools.intermediate(IntermediateFormat::Yuv420p8)?;
    let partial = crate::fsutil::ffmpeg_temp_path(out)?;
    let rate = fps.ffmpeg_rational();
    let mut cmd = FfmpegCommand::default();
    cmd.texts([
        "-hide_banner",
        "-nostdin",
        "-y",
        "-loglevel",
        "error",
        "-loop",
        "1",
        "-framerate",
        &rate,
        "-i",
    ]);
    cmd.path(image);
    let n = frames.to_string();
    cmd.texts([
        "-vf",
        "scale=trunc(iw/2)*2:trunc(ih/2)*2,setsar=1,format=yuv420p",
        "-r",
        &rate,
        "-frames:v",
        &n,
        "-an",
    ]);
    cmd.texts(encode.args.iter().copied());
    cmd.texts(["-f", "mp4"]);
    cmd.path(partial.clone());
    let result = run_ffmpeg(
        &tools.ffmpeg,
        &cmd,
        RunOptions {
            timeout: Duration::from_secs(10 * 60),
            cancel,
            on_progress: &mut |_| {},
        },
    );
    match result {
        Ok(_) => rightkit_fs::publish_file(&partial, out).map_err(|e| {
            ProjectError::io(
                format!("renaming {}", out.display()),
                std::io::Error::other(e),
            )
        }),
        Err(e) => {
            let _ = fs::remove_file(&partial);
            Err(e)
        }
    }
}

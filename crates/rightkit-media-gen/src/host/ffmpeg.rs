//! FFmpeg/ffprobe discovery on `rightkit_ffmpeg::resolve_ffmpeg_handle` / `resolve_ffprobe_handle`.
//!
//! One call per tool: the bundled Tauri `externalBin` (beside the app executable, or the dev
//! checkout's `src-tauri/binaries/<tool>-<target-triple>`) is verified against the embedded
//! RightKit catalog (ffmpeg `export-ass`, or `export-ass-zimg` on Windows; ffprobe `decode`); with no bundled file, development
//! builds only (`dev-overrides`) fall back to the `<PREFIX>_FFMPEG` / `<PREFIX>_FFPROBE`
//! override, then `PATH`, labelled UNVERIFIED. Release builds never honour either.
//!
//! The owner's refusal is final: a host with no approved catalog row, a pending row, a digest
//! mismatch or any other verification failure returns the `MediaError` to the caller. This module
//! never substitutes a file it found by path.

use std::time::Duration;

use rightkit_ffmpeg::{
    resolve_ffmpeg_handle, resolve_ffprobe_handle, BuildMode, UnverifiedRequest,
};
pub use rightkit_ffmpeg::{
    FfmpegBinary, FfmpegHandle, FfprobeHandle, MediaError, Tool, ToolHandle,
};

/// The owner's trust class of a resolved tool, for status output: `verified` (matched its approved
/// catalog row) or `development` (a development build's override or PATH binary the catalog does
/// not list). The handle itself travels to every spawn site; nothing here converts it to a path.
pub fn trust<V>(handle: &ToolHandle<V>) -> &'static str {
    if handle.is_verified() {
        "verified"
    } else {
        "development"
    }
}

fn request(
    tool: Tool,
) -> Result<(std::path::PathBuf, &'static str, UnverifiedRequest), MediaError> {
    let exe = std::env::current_exe().map_err(MediaError::Io)?;
    let (variant, env_var) = match tool {
        // Windows ships the export-ass-zimg build (libass, drawtext, zscale); macOS ships export-ass.
        Tool::Ffmpeg if cfg!(windows) => ("export-ass-zimg", "FFMPEG"),
        Tool::Ffmpeg => ("export-ass", "FFMPEG"),
        Tool::Ffprobe => ("decode", "FFPROBE"),
    };
    let mut dev = UnverifiedRequest::new(tool);
    dev.env_var = Some(rightkit_local_gen::host::env_name(env_var));
    dev.allow_path_lookup = true;
    Ok((exe, variant, dev))
}

const TIMEOUT: Duration = Duration::from_secs(20);

/// Resolve ffmpeg through the owner. Any `MediaError` (no approved row, pending row, digest
/// mismatch, nothing found) is returned unchanged; callers report it.
pub fn resolve_ffmpeg() -> Result<FfmpegHandle, MediaError> {
    let (exe, variant, dev) = request(Tool::Ffmpeg)?;
    resolve_ffmpeg_handle(
        &exe,
        variant,
        &dev,
        BuildMode::from_build_features(),
        TIMEOUT,
    )
}

/// Resolve ffprobe through the owner (same contract as [`resolve_ffmpeg`]).
pub fn resolve_ffprobe() -> Result<FfprobeHandle, MediaError> {
    let (exe, variant, dev) = request(Tool::Ffprobe)?;
    resolve_ffprobe_handle(
        &exe,
        variant,
        &dev,
        BuildMode::from_build_features(),
        TIMEOUT,
    )
}

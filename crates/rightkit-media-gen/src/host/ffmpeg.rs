//! FFmpeg/ffprobe discovery on `rightkit_ffmpeg::resolve_ffmpeg_handle` / `resolve_ffprobe_handle`.
//!
//! One call per tool: the bundled Tauri `externalBin` (beside the app executable, or the dev
//! checkout's `src-tauri/binaries/<tool>-<target-triple>`) is verified against the embedded
//! RightKit catalog (ffmpeg `export-ass`, or `export-ass-zimg` on Windows; ffprobe `decode`). No build
//! honours a `<PREFIX>_FFMPEG` override or `PATH`.
//!
//! The owner's refusal is final: a host with no approved catalog row, a pending row, a digest
//! mismatch or any other verification failure returns the `MediaError` to the caller. This module
//! never substitutes a file it found by path.

use std::time::Duration;

use rightkit_ffmpeg::{resolve_ffmpeg_handle, resolve_ffprobe_handle};
pub use rightkit_ffmpeg::{
    FfmpegBinary, FfmpegHandle, FfprobeHandle, MediaError, Tool, ToolHandle,
};

/// The owner's trust class of a resolved tool, for status output: always `verified` (matched its
/// approved catalog row). The handle itself travels to every spawn site; nothing here converts it
/// to a path.
pub fn trust<V>(_handle: &ToolHandle<V>) -> &'static str {
    "verified"
}

fn request(tool: Tool) -> Result<(std::path::PathBuf, &'static str), MediaError> {
    let exe = std::env::current_exe().map_err(MediaError::Io)?;
    let variant = match tool {
        // Windows ships the export-ass-zimg build (libass, drawtext, zscale); macOS ships export-ass.
        Tool::Ffmpeg if cfg!(windows) => "export-ass-zimg",
        Tool::Ffmpeg => "export-ass",
        Tool::Ffprobe => "decode",
    };
    Ok((exe, variant))
}

const TIMEOUT: Duration = Duration::from_secs(20);

/// Resolve ffmpeg through the owner. Any `MediaError` (no approved row, pending row, digest
/// mismatch, nothing found) is returned unchanged; callers report it.
pub fn resolve_ffmpeg() -> Result<FfmpegHandle, MediaError> {
    let (exe, variant) = request(Tool::Ffmpeg)?;
    resolve_ffmpeg_handle(&exe, variant, TIMEOUT)
}

/// Resolve ffprobe through the owner (same contract as [`resolve_ffmpeg`]).
pub fn resolve_ffprobe() -> Result<FfprobeHandle, MediaError> {
    let (exe, variant) = request(Tool::Ffprobe)?;
    resolve_ffprobe_handle(&exe, variant, TIMEOUT)
}

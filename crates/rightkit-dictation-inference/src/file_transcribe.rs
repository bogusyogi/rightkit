//! Offline file transcription decode path for the sidecar.
//!
//! The shell chooses a file; the sidecar owns media decoding and ASR. ffmpeg is
//! resolved from an explicit bundled path or packaged runtime and must verify against the catalog.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use rightkit_ffmpeg::decode::{decode_pcm_s16, DecodeOptions};
use rightkit_ffmpeg::probe::{probe_media_opt_with, ProbeError};
use rightkit_ffmpeg::runner::RunError;
use rightkit_ffmpeg::{
    resolve_ffmpeg as resolve_catalog_ffmpeg, verify_ffmpeg, ArtifactCatalog, CancellationToken,
    FfmpegBinary, MediaError, ResolutionRequest, VerifiedFfmpeg,
};

/// Typed prefix for a privacy-denied read. Hosts match on it to offer the
/// matching Settings pane; the service maps it to error code `E_PERMISSION`.
pub const E_PERMISSION: &str = "E_PERMISSION";

static APP_DISPLAY_NAME: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();

/// Name the app in user-facing file errors (the name shown in macOS privacy settings). Call once
/// at startup; until then the messages say "this app". Returns false when already set.
pub fn configure_app_display_name(name: &'static str) -> bool {
    APP_DISPLAY_NAME.set(name).is_ok()
}

fn app_name() -> &'static str {
    APP_DISPLAY_NAME.get().copied().unwrap_or("this app")
}

/// The grant a denied read needs. macOS gates removable/external volumes
/// behind Files & Folders → Removable Volumes; anything else falls to Full
/// Disk Access.
pub fn permission_error(input: &Path) -> String {
    let app = app_name();
    let grant = if cfg!(target_os = "macos") && input.starts_with("/Volumes") {
        format!("System Settings → Privacy & Security → Files & Folders → {app} → Removable Volumes (or Full Disk Access)")
    } else if cfg!(target_os = "macos") {
        format!("System Settings → Privacy & Security → Full Disk Access → {app}")
    } else {
        format!("read access to this file for {app}")
    };
    format!(
        "{E_PERMISSION}: {app} is not allowed to read {}. Grant {grant}, then try again.",
        input.display()
    )
}

fn is_permission_denied(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::PermissionDenied || error.raw_os_error() == Some(1)
}

/// Open and read one byte in THIS process before ffmpeg runs. A child's
/// denied read never raises the TCC prompt for the app; our own open does, and
/// a denial becomes a typed error instead of ffmpeg's bare EPERM.
pub fn preflight_read(input: &Path) -> Result<(), String> {
    use std::io::Read;
    let result = std::fs::File::open(input).and_then(|mut file| file.read(&mut [0u8; 1]));
    match result {
        Ok(_) => Ok(()),
        Err(error) if is_permission_denied(&error) => Err(permission_error(input)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(format!("file not found: {}", input.display()))
        }
        Err(error) => Err(format!("could not read {}: {error}", input.display())),
    }
}

fn ffmpeg_read_error(input: &Path, stderr: &str) -> String {
    if stderr.contains("Operation not permitted") || stderr.contains("Permission denied") {
        return permission_error(input);
    }
    format!("ffmpeg could not read this file: {stderr}")
}

pub fn decode_to_16k_mono(input: &Path) -> Result<Vec<f32>, String> {
    if !input.exists() {
        return Err(format!("file not found: {}", input.display()));
    }
    preflight_read(input)?;

    // ffmpeg is the ONLY decoder on this path: warm-vs-warm it matches
    // AVFoundation on latency, ffmpeg still ships regardless as the webm/mkv
    // fallback, and Parakeet TDT's published WER was measured on
    // ffmpeg-decoded audio. `av_decode` is retained only for the
    // `av_decode_check` harness, not the shipping path.
    decode_with_ffmpeg(input)
}

fn decode_with_ffmpeg(input: &Path) -> Result<Vec<f32>, String> {
    let tool = media_tool()?;
    // 16 kHz mono s16 from stdout; sample identity with the legacy temp-WAV decode is
    // proven by tests/ffmpeg_decode_parity.rs. No deadline: multi-hour files are
    // legitimate, and nothing cancels this call today.
    let options = DecodeOptions {
        sample_rate: 16_000,
        channels: 1,
        cancel: CancellationToken::new(),
        timeout: None,
        max_duration: None,
        audio_stream_index: None,
    };
    let samples = decode_pcm_s16(tool, input, &options).map_err(|e| match e {
        RunError::Failed { stderr_tail, .. } => ffmpeg_read_error(input, stderr_tail.trim()),
        RunError::Spawn(e) => spawn_error(tool, &e, true),
        other => format!("ffmpeg could not read this file: {other}"),
    })?;
    Ok(samples
        .into_iter()
        .map(|v| v as f32 / i16::MAX as f32)
        .collect())
}

fn spawn_error(tool: &VerifiedFfmpeg, error: &std::io::Error, install_hint: bool) -> String {
    let path = tool.ffmpeg_path().display();
    if install_hint {
        format!("could not run ffmpeg at {path} ({error}). Install ffmpeg or use a bundled build.")
    } else {
        format!("could not run ffmpeg at {path} ({error}).")
    }
}

/// Best-effort media duration in whole seconds WITHOUT decoding the file. Runs
/// `ffmpeg -i <file>` (prints metadata to stderr, exits non-zero because no output
/// was requested — expected) and parses the `Duration: HH:MM:SS.ss` line. Returns
/// Ok(None) when ffmpeg reports no duration; the caller then relies on the
/// post-decode sample-count check. Err only when ffmpeg can't be run.
pub fn probe_duration_secs(input: &Path) -> Result<Option<u32>, String> {
    if !input.exists() {
        return Err(format!("file not found: {}", input.display()));
    }
    preflight_read(input)?;

    // AVFoundation reads duration from the container header, so this replaces a
    // whole process spawn plus stderr parsing. Falls through to ffmpeg for
    // containers it cannot open, matching the decode path above.
    #[cfg(target_os = "macos")]
    if let Some(secs) = crate::av_decode::probe_duration_secs(input) {
        return Ok(Some(secs));
    }

    let tool = media_tool()?;
    match probe_media_opt_with(tool, input, None, &CancellationToken::new()) {
        Ok(probe) => Ok(probe
            .and_then(|p| p.duration_ms)
            .map(|ms| (ms / 1000) as u32)),
        Err(ProbeError::Run(RunError::Spawn(e))) => Err(spawn_error(tool, &e, false)),
        // ffmpeg ran but gave no usable duration: the post-decode sample count decides.
        Err(_) => Ok(None),
    }
}

/// The ONLY place that chooses an ffmpeg binary. Verification hashes the file and runs
/// `-version`, so a verified tool is cached; a failure is never cached.
fn media_tool() -> Result<&'static VerifiedFfmpeg, String> {
    static TOOL: OnceLock<VerifiedFfmpeg> = OnceLock::new();
    if let Some(tool) = TOOL.get() {
        return Ok(tool);
    }
    let tool = verify_in_catalog(&ffmpeg_command()?).map_err(|error| media_tool_error(&error))?;
    Ok(TOOL.get_or_init(|| tool))
}

fn media_tool_error(error: &MediaError) -> String {
    format!(
        "The audio decoder failed its integrity check ({error}). Reinstall {} to restore it.",
        app_name()
    )
}

fn verify_in_catalog(path: &Path) -> Result<VerifiedFfmpeg, MediaError> {
    let catalog = ArtifactCatalog::embedded()?;
    let artifact = catalog.approved(std::env::consts::OS, std::env::consts::ARCH)?;
    let request = ResolutionRequest {
        override_path: None,
        bundled_paths: vec![path.to_path_buf()],
        development_path: None,
        allow_development_path: false,
    };
    verify_ffmpeg(
        resolve_catalog_ffmpeg(artifact, &request)?,
        Duration::from_secs(10),
    )
}

fn ffmpeg_command() -> Result<PathBuf, String> {
    resolve_ffmpeg(
        std::env::current_exe().ok().as_deref(),
        crate::engine_paths::ffmpeg_path()
            .as_deref()
            .map(Path::as_os_str),
    )
}

fn resolve_ffmpeg(
    exe: Option<&Path>,
    explicit: Option<&std::ffi::OsStr>,
) -> Result<PathBuf, String> {
    if let Some(path) = explicit {
        let path = PathBuf::from(path);
        if path.is_absolute() && path.is_file() {
            return Ok(path);
        }
    }
    let name = if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };
    if let Some(exe) = exe {
        if let Some(dir) = exe.parent() {
            for sibling in [
                dir.join("runtime").join(name),
                dir.join("resources/runtime").join(name),
            ] {
                if sibling.is_file() {
                    return Ok(sibling);
                }
            }
            #[cfg(target_os = "macos")]
            if let Some(contents_dir) = dir.parent() {
                let resource = contents_dir.join("Resources").join("runtime").join(name);
                if resource.is_file() {
                    return Ok(resource);
                }
            }
        }
    }
    Err(format!(
        "Bundled media decoder is missing; repair the {} installation.",
        app_name()
    ))
}

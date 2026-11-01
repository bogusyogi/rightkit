//! Offline file transcription decode path for the sidecar.
//!
//! The shell chooses a file; the sidecar owns media decoding and ASR. ffmpeg is
//! resolved from an explicit bundled path or packaged runtime. PATH is dev-only.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use rightkit_ffmpeg::decode::{decode_pcm_s16, DecodeOptions};
use rightkit_ffmpeg::probe::{probe_media_opt_with, ProbeError};
use rightkit_ffmpeg::runner::RunError;
use rightkit_ffmpeg::{
    resolve_ffmpeg as resolve_catalog_ffmpeg, verify_ffmpeg, ArtifactCatalog, CancellationToken,
    DevFfmpeg, FfmpegBinary, MediaError, ResolutionRequest, VerifiedFfmpeg,
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

fn spawn_error(tool: &MediaTool, error: &std::io::Error, install_hint: bool) -> String {
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

/// The ffmpeg this process runs: catalog-verified when the bundled build is in the
/// approved catalog, otherwise the same path as an explicit development binary.
enum MediaTool {
    Verified(VerifiedFfmpeg),
    Dev(DevFfmpeg),
}

impl FfmpegBinary for MediaTool {
    fn ffmpeg_path(&self) -> &Path {
        match self {
            Self::Verified(tool) => tool.ffmpeg_path(),
            Self::Dev(tool) => tool.ffmpeg_path(),
        }
    }
}

/// The ONLY place that chooses an ffmpeg binary. Verification hashes the file and runs
/// `-version`, so a verified tool is cached; a failure is never cached.
fn media_tool() -> Result<&'static MediaTool, String> {
    static TOOL: OnceLock<MediaTool> = OnceLock::new();
    if let Some(tool) = TOOL.get() {
        return Ok(tool);
    }
    let tool = choose_tool(ffmpeg_command()?)?;
    Ok(TOOL.get_or_init(|| tool))
}

/// The bundled ffmpeg is staged from the RightKit catalog, so it verifies. Anything else is
/// refused: only a binary that is merely not in the catalog may run unverified, and only where
/// rightkit-ffmpeg allows it (`DevFfmpeg::try_unverified`, refused unless the crate's
/// `dev-overrides` feature is on, which no the app build enables). A release build never runs
/// an unverified ffmpeg.
fn choose_tool(path: PathBuf) -> Result<MediaTool, String> {
    choose_tool_with(path, verify_in_catalog)
}

/// [`choose_tool`] with the catalog verification passed in, so tests inject each failure.
fn choose_tool_with(
    path: PathBuf,
    verify: impl FnOnce(&Path) -> Result<VerifiedFfmpeg, MediaError>,
) -> Result<MediaTool, String> {
    // A bare name is the debug-only PATH lookup; there is no file to hash.
    if path.is_file() {
        match verify(&path) {
            Ok(verified) => return Ok(MediaTool::Verified(verified)),
            Err(error) if !error.is_not_in_catalog() => return Err(media_tool_error(&error)),
            Err(error) => {
                tracing::warn!("ffmpeg build is not in the approved media catalog ({error})");
            }
        }
    }
    DevFfmpeg::try_unverified(path)
        .map(MediaTool::Dev)
        .map_err(|error| media_tool_error(&error))
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
        cfg!(debug_assertions),
    )
}

fn resolve_ffmpeg(
    exe: Option<&Path>,
    explicit: Option<&std::ffi::OsStr>,
    allow_path: bool,
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
    if allow_path {
        Ok(PathBuf::from(name))
    } else {
        Err(format!(
            "Bundled media decoder is missing; repair the {} installation.",
            app_name()
        ))
    }
}

#[cfg(test)]
mod resolver_tests {
    use super::*;

    #[test]
    fn denied_reads_become_typed_permission_errors() {
        let usb = Path::new("/Volumes/D/talking.mp4");
        for stderr in [
            "/Volumes/D/talking.mp4: Operation not permitted",
            "x: Permission denied",
        ] {
            let message = ffmpeg_read_error(usb, stderr);
            assert!(message.starts_with("E_PERMISSION: "), "{message}");
            assert!(message.contains("/Volumes/D/talking.mp4"), "{message}");
        }
        if cfg!(target_os = "macos") {
            assert!(permission_error(usb).contains("Removable Volumes"));
            assert!(permission_error(Path::new("/Users/a/x.mp4")).contains("Full Disk Access"));
        }
        assert!(is_permission_denied(&std::io::Error::from_raw_os_error(1)));
        assert!(is_permission_denied(&std::io::Error::from(
            std::io::ErrorKind::PermissionDenied
        )));
        assert!(!is_permission_denied(&std::io::Error::from(
            std::io::ErrorKind::NotFound
        )));
        assert!(ffmpeg_read_error(usb, "Invalid data found")
            .starts_with("ffmpeg could not read this file: "));
    }

    #[test]
    fn preflight_reads_readable_files_and_types_missing_ones() {
        let path = std::env::temp_dir().join(format!("hr_preflight_{}.bin", std::process::id()));
        std::fs::write(&path, b"x").unwrap();
        assert_eq!(preflight_read(&path), Ok(()));
        std::fs::remove_file(&path).unwrap();
        assert!(preflight_read(&path)
            .unwrap_err()
            .starts_with("file not found: "));
    }

    #[test]
    fn shipped_entry_points_decode_and_probe_a_fixture() {
        // Only test that touches the cached tool: point the production search at a staged build.
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let staged = [
            manifest.join("../src-tauri/resources/runtime/ffmpeg.exe"),
            manifest.join("../src-tauri/resources/runtime/ffmpeg"),
            PathBuf::from("D:/Claude/vendor/ffmpeg-lgpl/ffmpeg-win-x64.exe"),
        ]
        .into_iter()
        .find_map(|p| p.canonicalize().ok().filter(|p| p.is_file()));
        let Some(staged) = staged else {
            println!("SKIP: no staged ffmpeg for the production entry points");
            return;
        };
        std::env::set_var("RIGHTKIT_FFMPEG_PATH", &staged);
        let tool = media_tool().expect("explicit ffmpeg resolves");
        println!(
            "media tool: {}",
            if matches!(tool, MediaTool::Verified(_)) {
                "verified"
            } else {
                "dev"
            }
        );
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../rightkit-dictation-engine/tests/fixtures/short_9s.wav");
        let samples = decode_to_16k_mono(&fixture).expect("decode fixture");
        assert_eq!(samples.len(), 156_160);
        assert!(samples.iter().any(|s| s.abs() > 0.01));
        assert_eq!(probe_duration_secs(&fixture), Ok(Some(9)));
    }

    #[test]
    fn release_resolution_never_falls_back_to_path() {
        assert!(resolve_ffmpeg(None, None, false).is_err());
        assert!(resolve_ffmpeg(None, Some(std::ffi::OsStr::new("ffmpeg.exe")), false).is_err());
        assert!(resolve_ffmpeg(None, None, true).is_ok());
    }

    const REFUSED: &str = "The audio decoder failed its integrity check";

    fn refused(result: Result<MediaTool, String>, case: &str) {
        match result {
            Err(message) => assert!(message.starts_with(REFUSED), "{case}: {message}"),
            Ok(_) => panic!("{case}: an unverified ffmpeg was accepted"),
        }
    }

    /// Every catalog verification failure ends in the user-facing error, never in an
    /// unverified ffmpeg: the not-in-catalog pair goes to rightkit-ffmpeg's
    /// `DevFfmpeg::try_unverified`, which refuses without `dev-overrides`.
    #[test]
    fn every_verification_failure_is_refused() {
        let file = std::env::temp_dir().join(format!("hr-ffmpeg-refusal-{}", std::process::id()));
        std::fs::write(&file, b"not an approved ffmpeg").expect("stand-in binary");
        let failures = [
            MediaError::DigestMismatch { path: file.clone() },
            MediaError::NoApprovedArtifact {
                os: "windows".into(),
                arch: "x86_64".into(),
            },
            MediaError::ArtifactPending {
                id: "ffmpeg-7.1.5-windows-x64".into(),
            },
            MediaError::ArtifactNotFound,
            MediaError::BuildProbeFailed,
            MediaError::BuildProbeTimedOut,
            MediaError::MalformedBinary("truncated PE".into()),
            MediaError::UnexpectedVersion {
                expected: "7.1.5".into(),
            },
        ];
        for failure in failures {
            let case = format!("{failure:?}");
            refused(choose_tool_with(file.clone(), move |_| Err(failure)), &case);
        }
        let _ = std::fs::remove_file(&file);
    }

    /// A bare name (no file to hash) is refused too: it never reaches the verifier.
    #[test]
    fn a_name_without_a_file_is_refused() {
        refused(
            choose_tool_with(PathBuf::from("ffmpeg"), |_| {
                panic!("a missing file is never verified")
            }),
            "bare name",
        );
    }
}

#[cfg(all(test, windows))]
mod tests {
    use rightkit_ffmpeg::runner::{run_capture_with, RunOptions};
    use rightkit_ffmpeg::FfmpegCommand;

    use super::*;

    #[test]
    fn media_runner_child_has_no_console_and_preserves_output() {
        let probe = r#"Add-Type 'using System; using System.Runtime.InteropServices; public static class ConsoleProbe { [DllImport("kernel32.dll")] public static extern IntPtr GetConsoleWindow(); }'; [Console]::WriteLine([ConsoleProbe]::GetConsoleWindow().ToInt64()); [Console]::Error.WriteLine('decoder stderr'); exit 7"#;
        let args = ["-NoProfile", "-NonInteractive", "-Command", probe]
            .map(String::from)
            .to_vec();
        let output = run_capture_with(
            &DevFfmpeg::unverified("powershell.exe"),
            &FfmpegCommand::raw_args(args),
            RunOptions::unbounded(CancellationToken::new()),
        )
        .expect("run hidden decoder child probe");
        assert_eq!(output.status.code(), Some(7));
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "0");
        assert_eq!(output.stderr.trim(), "decoder stderr");
    }
}

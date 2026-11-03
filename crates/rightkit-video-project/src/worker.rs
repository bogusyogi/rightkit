//! Media worker: runs a typed [`FfmpegCommand`] (never through a shell) on the
//! RightKit media runner (`rightkit-ffmpeg`). `-progress` output becomes
//! [`ProgressEvent`]s; cancellation, a timeout and a bounded stderr tail in failures
//! come from the runner.

use std::io;
use std::path::Path;
use std::process::Output;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use rightkit_ffmpeg as rk;
use rightkit_ffmpeg::FfmpegCommand;

pub use rightkit_ffmpeg::runner::{StdioMode, StdioPolicy};
pub use rightkit_ffmpeg::OwnedChild;

use crate::error::ProjectError;

/// Time between the polite stop signal and the forced kill of the process tree. Zero
/// keeps this worker's behaviour from before the port: cancel and timeout kill at once.
const KILL_GRACE: Duration = Duration::ZERO;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgressEvent {
    pub frame: u64,
    pub out_time_ms: i64,
    /// `progress=end` was reported.
    pub end: bool,
}

pub struct RunOptions<'a> {
    pub timeout: Duration,
    pub cancel: Arc<AtomicBool>,
    pub on_progress: &'a mut dyn FnMut(&ProgressEvent),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunReport {
    pub frames: u64,
    pub out_time_ms: i64,
    pub elapsed_ms: u64,
}

/// Maps a RightKit runner error onto this crate's error codes. `tool` names the program
/// in messages (`ffmpeg`, `ffprobe`).
pub(crate) fn map_run_error(
    program: &Path,
    tool: &str,
    timeout: Duration,
    error: rk::RunError,
) -> ProjectError {
    match error {
        rk::RunError::Spawn(e) => ProjectError::MediaToolMissing(format!(
            "cannot run {tool} at {}: {e}",
            program.display()
        )),
        rk::RunError::Io(e) => ProjectError::io(format!("waiting for {tool}"), e),
        rk::RunError::Cancelled => ProjectError::RenderCancelled,
        rk::RunError::TimedOut(_) => ProjectError::RenderTimeout(timeout.as_secs()),
        rk::RunError::Failed {
            status,
            stderr_tail,
        } => ProjectError::RenderFailed {
            message: format!("{tool} exited with {status}: {stderr_tail}"),
        },
        other => ProjectError::RenderFailed {
            message: format!("{tool}: {other}"),
        },
    }
}

/// Maps a spawn-time runner error onto the `io::Error` that callers already handle. The
/// spawn path only fails with `Spawn` or `Io`; the other variants come from running
/// to completion and are reported as text.
pub(crate) fn spawn_error_to_io(error: rk::RunError) -> io::Error {
    match error {
        rk::RunError::Spawn(e) | rk::RunError::Io(e) => e,
        other => io::Error::other(other.to_string()),
    }
}

/// Spawns `ffmpeg` with the typed argument vector (no shell) and returns the owned child.
/// This is the single typed spawn point for ffmpeg. The caller chooses the stdio policy
/// and maps the error; a streaming caller keeps the child for its whole lifetime, and
/// dropping the child stops its process tree.
pub fn spawn_ffmpeg(
    ffmpeg: &Path,
    cmd: &FfmpegCommand,
    policy: StdioPolicy,
) -> io::Result<OwnedChild> {
    rk::runner::spawn_streaming(
        &rk::verify_ffmpeg_handle(
            ffmpeg,
            rk::EXPORT_ASS_ZIMG_VARIANT,
            crate::tools::VERIFY_TIMEOUT,
        )
        .or_else(|_| {
            rk::verify_ffmpeg_handle(ffmpeg, rk::EXPORT_ASS_VARIANT, crate::tools::VERIFY_TIMEOUT)
        })
        .map_err(std::io::Error::other)?,
        cmd,
        policy,
    )
    .map_err(spawn_error_to_io)
}

/// Waits for `child` and returns what it wrote. Stdout and stderr are drained
/// concurrently through RightKit byte captures, so a full pipe on one stream cannot stall
/// the other. A stream the caller did not pipe is simply empty. The wait closes stdin
/// first, as `Child::wait_with_output` does.
pub fn wait_with_output(mut child: OwnedChild) -> io::Result<Output> {
    use rightkit_child_process::OutputCapture;
    child.close_stdin();
    let mut stdout = child
        .take_stdout()
        .map(|pipe| OutputCapture::try_capture(pipe, usize::MAX))
        .transpose()?;
    let mut stderr = child
        .take_stderr()
        .map(|pipe| OutputCapture::try_capture(pipe, usize::MAX))
        .transpose()?;
    let status = child.wait()?;
    let stdout_bytes = stdout
        .as_mut()
        .map(|capture| capture.finish().bytes)
        .unwrap_or_default();
    let stderr_bytes = stderr
        .as_mut()
        .map(|capture| capture.finish().bytes)
        .unwrap_or_default();
    for capture in [stdout.as_ref(), stderr.as_ref()].into_iter().flatten() {
        if let Some(error) = capture.take_error() {
            return Err(error);
        }
    }
    Ok(Output {
        status,
        stdout: stdout_bytes,
        stderr: stderr_bytes,
    })
}

/// Runs `ffmpeg` with `cmd`. The command's own `-progress pipe:1` lines are read from
/// stdout; stderr (warnings and errors) is kept as a bounded tail for failures.
pub fn run_ffmpeg(
    ffmpeg: &Path,
    cmd: &FfmpegCommand,
    opts: RunOptions<'_>,
) -> Result<RunReport, ProjectError> {
    let RunOptions {
        timeout,
        cancel,
        on_progress,
    } = opts;
    let mut forward = |event: &rk::ProgressEvent| {
        on_progress(&ProgressEvent {
            frame: event.frame,
            out_time_ms: event.out_time_ms,
            end: event.end,
        });
    };
    // Built field by field: `RunOptions` gains fields over time.
    let mut options = rk::RunOptions::new(timeout, rk::CancellationToken::from_flag(cancel))
        .with_kill_grace(KILL_GRACE);
    options.on_progress = Some(&mut forward);
    let run = rk::run_with_progress(
        &rk::verify_ffmpeg_handle(
            ffmpeg,
            rk::EXPORT_ASS_ZIMG_VARIANT,
            crate::tools::VERIFY_TIMEOUT,
        )
        .or_else(|_| {
            rk::verify_ffmpeg_handle(ffmpeg, rk::EXPORT_ASS_VARIANT, crate::tools::VERIFY_TIMEOUT)
        })
        .map_err(|e| ProjectError::MediaToolMissing(e.to_string()))?,
        cmd,
        options,
    )
    .map_err(|e| map_run_error(ffmpeg, "ffmpeg", timeout, e))?;
    Ok(RunReport {
        frames: run.frames,
        out_time_ms: run.out_time_ms,
        elapsed_ms: run.elapsed_ms,
    })
}

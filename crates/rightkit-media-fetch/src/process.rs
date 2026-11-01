use std::{io, path::Path, thread, time::Duration};

use rightkit_child_process::{OwnedCommand, ProcessPriority, RunnerFailure, RunnerPhase};
use rightkit_ffmpeg::{
    runner::{run_with_progress, RunError, RunOptions},
    CancellationToken, VerifiedFfmpeg,
};

use crate::args::normalize_ffmpeg_command;

/// Ceiling for one normalize run. The original app had no timeout before; this only
/// bounds a wedged FFmpeg, and 20-90 minute media finishes far inside it.
const FFMPEG_TIMEOUT: Duration = Duration::from_secs(6 * 60 * 60);
/// How often the calling thread samples `should_cancel` while FFmpeg runs.
const FFMPEG_CANCEL_POLL: Duration = Duration::from_millis(50);

/// Run the verified ffmpeg to convert `input` → `out_wav` (16 kHz mono PCM s16le).
///
/// The binary was already verified by the caller, which also hands the same
/// path to yt-dlp. Cancellation is sampled from `should_cancel` on this thread
/// and forwarded to the `rightkit-ffmpeg` runner, which stops the whole FFmpeg
/// tree. The run is placed at the same priority as the fetch children.
pub(crate) fn run_ffmpeg_cancelable(
    ffmpeg: &VerifiedFfmpeg,
    input: &Path,
    out_wav: &Path,
    should_cancel: &impl Fn() -> bool,
) -> Result<(), String> {
    let command = normalize_ffmpeg_command(input, out_wav);
    let token = CancellationToken::new();
    let worker_result = thread::scope(|scope| {
        let worker = scope.spawn(|| {
            run_with_progress(
                ffmpeg,
                &command,
                RunOptions::new(FFMPEG_TIMEOUT, token.clone()).with_priority(FETCH_CHILD_PRIORITY),
            )
        });
        while !worker.is_finished() {
            if should_cancel() {
                token.cancel();
            }
            thread::sleep(FFMPEG_CANCEL_POLL);
        }
        worker.join()
    });
    let report = worker_result.map_err(|_| "ffmpeg worker panicked".to_string())?;
    match report {
        Ok(_) => Ok(()),
        Err(RunError::Cancelled) => Err("cancelled".into()),
        Err(RunError::Failed {
            status,
            stderr_tail,
        }) => Err(format!(
            "ffmpeg exited with {status}: {}",
            crate::stderr_tail(&stderr_tail)
        )),
        Err(RunError::TimedOut(limit)) => Err(format!("ffmpeg timed out after {limit:?}")),
        Err(RunError::Spawn(error)) => Err(format!(
            "failed to launch ffmpeg ({}): {error}",
            ffmpeg.path().display()
        )),
        Err(RunError::Io(error)) => Err(format!("ffmpeg i/o failure: {error}")),
        // RunError is non-exhaustive; this crate never sets a working directory,
        // so anything else is reported as a launch failure with the crate's text.
        Err(error) => Err(format!(
            "failed to launch ffmpeg ({}): {error}",
            ffmpeg.path().display()
        )),
    }
}

/// Fetch children (yt-dlp) run at `Efficient` priority: `BelowNormal`
/// plus the platform's energy class, so the app stays responsive (#184). The
/// ffmpeg runs in this crate take the same priority through `RunOptions`.
pub(crate) const FETCH_CHILD_PRIORITY: ProcessPriority = ProcessPriority::Efficient;

/// Most stdout kept from a fetch child (`StreamOptions::stdout_tail_cap`). yt-dlp prints the
/// `SR_TITLE=` / `SR_DURATION=` records before the download and then one `[download]` progress
/// line per update, so the tail must outlast a long download for the earlier records to survive.
pub(crate) const FETCH_STDOUT_CAP: usize = 16 * 1024 * 1024;

/// The yt-dlp command from the configured `ytdlp_cmd` (executable plus leading arguments),
/// owned and at fetch priority, ready for `rightkit_child_process::run_streaming`.
pub(crate) fn fetch_command(ytdlp_cmd: &[String]) -> Result<OwnedCommand, String> {
    let (exe, leading) = ytdlp_cmd
        .split_first()
        .ok_or_else(|| "ytdlp_cmd is empty".to_string())?;
    let mut command = OwnedCommand::new(exe);
    command.command_mut().args(leading);
    command.process_priority(FETCH_CHILD_PRIORITY);
    Ok(command)
}

/// The caller-facing text of a failed `run_streaming` call: a spawn failure names the launch,
/// anything else (a read, wait or drain failure) names the tool and the cause.
pub(crate) fn stream_error(label: &str, error: &io::Error) -> String {
    match RunnerFailure::from_io(error).map(RunnerFailure::phase) {
        Some(RunnerPhase::Spawn) => format!("failed to launch {label}: {error}"),
        _ => format!("{label} failed: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_child_process::{run_streaming, StreamOptions};

    #[test]
    fn fetch_children_run_efficient() {
        assert_eq!(FETCH_CHILD_PRIORITY, ProcessPriority::Efficient);
    }

    #[test]
    fn fetch_command_refuses_an_empty_command() {
        assert_eq!(fetch_command(&[]).unwrap_err(), "ytdlp_cmd is empty");
    }

    #[test]
    fn a_missing_program_is_reported_as_a_launch_failure() {
        let command =
            fetch_command(&["rightkit-no-such-yt-dlp-binary".to_string()]).expect("command");
        let error = run_streaming(
            command,
            &StreamOptions::new(),
            &|| false,
            &mut |_| {},
            &mut |_| {},
        )
        .expect_err("a missing program cannot start");
        let text = stream_error("yt-dlp", &error);
        assert!(text.starts_with("failed to launch yt-dlp: "), "{text}");
    }

    #[cfg(not(windows))]
    #[test]
    fn fetch_command_streams_stderr_lines_and_stdout() {
        let mut command = fetch_command(&["sh".to_string(), "-c".to_string()]).expect("command");
        command
            .command_mut()
            .arg(r"printf 'one\rtwo\r\nthree\n' >&2; printf out");
        let mut lines = Vec::new();
        let mut stdout = Vec::new();
        let options = StreamOptions::new().stdout_tail_cap(FETCH_STDOUT_CAP);
        let output = run_streaming(
            command,
            &options,
            &|| false,
            &mut |bytes| stdout.extend_from_slice(bytes),
            &mut |line| lines.push(line.to_string()),
        )
        .expect("test child runs");
        assert_eq!(lines, ["one", "two", "three"]);
        assert_eq!(stdout, b"out");
        assert_eq!(output.stderr_tail, "one\ntwo\nthree");
        assert_eq!(output.stdout_tail, b"out");
    }

    #[cfg(windows)]
    #[test]
    fn fetch_command_streams_stderr_lines_and_stdout() {
        let mut command = fetch_command(&["cmd".to_string(), "/C".to_string()]).expect("command");
        command.command_mut().arg("echo one 1>&2 & echo two 1>&2");
        let mut lines = Vec::new();
        let output = run_streaming(
            command,
            &StreamOptions::new(),
            &|| false,
            &mut |_| {},
            &mut |line| lines.push(line.trim_end().to_string()),
        )
        .expect("test child runs");
        assert_eq!(lines, ["one", "two"]);
        assert!(output.stderr_tail.contains("two"));
    }
}

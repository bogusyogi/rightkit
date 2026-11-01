use std::path::Path;

use rightkit_ffmpeg::FfmpegCommand;

use crate::{FetchPlan, MediaDownload};

/// Build yt-dlp arguments for either transcription, audio download, or video
/// download. Video plans deliberately omit the app's Android-client audio
/// workaround because it caps current YouTube format discovery at 360p.
pub fn build_ytdlp_args_for_plan(
    url: &str,
    out_tmpl: &str,
    cookies: Option<&Path>,
    cookies_from_browser: Option<&str>,
    plan: &FetchPlan,
    ffmpeg: Option<&Path>,
    deno: Option<&Path>,
) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();

    if is_youtube_url(url) && !matches!(plan.download, MediaDownload::Video(_)) {
        // Prefer yt-dlp's default client set and keep the Android client as a
        // fallback rather than the only choice: forcing android narrows
        // format discovery. No `formats=missing_pot` filter — it *includes*
        // formats missing their required PO token (per yt-dlp's YouTube
        // extractor docs), i.e. the exact streams that 403 on download.
        args.push("--extractor-args".into());
        args.push("youtube:player_client=default,android".into());
    }

    if is_youtube_url(url) {
        if let Some(path) = deno {
            args.push("--js-runtimes".into());
            args.push(format!("deno:{}", path.to_string_lossy()));
        }
    }

    args.push("-f".into());
    args.push(plan.format_selector());

    if matches!(plan.download, MediaDownload::Video(_)) {
        // `bv*+ba/b` guarantees a selected video contains audio: yt-dlp either
        // downloads split streams and stream-copy merges them, or falls back
        // to one combined format. No transcoding is requested.
        args.push("--merge-output-format".into());
        // Matroska accepts all stream combinations yt-dlp can return (in
        // particular AV1+Opus from current YouTube), without re-encoding.
        args.push("mkv".into());
        if let Some(path) = ffmpeg {
            args.push("--ffmpeg-location".into());
            args.push(path.to_string_lossy().into_owned());
        }
    } else if matches!(plan.download, MediaDownload::Audio(_)) {
        // A YouTube fallback may be a combined MP4 even when audio was
        // requested. Extract its audio stream so download-only output has an
        // honest audio container/extension; `best` avoids forced re-encoding.
        args.push("--extract-audio".into());
        args.push("--audio-format".into());
        args.push("best".into());
        if let Some(path) = ffmpeg {
            args.push("--ffmpeg-location".into());
            args.push(path.to_string_lossy().into_owned());
        }
    }

    args.push("-o".into());
    args.push(out_tmpl.into());

    args.push("--print".into());
    args.push("before_dl:SR_TITLE=%(title)s".into());
    args.push("--print".into());
    args.push("before_dl:SR_DURATION=%(duration)s".into());
    args.push("--print".into());
    args.push("after_move:SR_FILEPATH=%(filepath)s".into());

    // `--print` implies simulation; restore the actual download.
    args.push("--no-simulate".into());
    // Emit one progress record per line so the engine can forward real
    // download updates instead of waiting for a carriage-return terminal
    // redraw or process exit.
    args.push("--newline".into());
    // `--print` also implies quiet, which suppresses the `[download]` lines
    // this app's progress reporting is built on — re-enable them explicitly.
    args.push("--progress".into());

    args.push("--no-playlist".into());
    args.push("--max-filesize".into());
    args.push(plan.max_filesize().into());
    args.push("--socket-timeout".into());
    args.push("20".into());

    if let Some(spec) = cookies_from_browser {
        args.push("--cookies-from-browser".into());
        args.push(spec.to_string());
    } else if let Some(p) = cookies {
        args.push("--cookies".into());
        args.push(p.to_string_lossy().into_owned());
    }

    args.push(url.into());

    args
}

fn is_youtube_url(url: &str) -> bool {
    matches!(
        crate::detect::host(url).as_deref(),
        Some("youtube.com" | "m.youtube.com" | "youtu.be")
    )
}

/// Build the yt-dlp command vector from an executable override plus optional
/// leading args. `command_env` remains one executable path; `args_env` is
/// shell-like whitespace split with single/double quote support.
pub fn ytdlp_cmd_from_env_parts(
    command_env: Option<String>,
    args_env: Option<String>,
) -> Vec<String> {
    crate::ytdlp::ytdlp_cmd_from_env_parts(command_env, args_env)
}

/// The typed normalize command that `process::run_ffmpeg_cancelable` runs.
///
/// Argv: `-y -loglevel error -i <input> -vn -ac 1 -ar 16000 -c:a pcm_s16le <out_wav>`,
/// which produces a 16 kHz, mono, PCM s16le WAV. Paths go in as `OsString`-faithful
/// values, not lossy text. The runner adds no flags of its own (no `-hide_banner`,
/// `-nostdin` or `-progress`).
pub(crate) fn normalize_ffmpeg_command(input: &Path, out_wav: &Path) -> FfmpegCommand {
    let mut command = FfmpegCommand::new();
    command.text("-y").texts(["-loglevel", "error"]);
    command
        .input(input)
        .texts(["-vn", "-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le"])
        .path(out_wav);
    command
}

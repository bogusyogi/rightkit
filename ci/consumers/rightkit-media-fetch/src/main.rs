//! Registry consumer for rightkit-media-fetch 0.1.0: builds against the published API only.
//! It never runs yt-dlp or ffmpeg.
use rightkit_media_fetch::detect::{classify, is_media_ext, JobKind};
use rightkit_media_fetch::{ytdlp_cmd_from_env_parts, FetchPlan};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    check(
        classify("https://www.youtube.com/watch?v=dQw4w9WgXcQ") == JobKind::YouTube,
        "a YouTube URL classifies as YouTube",
    );
    check(
        is_media_ext("mp3") && !is_media_ext("txt"),
        "the media-type table knows mp3, not txt",
    );
    check(
        ytdlp_cmd_from_env_parts(None, Some("--proxy 'a b'".into()))
            == vec!["yt-dlp".to_string(), "--proxy".into(), "a b".into()],
        "the yt-dlp argv is built from the overrides",
    );
    check(
        FetchPlan::transcribe().needs_wav,
        "a transcribe plan needs a WAV",
    );
    println!("rightkit-media-fetch consumer: all checks passed");
}

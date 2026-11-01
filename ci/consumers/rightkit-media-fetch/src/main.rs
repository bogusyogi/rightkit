//! Registry consumer for rightkit-media-fetch 0.1.1: builds against the published API only.
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
    check(
        rightkit_media_fetch::x_post::x_status_id("https://x.com/u/status/12345?s=1").as_deref()
            == Some("12345"),
        "an X status id is read from the URL",
    );
    check(
        rightkit_media_fetch::source_auth::is_tiktok_host("https://vm.tiktok.com/abc"),
        "TikTok short-link hosts are recognized",
    );
    check(
        rightkit_media_fetch::extraction::classify_error_kind("operation timed out")
            == rightkit_media_fetch::protocol::ErrorKind::Timeout,
        "error text classifies to a kind",
    );
    println!("rightkit-media-fetch consumer: all checks passed");
}

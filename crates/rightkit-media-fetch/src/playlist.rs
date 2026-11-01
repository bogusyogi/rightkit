//! #251: enumerate a playlist or channel with `yt-dlp --flat-playlist`.
//!
//! Only the entry list is fetched (no media, no per-video extraction), bounded
//! by `--playlist-end` and a wall-clock timeout. The argument builder and the
//! output parser are pure so they are unit-tested without yt-dlp.

use std::time::Duration;

use rightkit_child_process::{run_streaming, StreamOptions};

/// Wall-clock cap on one enumeration.
pub const ENUMERATE_TIMEOUT: Duration = Duration::from_secs(120);

/// Prefix of every entry line, so ordinary yt-dlp output cannot be mistaken
/// for an entry.
const ENTRY_PREFIX: &str = "SR_ENTRY=";

/// yt-dlp arguments (without the executable) that list `url`'s entries, at
/// most `limit` of them, one `SR_ENTRY=<url>` line each.
pub fn build_ytdlp_enumerate_args(url: &str, limit: u32) -> Vec<String> {
    vec![
        "--flat-playlist".into(),
        "--playlist-end".into(),
        limit.max(1).to_string(),
        // A private or deleted entry must not fail the whole listing.
        "--ignore-errors".into(),
        "--no-warnings".into(),
        "--print".into(),
        format!("{ENTRY_PREFIX}%(url)s"),
        "--socket-timeout".into(),
        "20".into(),
        url.into(),
    ]
}

/// Entry URLs from yt-dlp's stdout: http(s) only, first sighting wins, cut to
/// `limit`. Lines without the prefix and `NA` values are dropped.
pub fn parse_enumerate_output(stdout: &str, limit: u32) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for line in stdout.lines() {
        let Some(value) = line.trim().strip_prefix(ENTRY_PREFIX) else {
            continue;
        };
        let value = value.trim();
        if !(value.starts_with("https://") || value.starts_with("http://")) {
            continue;
        }
        if seen.insert(value.to_string()) {
            out.push(value.to_string());
            if out.len() >= limit as usize {
                break;
            }
        }
    }
    out
}

fn tail(text: &str) -> String {
    let t = text.trim();
    let skip = t.chars().count().saturating_sub(300);
    t.chars().skip(skip).collect()
}

/// Run yt-dlp (`ytdlp_cmd`: executable plus leading args) and return the
/// entry URLs of `url`, at most `limit`. A non-zero exit still returns the
/// entries read (a partial listing beats none); with none it is an error.
pub fn list_playlist_entries(
    ytdlp_cmd: &[String],
    url: &str,
    limit: u32,
    should_cancel: &impl Fn() -> bool,
) -> Result<Vec<String>, String> {
    let mut cmd = crate::process::fetch_command(ytdlp_cmd)?;
    cmd.command_mut()
        .args(build_ytdlp_enumerate_args(url, limit));
    let options = StreamOptions::new()
        .timeout(ENUMERATE_TIMEOUT)
        .stdout_tail_cap(crate::process::FETCH_STDOUT_CAP);
    let output = run_streaming(cmd, &options, should_cancel, &mut |_| {}, &mut |_| {})
        .map_err(|e| crate::process::stream_error("yt-dlp (list playlist)", &e))?;
    if output.timed_out {
        return Err("listing the playlist timed out".into());
    }
    if output.cancelled {
        return Err("cancelled".into());
    }
    let entries = parse_enumerate_output(&String::from_utf8_lossy(&output.stdout_tail), limit);
    if !entries.is_empty() {
        return Ok(entries);
    }
    if output.status.success() {
        return Ok(entries);
    }
    Err(format!(
        "yt-dlp could not list it: {}",
        tail(&output.stderr_tail)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_are_flat_bounded_and_end_with_the_url() {
        let args = build_ytdlp_enumerate_args("https://www.youtube.com/@x/videos", 50);
        assert!(args.contains(&"--flat-playlist".to_string()));
        let end = args.iter().position(|a| a == "--playlist-end").unwrap();
        assert_eq!(args[end + 1], "50");
        assert!(args.contains(&"SR_ENTRY=%(url)s".to_string()));
        assert_eq!(args.last().unwrap(), "https://www.youtube.com/@x/videos");
        // The single-video flag must never appear on a listing.
        assert!(!args.iter().any(|a| a == "--no-playlist"));
    }

    #[test]
    fn zero_limit_still_asks_for_one_entry() {
        let args = build_ytdlp_enumerate_args("u", 0);
        let end = args.iter().position(|a| a == "--playlist-end").unwrap();
        assert_eq!(args[end + 1], "1");
    }

    #[test]
    fn output_keeps_http_entries_deduped_and_capped() {
        let stdout = "[youtube:tab] Extracting\n\
            SR_ENTRY=https://www.youtube.com/watch?v=a\n\
            SR_ENTRY=NA\n\
            SR_ENTRY=file:///etc/passwd\n\
            SR_ENTRY=https://www.youtube.com/watch?v=a\n\
            SR_ENTRY=https://www.youtube.com/watch?v=b\r\n\
            SR_ENTRY=https://www.youtube.com/watch?v=c\n";
        assert_eq!(
            parse_enumerate_output(stdout, 2),
            vec![
                "https://www.youtube.com/watch?v=a".to_string(),
                "https://www.youtube.com/watch?v=b".to_string(),
            ]
        );
        assert_eq!(parse_enumerate_output(stdout, 10).len(), 3);
        assert!(parse_enumerate_output("", 5).is_empty());
    }

    #[test]
    fn missing_executable_is_a_typed_error_not_a_panic() {
        let err = list_playlist_entries(&[], "u", 5, &|| false).unwrap_err();
        assert!(err.contains("empty"), "{err}");
        let err = list_playlist_entries(
            &["definitely-not-a-real-yt-dlp-binary".to_string()],
            "u",
            5,
            &|| false,
        )
        .unwrap_err();
        assert!(err.contains("failed to launch"), "{err}");
    }
}

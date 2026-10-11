//! Shared by this crate's suites: the missing-asset policy for hosted runners.
#![allow(dead_code)]

/// Reports an asset that this host does not have, then returns so the caller returns from the
/// test. Prints one `SKIP <test>: <what> (looked at <where>)` line to stderr. With
/// `RIGHTKIT_REQUIRE_ASSETS=1` it panics with that same text instead, so a host that is meant to
/// have the asset cannot pass by skipping. The test name is the name of the test thread.
pub fn skip_missing(what: &str, looked_at: &str) {
    let test = std::thread::current()
        .name()
        .unwrap_or("unnamed")
        .to_owned();
    let message = format!("SKIP {test}: {what} (looked at {looked_at})");
    if std::env::var("RIGHTKIT_REQUIRE_ASSETS").is_ok_and(|value| value == "1") {
        panic!("{message}");
    }
    eprintln!("{message}");
}

/// Where the FFmpeg lookup looked (the same candidates `resolve_for_development` tries), for the
/// SKIP line.
pub fn ffmpeg_looked_at() -> String {
    let candidates: Vec<String> = rightkit_media::dev_resolve::development_candidates()
        .iter()
        .map(|path| path.display().to_string())
        .collect();
    format!("RIGHTKIT_FFMPEG or {}", candidates.join(", "))
}

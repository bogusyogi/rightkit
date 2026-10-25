//! Shared by this crate's suites: the missing-asset policy for hosted runners.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

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

/// A binary for tests that run a script or a system tool through the runner. `DevFfmpeg::unverified`
/// is not available to integration tests (it needs the `dev-overrides` feature, which a plain
/// `cargo test` does not turn on), so this implements `FfmpegBinary` itself.
pub struct TestBinary(PathBuf);

impl TestBinary {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self(path.into())
    }
}

impl rightkit_ffmpeg::FfmpegBinary for TestBinary {
    fn ffmpeg_path(&self) -> &Path {
        &self.0
    }
}

/// Where the FFmpeg lookup looked (the same candidates `resolve_for_development` tries), for the
/// SKIP line.
pub fn ffmpeg_looked_at() -> String {
    let candidates: Vec<String> = rightkit_ffmpeg::dev_resolve::development_candidates()
        .iter()
        .map(|path| path.display().to_string())
        .collect();
    format!("RIGHTKIT_FFMPEG or {}", candidates.join(", "))
}

/// `rel` found by walking up from this crate: `<ancestor>/<rel>`, then `<ancestor>/claude/<rel>`.
/// A fixed `../..` depth breaks when the checkout moves, so workspace assets are searched for.
pub fn workspace_asset(rel: &str) -> Option<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .flat_map(|dir| [dir.join(rel), dir.join("claude").join(rel)])
        .find(|path| path.exists())
}

/// The approved FFmpeg artifacts folder, `vendor/ffmpeg-lgpl` in the workspace. When it is absent
/// the returned path still names where it was looked for, for the SKIP line.
pub fn vendor_dir() -> PathBuf {
    workspace_asset("vendor/ffmpeg-lgpl").unwrap_or_else(|| {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../vendor/ffmpeg-lgpl")
    })
}

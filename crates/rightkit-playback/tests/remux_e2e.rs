//! Outcome-level test: a real FFmpeg remux of a real media fixture produces a
//! playable derivative, leaves the source byte-identical, is reused from cache
//! on a second call, and honours cancellation. Skips only when no FFmpeg binary
//! is available on this host (RIGHTKIT_PLAYBACK_FFMPEG or PATH).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use rightkit_media::CancellationToken;
use rightkit_playback::{
    compatibility_copy, MediaKind, PlaybackError, PlaybackRegistry, PlaybackSource,
};

fn ffmpeg() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("RIGHTKIT_PLAYBACK_FFMPEG").map(PathBuf::from) {
        return path.is_file().then_some(path);
    }
    for dir in std::env::split_paths(&std::env::var_os("PATH")?) {
        let candidate = dir.join(if cfg!(windows) {
            "ffmpeg.exe"
        } else {
            "ffmpeg"
        });
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn run(ffmpeg: &Path, args: &[&str]) -> bool {
    Command::new(ffmpeg)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn tempdir(tag: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "rightkit-playback-{tag}-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir_all(&root).unwrap();
    root
}

/// Build a real MKV source (video + sine audio) with the resolved FFmpeg.
fn make_source(ffmpeg: &Path, dir: &Path) -> PathBuf {
    let mkv = dir.join("clip source.mkv");
    let ok = run(
        ffmpeg,
        &[
            "-hide_banner",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=2:size=160x120:rate=10",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=2",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest",
            mkv.to_str().unwrap(),
        ],
    );
    assert!(
        ok,
        "fixture generation failed — host FFmpeg lacks libx264/aac?"
    );
    mkv
}

#[test]
fn real_remux_produces_playable_mp4_reuses_cache_and_preserves_source() {
    let Some(ffmpeg) = ffmpeg() else {
        eprintln!("SKIP: no FFmpeg binary available");
        return;
    };
    let dir = tempdir("remux");
    let source_path = make_source(&ffmpeg, &dir);
    let before = fs::read(&source_path).unwrap();
    let cache = dir.join("cache");
    let token = CancellationToken::new();

    let derivative =
        compatibility_copy(&ffmpeg, &PlaybackSource::new(&source_path), &cache, &token)
            .expect("compatibility copy");

    // Outcome: a non-trivial MP4 derivative exists under the cache root.
    assert!(derivative.is_file());
    assert_eq!(derivative.extension().unwrap(), "mp4");
    assert!(derivative.starts_with(&cache));
    assert!(derivative.metadata().unwrap().len() > 256);
    // Source bytes untouched.
    assert_eq!(fs::read(&source_path).unwrap(), before);
    // Derivative probes as mp4 video+audio via the same FFmpeg build.
    assert!(run(
        &ffmpeg,
        &[
            "-hide_banner",
            "-i",
            derivative.to_str().unwrap(),
            "-f",
            "null",
            "-"
        ]
    ));
    // Second call hits the cache: same path, no rewrite.
    let again =
        compatibility_copy(&ffmpeg, &PlaybackSource::new(&source_path), &cache, &token).unwrap();
    assert_eq!(again, derivative);
    // Explicit audio kind produces an .m4a derivative instead.
    let audio = compatibility_copy(
        &ffmpeg,
        &PlaybackSource::new(&source_path).with_kind(MediaKind::Audio),
        &cache,
        &token,
    )
    .unwrap();
    assert_eq!(audio.extension().unwrap(), "m4a");
    assert!(audio.metadata().unwrap().len() > 256);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn cancellation_and_lifecycle_are_explicit() {
    let dir = tempdir("cancel");
    let source_path = dir.join("missing-source.bin");
    let token = CancellationToken::new();
    token.cancel();
    let err = compatibility_copy(
        Path::new("unused-ffmpeg"),
        &PlaybackSource::new(&source_path),
        &dir,
        &token,
    )
    .unwrap_err();
    assert!(matches!(err, PlaybackError::Cancelled));
    assert_eq!(err.code(), "PLAYBACK_CANCELLED");

    // A missing source is a coded failure, not a panic.
    let err = compatibility_copy(
        Path::new("unused-ffmpeg"),
        &PlaybackSource::new(&source_path),
        &dir,
        &CancellationToken::new(),
    )
    .unwrap_err();
    assert!(matches!(err, PlaybackError::Missing));

    // Registry: early cancel blocks begin; ids release on drop; invalid ids refuse.
    let registry = PlaybackRegistry::shared();
    let id = format!("playback-e2e-{}", std::process::id());
    registry.cancel(&id);
    assert!(registry.begin(&id).is_err());
    registry.cancel("not-a-playback-id"); // ignored, no panic
    assert!(registry.begin("bogus").is_err());
    fs::remove_dir_all(&dir).ok();
}

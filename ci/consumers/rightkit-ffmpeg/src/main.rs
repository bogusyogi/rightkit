// rightkit-ffmpeg 0.2.10 consumed by exact registry pin, outside the workspace.
//
// Proves the pure, asset-free API: argument builders (`pcm_s16le_16khz_mono_args`,
// `command::extract_args`, `command::transcode_audio_command`), the `-encoders` listing parser
// `EncoderInventory::parse` with `pick_h264` on a fixed listing, and the embedded artifact catalog
// (`ArtifactCatalog::embedded`, and `parse` refusing an unsupported schema). FFmpeg is never
// resolved or run.
// Does not prove: `verify_ffmpeg`, `run_ffmpeg`, the decode or encode path, or any binary digest.
// State: none. No network, no files, no real user data.
use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use rightkit_ffmpeg::command::{
    extract_args, transcode_audio_command, AudioTranscode, ExtractMode, ExtractOptions,
};
use rightkit_ffmpeg::encoders::{pick_h264, EncoderInventory};
use rightkit_ffmpeg::{pcm_s16le_16khz_mono_args, ArtifactCatalog, MediaError};

fn s<E: std::fmt::Debug>(e: E) -> String {
    format!("{e:?}")
}

fn os(values: &[&str]) -> Vec<OsString> {
    values.iter().copied().map(OsString::from).collect()
}

fn main() -> Result<(), String> {
    // 16-bit mono 16 kHz WAV argument vector, exact.
    let wav = pcm_s16le_16khz_mono_args(Path::new("in.mp4"), Path::new("out.wav"));
    assert_eq!(
        wav,
        os(&[
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            "in.mp4",
            "-vn",
            "-ac",
            "1",
            "-ar",
            "16000",
            "-c:a",
            "pcm_s16le",
            "out.wav",
        ])
    );

    // Typed audio transcode: the sample rate and channel pairs are present, the output is last.
    let cmd = transcode_audio_command(
        Path::new("in.mp4"),
        Path::new("out.wav"),
        &AudioTranscode::pcm_s16le(16_000, 1),
    );
    let args = cmd.to_os_args();
    let pair = |flag: &str, value: &str| {
        args.windows(2)
            .any(|w| w[0] == OsString::from(flag) && w[1] == OsString::from(value))
    };
    assert!(pair("-ar", "16000"));
    assert!(pair("-ac", "1"));
    assert!(pair("-c:a", "pcm_s16le"));
    assert_eq!(args.last(), Some(&OsString::from("out.wav")));

    // Clip with a stream copy: seconds print with microsecond precision, `-ss` before `-i`.
    let clip = extract_args(
        Path::new("in.mp4"),
        Path::new("clip.mp4"),
        &ExtractOptions {
            start: Some(Duration::from_millis(1_500)),
            duration: Some(Duration::from_secs(2)),
            mode: Some(ExtractMode::Copy),
            extra: Vec::new(),
        },
    );
    assert_eq!(
        clip,
        os(
            &[
                "-y", "-ss", "1.500000", "-i", "in.mp4", "-t", "2.000000", "-c", "copy",
                "clip.mp4",
            ]
        )
    );

    // Encoder listing: video rows only (V-flags); the legend and header rows are skipped.
    let listing =
        "Encoders:\n V..... = Video\n V....D libopenh264  OpenH264 H.264\n A....D aac  AAC\n";
    let inventory = EncoderInventory::parse(listing);
    assert!(inventory.has("libopenh264"));
    assert!(!inventory.has("aac"));
    assert!(!inventory.has("h264_nvenc"));
    // No hardware H.264 encoder on this Linux listing, so the software fallback is picked.
    assert_eq!(
        pick_h264(&inventory, "linux", &mut |_| true),
        Some("libopenh264")
    );

    // Embedded catalog: schema 2, and the approved Windows decode entry is present by id.
    let catalog = ArtifactCatalog::embedded().map_err(s)?;
    assert_eq!(catalog.schema_version, 2);
    assert!(catalog.by_id("ffmpeg-7.1.5-windows-x64").is_some());

    // A catalog with an unsupported schema is refused, typed.
    let refused = ArtifactCatalog::parse(r#"{"schemaVersion": 99, "artifacts": []}"#);
    assert!(matches!(
        refused,
        Err(MediaError::UnsupportedCatalogSchema(99))
    ));

    println!("OK rightkit-ffmpeg 0.2.10 (registry consumer)");
    Ok(())
}

//! Decode distinct audio streams from real media & verify output duration in samples.

mod common;

use std::{path::Path, time::Duration};

use rightkit_ffmpeg::{
    decode::{decode_pcm_f32, decode_pcm_s16, DecodeOptions},
    dev_resolve::resolve_for_development,
    run_checked, CancellationToken, FfmpegBinary, FfmpegCommand, RunError,
};

const T: Duration = Duration::from_secs(30);

fn two_audio_streams(binary: &dyn FfmpegBinary, output: &Path) {
    let mut cmd = FfmpegCommand::base("error");
    cmd.texts([
        "-f",
        "lavfi",
        "-i",
        "aevalsrc=0.125:s=8000:d=0.5",
        "-f",
        "lavfi",
        "-i",
        "aevalsrc=-0.375:s=8000:d=0.5",
        "-map",
        "0:a:0",
        "-map",
        "1:a:0",
        "-c:a",
        "pcm_f32le",
    ])
    .path(output);
    run_checked(binary, &cmd, T, &CancellationToken::new()).expect("two-stream fixture");
}

#[test]
fn public_decoders_select_audio_streams_and_limit_output_duration() {
    let Some((binary, _)) = resolve_for_development() else {
        common::skip_missing("FFmpeg", &common::ffmpeg_looked_at());
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("two audio streams.mkv");
    two_audio_streams(&binary, &input);
    let mut options = DecodeOptions {
        sample_rate: 8_000,
        channels: 1,
        cancel: CancellationToken::new(),
        timeout: Some(T),
        ..Default::default()
    }
    .with_max_duration(Some(Duration::from_millis(125)))
    .with_audio_stream_index(Some(1));

    let second = decode_pcm_f32(&binary, &input, &options).unwrap();
    assert_eq!(second.len(), 1_000);
    assert!(second.iter().all(|sample| *sample == -0.375));
    let second_s16 = decode_pcm_s16(&binary, &input, &options).unwrap();
    assert_eq!(second_s16, vec![-12_288; 1_000]);

    options = options.with_audio_stream_index(Some(0));
    let first = decode_pcm_f32(&binary, &input, &options).unwrap();
    assert_eq!(first, vec![0.125; 1_000]);

    options = options.with_max_duration(None);
    let full = decode_pcm_f32(&binary, &input, &options).unwrap();
    assert_eq!(full, vec![0.125; 4_000]);

    options = options.with_audio_stream_index(None);
    let automatic = decode_pcm_f32(&binary, &input, &options).unwrap();
    assert_eq!(automatic, full);

    options = options.with_audio_stream_index(Some(2));
    assert!(matches!(
        decode_pcm_f32(&binary, &input, &options),
        Err(RunError::Failed { .. })
    ));
}

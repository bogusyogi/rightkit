//! End-to-end audio coverage against real files, FFmpeg, and (when available) a
//! named loopback input device.  No test routes audio to speakers.

#![cfg(feature = "capture")]

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use rightkit_audio::{
    capture::{list_input_devices, CaptureSession, OutDType, SessionConfig, StreamResampler},
    chain::{Denoise, DialogueChain, HighPass, Limiter},
    ducking::Ducking,
    measure::{rms_db, AudioTools},
    room_tone::{find_room_tone, GapSpan, RoomToneParams},
    wav::{read_wav_16k_mono, read_wav_mono_f32, write_wav_mono_i16},
};
use rightkit_media::{
    dev_resolve::{resolve_for_development, DevResolved},
    run_checked, CancellationToken, FfmpegBinary, FfmpegCommand,
};

const TIMEOUT: Duration = Duration::from_secs(120);

fn ffmpeg() -> DevResolved {
    let (bin, kind) = resolve_for_development().expect("FFmpeg must be available for audio E2E");
    eprintln!(
        "audio E2E FFmpeg: {kind:?} ({})",
        bin.ffmpeg_path().display()
    );
    bin
}

fn path(dir: &Path, name: &str) -> PathBuf {
    dir.join(name)
}

fn render(bin: &dyn FfmpegBinary, output: &Path, args: &[&str]) {
    let mut cmd = FfmpegCommand::base("error");
    cmd.texts(args.iter().copied()).path(output);
    run_checked(bin, &cmd, TIMEOUT, &CancellationToken::new()).expect("FFmpeg fixture render");
}

fn source_wav(bin: &dyn FfmpegBinary, output: &Path, duration: u32, frequency: u32) {
    render(
        bin,
        output,
        &[
            "-f",
            "lavfi",
            "-i",
            &format!("sine=frequency={frequency}:sample_rate=48000:duration={duration}"),
            "-ac",
            "2",
            "-c:a",
            "pcm_s16le",
        ],
    );
}

#[test]
fn filter_graph_render_is_measurable_and_wav_round_trips() {
    let bin = ffmpeg();
    let dir = tempfile::tempdir().unwrap();
    let source = path(dir.path(), "source.wav");
    let filtered = path(dir.path(), "filtered.wav");
    source_wav(&bin, &source, 3, 440);

    let chain = DialogueChain {
        high_pass: Some(HighPass { hz: 80.0, poles: 2 }),
        denoise: Denoise::None,
        expander: None,
        compressor: None,
        deesser: None,
        limiter: Limiter {
            ceiling_db: -1.5,
            attack_ms: 5.0,
            release_ms: 50.0,
        },
    };
    let tools = AudioTools::new(&bin);
    tools.apply_chain(&source, &filtered, &chain).unwrap();
    let level = tools.measure_loudness(&filtered).unwrap();
    assert!(level.integrated_lufs.is_some());
    assert!(level.true_peak_dbtp.unwrap() <= 0.0);

    let decoded = tools
        .decode_mono_f32(&filtered, 48_000, None, None)
        .unwrap();
    assert!(
        decoded.len() > 140_000,
        "decoded samples: {}",
        decoded.len()
    );
    assert!(decoded.iter().all(|sample| sample.is_finite()));

    let roundtrip = path(dir.path(), "roundtrip.wav");
    write_wav_mono_i16(&roundtrip, 16_000, &decoded[..16_000.min(decoded.len())]).unwrap();
    let samples = read_wav_16k_mono(&roundtrip).unwrap();
    assert_eq!(samples.len(), 16_000.min(decoded.len()));
    assert!(read_wav_mono_f32(&roundtrip).unwrap().samples.len() == samples.len());
}

#[test]
fn ducking_and_room_tone_render_have_content_changes() {
    let bin = ffmpeg();
    let dir = tempfile::tempdir().unwrap();
    let music = path(dir.path(), "music.wav");
    let dialogue = path(dir.path(), "dialogue.wav");
    let ducked = path(dir.path(), "ducked.wav");
    source_wav(&bin, &music, 4, 220);
    source_wav(&bin, &dialogue, 4, 440);

    let tools = AudioTools::new(&bin);
    tools
        .duck_music(
            &music,
            &dialogue,
            &Ducking::Spans {
                duck_db: -18.0,
                fade_ms: 50,
            },
            &[(1_000, 2_000)],
            &ducked,
        )
        .unwrap();
    let before = rms_db(
        &tools
            .decode_mono_f32(&ducked, 48_000, Some((200, 500)), None)
            .unwrap(),
    );
    let during = rms_db(
        &tools
            .decode_mono_f32(&ducked, 48_000, Some((1_300, 500)), None)
            .unwrap(),
    );
    assert!(before - during > 8.0, "ducking delta {before} vs {during}");

    // Build a real quiet-noise/tone/quiet WAV, select its quiet region, then render
    // an FFmpeg room-tone layer over a timeline gap.
    let source = path(dir.path(), "room-source.wav");
    let mut samples = Vec::with_capacity(48_000 * 4);
    for i in 0..48_000 * 4 {
        let t = i as f32 / 48_000.0;
        let quiet = (i % 17) as f32 / 17.0 * 0.002 - 0.001;
        samples.push(if (1.0..2.0).contains(&t) {
            0.1 * (2.0 * std::f32::consts::PI * 330.0 * t).sin()
        } else {
            quiet
        });
    }
    write_wav_mono_i16(&source, 48_000, &samples).unwrap();
    let room = find_room_tone(
        &samples,
        48_000,
        &[(1_000, 2_000)],
        &RoomToneParams {
            max_level_db: -35.0,
            ..RoomToneParams::default()
        },
    )
    .unwrap();
    assert!(room.start_ms < 900 || room.start_ms > 2_100);
    let layer = path(dir.path(), "room-layer.wav");
    let chain = DialogueChain {
        high_pass: None,
        denoise: Denoise::None,
        expander: None,
        compressor: None,
        deesser: None,
        limiter: Limiter {
            ceiling_db: -1.5,
            attack_ms: 5.0,
            release_ms: 50.0,
        },
    };
    tools
        .render_room_tone_layer(
            &source,
            &room,
            &chain,
            &[GapSpan {
                start_ms: 1_200,
                end_ms: 1_700,
            }],
            0.0,
            80,
            4_000,
            &layer,
        )
        .unwrap();
    let gap = rms_db(
        &tools
            .decode_mono_f32(&layer, 48_000, Some((1_250, 300)), None)
            .unwrap(),
    );
    let outside = rms_db(
        &tools
            .decode_mono_f32(&layer, 48_000, Some((200, 300)), None)
            .unwrap(),
    );
    assert!(
        gap > outside + 10.0,
        "room tone gap {gap} vs outside {outside}"
    );
}

#[test]
fn resampler_and_capture_gate_are_explicit() {
    let mut resampler = StreamResampler::new(48_000, 16_000).unwrap();
    let input: Vec<f32> = (0..48_000).map(|i| (i as f32 * 0.01).sin()).collect();
    let output = resampler.process_offline(&input).unwrap();
    assert!((output.len() as isize - 16_000).unsigned_abs() < 64);
    assert!(output.iter().all(|sample| sample.is_finite()));

    let devices = list_input_devices().unwrap();
    let Some(device) = devices.iter().find(|device| {
        let name = device.name.to_ascii_lowercase();
        name.contains("blackhole") || name.contains("loopback") || name.contains("soundflower")
    }) else {
        eprintln!("capture gate: no BlackHole/Loopback/Soundflower input device present; no microphone opened");
        return;
    };
    let mut session = CaptureSession::start(SessionConfig {
        device_id: Some(device.id),
        device_platform_id: device.platform_id.clone(),
        target_rate: 16_000,
        target_channels: 1,
        target_dtype: OutDType::Float32,
        block_ms: 20,
    })
    .expect("loopback capture should open");
    let samples = session.read_f32_blocking(160, 2_000).unwrap();
    assert!(samples.len() >= 160);
    assert!(samples.iter().all(|sample| sample.is_finite()));
    session.pause();
}

/// Real IMMNotificationClient + power registration on this host: both sources
/// register, nothing is pending at rest, a second independent registration
/// coexists, and drop unregisters without blocking.
#[cfg(windows)]
#[test]
fn windows_lifecycle_registers_and_unregisters() {
    use rightkit_audio::lifecycle::try_register;
    use std::time::Instant;

    let (first, first_rx) = try_register().expect("endpoint + power registration");
    assert!(first.take_pending_power_event().is_none());
    assert!(first_rx.try_recv().is_err());
    let (second, _second_rx) = try_register().expect("second independent registration");
    let started = Instant::now();
    drop(second);
    drop(first);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "unregister must not block"
    );
    assert!(first_rx.recv_timeout(Duration::from_millis(50)).is_err());
}

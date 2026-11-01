//! Real-media proof for streamed playback audio: `AudioStreamer` decodes only
//! the output window asked for, so timelines longer than any in-memory cap
//! still play with audio at the correct offset.
//!
//! Needs an editing ffmpeg named by `RIGHTKIT_FFMPEG`; prints `SKIP:` and returns without one.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rightkit_compose::native::{AudioMixDescriptor, AudioSegment, AudioStreamer};
use rightkit_timeline::plan::LayerRole;
use rightkit_timeline::{MilliDb, SourceId, Ticks};

const RATE: i64 = 48_000;
const FREQ: f64 = 440.0;
const SECS: i64 = 660; // > 10 minutes — beyond the old ~8.3 min whole-mix cap
const CHUNK: i64 = 96_000; // same granularity the playback feeder uses

fn ffmpeg() -> Option<PathBuf> {
    std::env::var_os("RIGHTKIT_FFMPEG")
        .map(PathBuf::from)
        .filter(|p| p.is_file())
}

/// Deterministic source: s16le stereo WAV, 0.5-amplitude sine. No encoder
/// needed, stereo avoids ffmpeg's mono upmix pan law, and `-ss` seeks in WAV
/// are sample-exact.
fn write_sine_wav(path: &Path, secs: i64) {
    let frames = secs * RATE;
    let data_bytes = frames as u32 * 4;
    let mut w = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    use std::io::Write;
    w.write_all(b"RIFF").unwrap();
    w.write_all(&(36u32 + data_bytes).to_le_bytes()).unwrap();
    w.write_all(b"WAVEfmt ").unwrap();
    w.write_all(&16u32.to_le_bytes()).unwrap();
    w.write_all(&1u16.to_le_bytes()).unwrap(); // PCM
    w.write_all(&2u16.to_le_bytes()).unwrap(); // stereo
    w.write_all(&(RATE as u32).to_le_bytes()).unwrap();
    w.write_all(&(RATE as u32 * 4).to_le_bytes()).unwrap();
    w.write_all(&4u16.to_le_bytes()).unwrap();
    w.write_all(&16u16.to_le_bytes()).unwrap();
    w.write_all(b"data").unwrap();
    w.write_all(&data_bytes.to_le_bytes()).unwrap();
    let mut buf = Vec::with_capacity(RATE as usize * 4);
    for i in 0..frames {
        let s = (f64::sin(2.0 * std::f64::consts::PI * FREQ * i as f64 / RATE as f64) * 16383.0)
            .round() as i16;
        buf.extend_from_slice(&s.to_le_bytes());
        buf.extend_from_slice(&s.to_le_bytes());
        if buf.len() == buf.capacity() {
            w.write_all(&buf).unwrap();
            buf.clear();
        }
    }
    w.write_all(&buf).unwrap();
}

fn expected(i: i64) -> f32 {
    (f64::sin(2.0 * std::f64::consts::PI * FREQ * i as f64 / RATE as f64) * 16383.0 / 32768.0)
        as f32
}

fn streamer_for(wav: &Path, total: i64) -> AudioStreamer {
    let id = SourceId::new("b".repeat(64)).unwrap();
    let descriptor = AudioMixDescriptor {
        sample_rate: RATE,
        total_samples: total,
        layers: vec![AudioSegment {
            id: "l1".into(),
            role: LayerRole::SourceAudio,
            source: id.clone(),
            source_in: Ticks(0),
            start_sample: 0,
            duration_samples: total,
            gain_linear: 1.0,
            gain_db: MilliDb(0),
            windows: vec![],
        }],
    };
    AudioStreamer::new(
        ffmpeg().expect("checked by caller"),
        descriptor,
        HashMap::from([(id, wav.to_path_buf())]),
        2,
    )
}

/// Mean absolute error of `chunk` against the analytic sine at absolute frame
/// `base`, over the left channel's middle of the buffer.
fn sine_error(chunk: &[f32], base: i64) -> f64 {
    let frames = chunk.len() / 2;
    let lo = frames / 4;
    let hi = frames * 3 / 4;
    let mut err = 0f64;
    for f in lo..hi {
        err += (chunk[f * 2] as f64 - expected(base + f as i64) as f64).abs();
    }
    err / (hi - lo) as f64
}

#[test]
fn long_timeline_streams_bounded_chunks() {
    if ffmpeg().is_none() {
        eprintln!("SKIP: RIGHTKIT_FFMPEG unavailable; skipping stream proof");
        panic!("audio_stream not run: RIGHTKIT_FFMPEG unavailable");
    }
    let dir = tempfile::tempdir().unwrap();
    let wav = dir.path().join("sine.wav");
    write_sine_wav(&wav, SECS);
    let total = SECS * RATE;
    let streamer = streamer_for(&wav, total);
    assert_eq!(streamer.total_frames(), total);

    // A >10-minute mix is scheduled in bounded chunks: every decode_range call
    // returns exactly the requested window (never the whole mix) and matches
    // the source sine at the window's absolute offset.
    let mut at = 0i64;
    let mut chunks = 0;
    while at < 3 * CHUNK {
        let n = (total - at).min(CHUNK) as usize;
        let chunk = streamer.decode_range(at, n).unwrap();
        assert_eq!(chunk.len(), n * 2);
        assert!(
            chunk.len() * 4 <= (CHUNK as usize) * 8,
            "chunk exceeds bounded memory"
        );
        let e = sine_error(&chunk, at);
        assert!(e < 0.01, "chunk at {at} mean error {e}");
        at += n as i64;
        chunks += 1;
    }
    assert!(chunks >= 3);
}

#[test]
fn seek_far_yields_correct_audio_offset() {
    if ffmpeg().is_none() {
        eprintln!("SKIP: RIGHTKIT_FFMPEG unavailable; skipping seek proof");
        panic!("audio_stream not run: RIGHTKIT_FFMPEG unavailable");
    }
    let dir = tempfile::tempdir().unwrap();
    let wav = dir.path().join("sine.wav");
    write_sine_wav(&wav, SECS);
    let streamer = streamer_for(&wav, SECS * RATE);

    // Seek to the 10-minute mark: frame 0 of the returned chunk must be the
    // mix frame at that absolute position.
    let seek = 600 * RATE;
    let chunk = streamer.decode_range(seek, CHUNK as usize).unwrap();
    assert_eq!(chunk.len(), CHUNK as usize * 2);
    assert!(
        (chunk[0] - expected(seek)).abs() < 0.02,
        "frame at seek offset: got {} want {}",
        chunk[0],
        expected(seek)
    );
    let e = sine_error(&chunk, seek);
    assert!(e < 0.01, "seeked chunk mean error {e}");

    // A window spanning the mix end returns full length with a silent tail.
    let tail = streamer.decode_range(SECS * RATE - 1000, 2000).unwrap();
    assert_eq!(tail.len(), 4000);
    assert!(tail[..2000].iter().any(|s| s.abs() > 0.01));
    assert_eq!(&tail[1000 * 2..], &[0f32; 2000][..]);
}

#[test]
fn non_overlapping_layers_are_not_decoded() {
    if ffmpeg().is_none() {
        eprintln!("SKIP: RIGHTKIT_FFMPEG unavailable; skipping layer proof");
        panic!("audio_stream not run: RIGHTKIT_FFMPEG unavailable");
    }
    let dir = tempfile::tempdir().unwrap();
    let wav = dir.path().join("sine.wav");
    write_sine_wav(&wav, SECS);
    let id = SourceId::new("b".repeat(64)).unwrap();
    let missing = SourceId::new("c".repeat(64)).unwrap();
    let total = SECS * RATE;
    let descriptor = AudioMixDescriptor {
        sample_rate: RATE,
        total_samples: total,
        layers: vec![
            AudioSegment {
                id: "late".into(),
                role: LayerRole::SourceAudio,
                source: id.clone(),
                source_in: Ticks(0),
                start_sample: 500 * RATE,
                duration_samples: 100 * RATE,
                gain_linear: 1.0,
                gain_db: MilliDb(0),
                windows: vec![],
            },
            // Absent from the source map: must contribute silence, not error.
            AudioSegment {
                id: "gone".into(),
                role: LayerRole::Music,
                source: missing,
                source_in: Ticks(0),
                start_sample: 0,
                duration_samples: total,
                gain_linear: 1.0,
                gain_db: MilliDb(0),
                windows: vec![],
            },
        ],
    };
    let streamer = AudioStreamer::new(
        ffmpeg().unwrap(),
        descriptor,
        HashMap::from([(id, wav.to_path_buf())]),
        2,
    );
    // Early window: only the missing-source layer overlaps -> silence.
    let early = streamer.decode_range(0, RATE as usize).unwrap();
    assert!(early.iter().all(|s| *s == 0.0));
    // Late window: the placed layer decodes at its absolute offset.
    let late = streamer.decode_range(501 * RATE, RATE as usize).unwrap();
    assert!((late[0] - expected(501 * RATE - 500 * RATE)).abs() < 0.02);
}

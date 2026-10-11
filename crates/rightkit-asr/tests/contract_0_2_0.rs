//! Model-free contract checks for the 0.2.0 Parakeet additions: terminal-silence
//! pad policy, u64 window-offset conversion, cancellation and progress in
//! samples, the typed window-length error and the strict transducer layout.

use rightkit_asr::longform::{self, LongFormOptions, LongFormProgress, PadPolicy, SeamPolicy};
use rightkit_asr::{Error, StrictLayout, TimedPiece, Transducer, TranscriptionResult};
use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

fn tone(n: usize) -> Vec<f32> {
    (0..n).map(|i| ((i as f32) * 0.05).sin() * 0.3).collect()
}

fn opts(pad: bool, policy: PadPolicy) -> LongFormOptions {
    LongFormOptions {
        window_samples: 240_000,
        terminal_silence: pad,
        seam: SeamPolicy::Exact,
        word_assembly_floor_ms: Some(80),
        pad_policy: policy,
    }
}

fn empty() -> TranscriptionResult {
    TranscriptionResult { text: String::new(), tokens: Vec::new() }
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("rk-asr-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn pad_terminal_silence_appends_480ms_to_speech_only() {
    let speech = tone(16_000);
    let padded = longform::pad_terminal_silence(&speech, &opts(true, PadPolicy::FinalSegmentOnly));
    assert_eq!(padded.len(), 16_000 + 7_680, "480 ms at 16 kHz");
    assert_eq!(&padded[..16_000], &speech[..], "real samples come first");
    assert!(padded[16_000..].iter().all(|s| *s == 0.0));

    let off = longform::pad_terminal_silence(&speech, &opts(false, PadPolicy::FinalSegmentOnly));
    assert_eq!(off.len(), 16_000, "no pad when terminal_silence is off");

    let silence = vec![0.0f32; 16_000];
    let quiet = longform::pad_terminal_silence(&silence, &opts(true, PadPolicy::FinalSegmentOnly));
    assert_eq!(quiet.len(), 16_000, "a segment that ends in silence is not padded");
}

#[test]
fn every_window_policy_pads_more_than_final_only_and_keeps_the_schedule() {
    let audio = tone(60 * 16_000);
    let run = |policy: PadPolicy| {
        let lens = RefCell::new(Vec::new());
        longform::transcribe_padded_window(
            &audio,
            &opts(true, policy),
            None,
            &|_p: &LongFormProgress| {},
            |seg| {
                lens.borrow_mut().push(seg.len());
                Ok(empty())
            },
        )
        .unwrap();
        lens.into_inner()
    };
    let final_only = run(PadPolicy::FinalSegmentOnly);
    let every = run(PadPolicy::EveryWindow);
    assert_eq!(final_only.len(), every.len(), "the cut schedule is identical");
    assert!(every.len() > 1, "fixture must span several windows");
    assert!(every[0] > final_only[0], "the first (non-final) window is padded under EveryWindow");
    assert_eq!(every.last(), final_only.last(), "the final segment is padded under both policies");
    assert_eq!(PadPolicy::default(), PadPolicy::FinalSegmentOnly);
}

#[test]
fn cancel_flag_stops_before_the_next_window_and_progress_counts_samples() {
    let audio = tone(60 * 16_000);
    let cancel = AtomicBool::new(false);
    let progress = RefCell::new(Vec::new());
    let calls = RefCell::new(0usize);
    let result = longform::transcribe_padded_window(
        &audio,
        &opts(false, PadPolicy::FinalSegmentOnly),
        Some(&cancel),
        &|p: &LongFormProgress| progress.borrow_mut().push((p.samples_done, p.samples_total)),
        |_seg| {
            *calls.borrow_mut() += 1;
            if *calls.borrow() == 2 {
                cancel.store(true, Ordering::SeqCst);
            }
            Ok(empty())
        },
    );
    assert!(matches!(result, Err(Error::Cancelled)));
    assert_eq!(*calls.borrow(), 2, "no decode after the flag is set");
    let p = progress.borrow();
    assert_eq!(p.len(), 2);
    assert!(p.iter().all(|(_, total)| *total == audio.len()), "progress totals are sample counts");
    assert!(p[1].0 > p[0].0, "samples done advances per window");
}

#[test]
fn window_offsets_become_seconds_only_at_the_edge() {
    assert_eq!(longform::samples_to_secs(16_000 * 3_600), 3_600.0);
    assert_eq!(longform::samples_to_secs(8_000), 0.5);
    assert!((longform::samples_to_secs(1) - 1.0 / 16_000.0).abs() < 1e-9);
}

#[test]
fn window_too_long_is_a_typed_error_naming_both_limits() {
    let e = Error::WindowTooLong { samples: 300_000, max_samples: 240_160 };
    assert!(matches!(e, Error::WindowTooLong { samples: 300_000, max_samples: 240_160 }));
    let msg = e.to_string();
    assert!(msg.contains("300000") && msg.contains("240160"), "{msg}");
}

#[test]
fn strict_production_layout_is_fp32_static_with_mel() {
    assert_eq!(
        StrictLayout::PRODUCTION,
        StrictLayout { require_mel_nemo: true, allow_int8: false, require_static_window: true }
    );
}

#[test]
fn strict_load_refuses_a_directory_without_the_mel_stage() {
    let d = scratch("strict-nomel");
    std::fs::write(d.join("vocab.txt"), "<blk> 0\n").unwrap();
    std::fs::write(d.join("config.json"), "{}").unwrap();
    let err = match Transducer::load_strict(&d, StrictLayout::PRODUCTION) {
        Ok(_) => panic!("strict load must refuse a directory without mel_nemo.onnx"),
        Err(e) => e,
    };
    assert!(matches!(&err, Error::Config(m) if m.contains("mel_nemo")), "{err}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn strict_load_refuses_an_int8_only_encoder_when_int8_is_not_allowed() {
    let d = scratch("strict-int8");
    std::fs::write(d.join("vocab.txt"), "<blk> 0\n").unwrap();
    std::fs::write(d.join("config.json"), "{}").unwrap();
    std::fs::write(d.join("mel_nemo.onnx"), b"").unwrap();
    std::fs::write(d.join("encoder-model.int8.onnx"), b"").unwrap();
    let err = match Transducer::load_strict(&d, StrictLayout::PRODUCTION) {
        Ok(_) => panic!("strict load must not select an int8 encoder"),
        Err(e) => e,
    };
    assert!(matches!(&err, Error::Config(m) if m.contains("int8")), "{err}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn pieces_are_shifted_to_absolute_samples_per_window() {
    let audio = tone(60 * 16_000);
    let window_lens = RefCell::new(Vec::new());
    let (_, pieces) = longform::transcribe_padded_window_with_pieces(
        &audio,
        &opts(false, PadPolicy::FinalSegmentOnly),
        None,
        &|_p: &LongFormProgress| {},
        |seg| {
            window_lens.borrow_mut().push(seg.len());
            let piece = TimedPiece {
                token_id: 0,
                text: "x".into(),
                raw: "x".into(),
                token_index: 0,
                frame: 0,
                start_sample: 0,
                end_sample: seg.len() as u64,
                dur_frames: 1,
                frame_samples: 1280,
                sample_rate: 16_000,
            };
            Ok((empty(), vec![piece]))
        },
    )
    .unwrap();
    let lens = window_lens.into_inner();
    assert!(lens.len() > 1, "fixture must span several windows");
    assert_eq!(pieces.len(), lens.len());
    let mut base = 0u64;
    for (piece, len) in pieces.iter().zip(&lens) {
        assert_eq!(piece.start_sample, base, "window offset accumulates in samples");
        assert_eq!(piece.end_sample, base + *len as u64);
        assert_eq!(piece.sample_rate, 16_000);
        assert_eq!(piece.token_index as usize, pieces.iter().position(|p| p == piece).unwrap());
        base += *len as u64;
    }
    assert_eq!(base as usize, audio.len(), "windows tile the recording exactly");
}

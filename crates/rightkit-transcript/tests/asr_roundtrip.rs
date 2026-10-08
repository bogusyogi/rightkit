//! End-to-end: a real `rightkit-asr` decode (CoreML Parakeet, real fixture
//! audio) flows through the contract, segmentation, SRT/VTT/JSON export and
//! back through the importers. Content-validated, no mocks.
//!
//! Gate: the CoreML Parakeet bundle (`RIGHTKIT_PARAKEET_COREML_DIR`, or the
//! shared-store / HeardRight default). Missing gate FAILS unless
//! `RIGHTKIT_ALLOW_MISSING_MODELS` is set (then SKIP is printed).
use rightkit_transcript::*;

#[cfg(target_os = "macos")]
mod asr {
    use super::*;
    use rightkit_asr::longform::{group_cues, timed_exports, timed_pieces_to_words};
    use rightkit_asr::{CoreMlParakeet, CoreMlParakeetOptions};
    use std::path::{Path, PathBuf};

    fn bundle() -> Option<PathBuf> {
        let home = std::env::var("HOME").ok()?;
        let app = format!("{home}/Library/Application Support");
        std::env::var_os("RIGHTKIT_PARAKEET_COREML_DIR")
            .map(PathBuf::from)
            .into_iter()
            .chain([
                PathBuf::from(format!("{app}/HeardRightNext/models/parakeet-tdt-v3")),
                PathBuf::from(format!("{app}/Orthic Labs/shared/models/parakeet-tdt-v3-coreml-compiled-5f8888117f47")),
            ])
            .find(|p| p.exists())
    }

    fn audio(name: &str) -> (Vec<f32>, u64) {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../rightkit-asr/tests/fixtures").join(name);
        let mut r = hound::WavReader::open(p).unwrap();
        assert_eq!((r.spec().sample_rate, r.spec().channels), (16_000, 1));
        let s: Vec<f32> = r.samples::<i16>().map(|s| s.unwrap() as f32 / 32768.0).collect();
        let ms = s.len() as u64 * 1000 / 16_000;
        (s, ms)
    }

    #[test]
    fn real_asr_output_round_trips_through_srt_vtt_and_json() {
        let Some(dir) = bundle() else {
            if std::env::var_os("RIGHTKIT_ALLOW_MISSING_MODELS").is_some() {
                eprintln!("SKIP: CoreML Parakeet bundle");
                return;
            }
            panic!("gate missing: CoreML Parakeet bundle (RIGHTKIT_PARAKEET_COREML_DIR)");
        };
        let engine = CoreMlParakeet::load(&dir, CoreMlParakeetOptions::default()).expect("load Parakeet");
        let (samples, dur_ms) = audio("short_9s.wav");
        let decoded = engine.transcribe_long(&samples, None, &|_| {}).expect("decode");
        let asr_words = timed_pieces_to_words(&decoded.result.tokens);
        assert!(asr_words.len() >= 8, "real decode should yield words: {}", decoded.result.text);

        // Engine -> contract.
        let timed = words_from_seconds(asr_words.iter().map(|w| (w.text.as_str(), w.start, w.end)));
        let tr = Transcript::from_words(
            &timed,
            "en",
            Some(dur_ms),
            ModelRef { id: "parakeet-tdt-v3".into(), revision: "coreml".into(), backend: "coreml".into() },
            EndSource::Model,
        );
        tr.validate().expect("real timings satisfy the contract");
        let lower = tr.text().to_lowercase();
        for kw in ["timer", "minutes", "oven", "mangoes", "bananas", "water"] {
            assert!(lower.contains(kw), "keyword {kw} missing from {lower}");
        }
        let back = Transcript::from_json(&tr.to_json()).unwrap();
        assert_eq!(back, tr, "JSON round trip is exact");

        // Segmentation + exports.
        let cfg = CaptionConfig::for_profile(Profile::Standard);
        let cues = build_cues(&tr.timed_words(), &cfg);
        assert!(!cues.is_empty());
        assert_eq!(check_alignment(&cues, &tr.timed_words(), ALIGNMENT_TOLERANCE_MS), vec![]);
        let srt = to_srt(&cues, Newline::Lf);
        let vtt = to_vtt(&cues, Newline::Crlf);
        assert!(vtt.starts_with("WEBVTT\r\n"));

        // Import our own exports: cue text and times survive exactly.
        for parsed in [parse_srt(&srt).unwrap(), parse_vtt(&vtt).unwrap()] {
            assert_eq!(parsed.len(), cues.len());
            for (p, c) in parsed.iter().zip(&cues) {
                assert_eq!((p.start_ms, p.end_ms), (c.start_ms, c.end_ms));
                assert_eq!(p.lines, c.lines);
            }
            let words: Vec<String> = words_from_cues(&parsed).into_iter().map(|w| w.text).collect();
            let orig: Vec<String> = tr.words.iter().map(|w| w.text.clone()).collect();
            assert_eq!(words, orig, "word sequence survives the subtitle round trip");
        }

        // Import the engine's own SRT/VTT/timing JSON: same words, same span.
        let exports = timed_exports(&decoded.result.tokens);
        let (e_srt, e_vtt, e_json) = (exports.srt.unwrap(), exports.vtt.unwrap(), exports.json.unwrap());
        let engine_cues = group_cues(&asr_words);
        let from_srt = parse_srt(&e_srt).unwrap();
        assert_eq!(from_srt.len(), engine_cues.len());
        assert_eq!(parse_vtt(&e_vtt).unwrap(), from_srt, "engine SRT and VTT carry identical cues");
        let interp: Vec<String> = words_from_cues(&from_srt).into_iter().map(|w| w.text).collect();
        assert_eq!(interp, tr.words.iter().map(|w| w.text.clone()).collect::<Vec<_>>());
        let json_words = parse_timing_json(&e_json).unwrap();
        assert_eq!(json_words, timed, "engine timing JSON imports to the same word timings (ms)");

        // Our timing JSON is readable by the same importer.
        assert_eq!(parse_timing_json(&to_timing_json(&cues, &tr.timed_words())).unwrap(), tr.timed_words());

        // Interpolated transcript from subtitles validates and keeps the span.
        let imp = Transcript::from_words(&words_from_cues(&from_srt), "en", None, ModelRef::default(), EndSource::Interpolated);
        imp.validate().unwrap();
        assert!(imp.duration_ms <= dur_ms + 100);
    }
}

//! Subtitle and transcript contract round trips that need no ASR engine.
//! The engine-backed end-to-end test lives in `asr_roundtrip.rs` and runs only
//! with `--features asr-roundtrip`.
use rightkit_transcript::*;

#[test]
fn hand_written_subtitle_files_with_real_world_quirks_parse() {
    let srt = "\u{feff}1\r\n00:00:01,000 --> 00:00:03,500\r\n<i>Hello</i> &amp; welcome\r\nto the show\r\n\r\n2\r\n00:00:04.000 --> 00:00:05.250\r\nSecond cue.\r\n\r\n";
    let c = parse_srt(srt).unwrap();
    assert_eq!(c.len(), 2);
    assert_eq!((c[0].start_ms, c[0].end_ms), (1000, 3500));
    assert_eq!(c[0].lines, vec!["Hello & welcome", "to the show"]);
    assert_eq!((c[1].start_ms, c[1].end_ms), (4000, 5250));

    let vtt = "WEBVTT - title\n\nNOTE a comment\n\nintro\n00:01.000 --> 00:02.500 align:start position:0%\n<v Ann>Hi there</v>\n\n01:00:00.000 --> 01:00:01.000\nLate\n";
    let c = parse_vtt(vtt).unwrap();
    assert_eq!(c.len(), 2);
    assert_eq!(
        (c[0].start_ms, c[0].end_ms, c[0].lines[0].as_str()),
        (1000, 2500, "Hi there")
    );
    assert_eq!(c[1].start_ms, 3_600_000);
    assert!(matches!(
        parse_srt("1\n00:00:xx,000 --> 00:00:01,000\nbad\n"),
        Err(ParseError::Timestamp(_))
    ));
}

#[test]
fn legacy_cutright_transcripts_load_and_overlaps_are_rejected() {
    let legacy = r#"{"schema":"cutright.transcript.v1","source_blake3":"ab","language":"en","duration_ms":2000,
      "words":[{"text":"hi","start_ms":0,"end_ms":400,"end_source":"next_token_start"},{"text":"there","start_ms":500,"end_ms":900,"confidence":0.9}],
      "vad":[{"start_ms":0,"end_ms":1000}],"model":{"id":"m","revision":"r","backend":"coreml"}}"#;
    let t = Transcript::from_json(legacy).unwrap();
    assert_eq!(t.words[0].end_source, EndSource::NextTokenStart);
    let bad = legacy.replace("\"start_ms\":500", "\"start_ms\":300");
    assert!(matches!(
        Transcript::from_json(&bad),
        Err(TranscriptError::Unsorted { index: 1 })
    ));
}

#[test]
fn long_speech_is_segmented_within_reading_limits() {
    // 60 words, 300 ms each, with a 1.2 s pause after word 30: real cue limits apply.
    let mut words = Vec::new();
    let mut t = 0u64;
    for i in 0..60 {
        if i == 30 {
            t += 1200;
        }
        words.push(TimedWord::new(
            format!("word{i}{}", if i % 12 == 11 { "." } else { "" }),
            t,
            t + 300,
        ));
        t += 300;
    }
    let cfg = CaptionConfig::for_profile(Profile::Vertical);
    let cues = build_cues(&words, &cfg);
    assert!(cues.len() >= 6);
    for c in &cues {
        assert!(c.lines.len() <= cfg.max_lines);
        assert!(c.end_ms - c.start_ms <= cfg.max_cue_ms);
        assert!(c
            .lines
            .iter()
            .all(|l| l.chars().count() <= cfg.max_chars_per_line));
    }
    assert_eq!(
        check_alignment(&cues, &words, ALIGNMENT_TOLERANCE_MS),
        vec![]
    );
    let covered: usize = cues.iter().map(|c| c.last_word - c.first_word + 1).sum();
    assert_eq!(covered, 60, "every word lands in exactly one cue");
}

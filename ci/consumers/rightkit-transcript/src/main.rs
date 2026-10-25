// rightkit-transcript 0.4.0 consumed by exact registry pin, outside the workspace.
//
// Proves the public transcript contract end to end with no engine, audio, model or file: schema
// constants; word construction and the seconds adapter; normalisation; Transcript build, validate,
// lenient and strict JSON reads, legacy schema writes and the duration clamp; caption cue building
// for every profile and with a width callback; the engine-side grouper; SRT, WebVTT, plain text and
// timing JSON export; SRT, WebVTT and timing JSON import; and the cut-boundary time mapping that
// keeps the source span. Every error path asserted names the exact variant. Public API only.
// Does not prove: transcription itself (an engine such as rightkit-asr produces the words), audio
// decoding, or any font or renderer (the width callback here is a fixed table).
// State: in-memory strings only. No network, no files, no process spawn, no user data.
use rightkit_transcript::{
    build_cues, build_cues_with_metrics, check_alignment, cues_from_spans, group_words,
    normalize_words, parse_srt, parse_timing_json, parse_vtt, to_plain_text, to_srt,
    to_timing_json, to_vtt, words_from_cues, words_from_seconds, words_from_transcript, wrap_lines,
    AlignmentIssue, CaptionConfig, Cue, EndSource, GroupRules, LineMetrics, ModelRef, Newline,
    ParseError, Profile, Span, TimedWord, Transcript, TranscriptError, Word, ACCEPTED_SCHEMAS,
    ALIGNMENT_TOLERANCE_MS, LEGACY_SCHEMA, MAP_DURATION_TOLERANCE_MS, SCHEMA,
};
use serde_json::json;

fn sample_words() -> Vec<TimedWord> {
    vec![
        TimedWord::new("Good", 0, 320),
        TimedWord::new("morning", 400, 880),
        TimedWord::new("everyone.", 960, 1520),
        // A 1.5 s pause is longer than every profile's max gap, so a new cue starts here.
        TimedWord::new("Today", 3000, 3400),
        TimedWord::new("we", 3450, 3600),
        TimedWord::new("ship", 3650, 4100),
        TimedWord::new("captions.", 4150, 4900),
    ]
}

fn schema_constants_and_word_construction() {
    assert_eq!(SCHEMA, "rightkit.transcript.v1");
    assert_eq!(LEGACY_SCHEMA, "cutright.transcript.v1");
    assert_eq!(ACCEPTED_SCHEMAS, [SCHEMA, LEGACY_SCHEMA]);
    assert_eq!(MAP_DURATION_TOLERANCE_MS, 20);
    assert_eq!(ALIGNMENT_TOLERANCE_MS, 100);

    let word = TimedWord::new("hello", 1500, 2250);
    assert_eq!(
        word.source_ms, None,
        "constructors default source_ms to None"
    );
    assert_eq!(word.start_secs(), 1.5);
    assert_eq!(word.end_secs(), 2.25);
    assert_eq!(TimedWord::from(("hello", 1500, 2250)), word);
    assert_eq!(TimedWord::from_seconds("hello", 1.5, 2.25), word);

    // Seconds adapter: nearest millisecond, negative and non-finite times clamp to zero.
    let adapted = words_from_seconds([
        ("a", 0.0004, 0.0016),
        ("b", -1.0, f32::NAN),
        ("c", f32::INFINITY, 2.0),
    ]);
    assert_eq!(
        adapted,
        vec![
            TimedWord::new("a", 0, 2),
            TimedWord::new("b", 0, 0),
            TimedWord::new("c", 0, 2000),
        ]
    );

    // Wire form: source_ms is omitted when absent, written when present, and optional on read.
    assert_eq!(
        serde_json::to_value(&word).unwrap(),
        json!({"text": "hello", "start_ms": 1500, "end_ms": 2250})
    );
    let mut mapped = word.clone();
    mapped.source_ms = Some((100, 110));
    let wire = serde_json::to_value(&mapped).unwrap();
    assert_eq!(wire["source_ms"], json!([100, 110]));
    assert_eq!(serde_json::from_value::<TimedWord>(wire).unwrap(), mapped);
}

fn normalisation() {
    let messy = vec![
        TimedWord::new("  spaced   out  ", 0, 500),
        TimedWord::new("   ", 500, 900),
        TimedWord::new("zero", 900, 900),
        // Overlaps the previous word, which is clamped to end where this one starts.
        TimedWord::new("overlap", 400, 800),
        TimedWord::new("tail", 800, 1000),
    ];
    let clean = normalize_words(&messy);
    assert_eq!(
        clean,
        vec![
            TimedWord::new("spaced out", 0, 400),
            TimedWord::new("overlap", 400, 800),
            TimedWord::new("tail", 800, 1000),
        ]
    );
    assert!(normalize_words(&[]).is_empty());
}

fn transcript_contract() {
    let model = ModelRef {
        id: "consumer-fixture".into(),
        revision: "r1".into(),
        backend: "fixture".into(),
    };
    let transcript = Transcript::from_words(
        &sample_words(),
        "en",
        Some(6000),
        model.clone(),
        EndSource::Model,
    );
    assert_eq!(transcript.schema, SCHEMA);
    assert_eq!(transcript.duration_ms, 6000);
    assert_eq!(transcript.words.len(), 7);
    assert_eq!(
        transcript.text(),
        "Good morning everyone. Today we ship captions."
    );
    assert_eq!(transcript.timed_words(), sample_words());
    transcript.validate().expect("a built transcript validates");

    // The duration is never shorter than the last word end.
    let grown = Transcript::from_words(
        &sample_words(),
        "en",
        Some(10),
        ModelRef::default(),
        EndSource::Vad,
    );
    assert_eq!(grown.duration_ms, 4900);
    assert!(grown.words.iter().all(|w| w.end_source == EndSource::Vad));
    let none = Transcript::from_words(&[], "en", None, ModelRef::default(), EndSource::Model);
    assert_eq!((none.duration_ms, none.words.len()), (0, 0));

    // JSON round trip keeps every field, and the lenient reader accepts what the writer wrote.
    let json_text = transcript.to_json();
    assert_eq!(Transcript::from_json(&json_text).unwrap(), transcript);

    // Legacy schema id: only ids in ACCEPTED_SCHEMAS can be written, and they read back.
    let legacy_text = transcript.to_json_with_schema_id(LEGACY_SCHEMA).unwrap();
    assert!(legacy_text.contains("\"cutright.transcript.v1\""));
    let legacy = Transcript::from_json(&legacy_text).unwrap();
    assert_eq!(
        legacy.schema, LEGACY_SCHEMA,
        "a parsed transcript keeps the id it was read with"
    );
    assert_eq!(legacy.words, transcript.words);
    assert_eq!(
        transcript.to_json_with_schema_id("other.transcript.v9"),
        Err(TranscriptError::Schema("other.transcript.v9".into()))
    );
    assert_eq!(
        transcript.schema, SCHEMA,
        "writing with another id does not change the value"
    );

    // Lenient reads default absent fields and ignore unknown ones.
    let lenient = Transcript::from_json(
        r#"{"schema":"rightkit.transcript.v1","duration_ms":1000,"words":[{"text":"hi","start_ms":0,"end_ms":400}],"extra":true}"#,
    )
    .unwrap();
    assert_eq!(lenient.source_blake3, "");
    assert_eq!(lenient.language, "");
    assert!(lenient.vad.is_empty());
    assert_eq!(lenient.model, ModelRef::default());
    assert_eq!(lenient.words[0].end_source, EndSource::Model);
    assert_eq!(lenient.words[0].confidence, None);

    // Strict reads (CutRight rules): unknown fields rejected, required fields reported by name.
    let strict_doc = r#"{"schema":"rightkit.transcript.v1","source_blake3":"abc123","language":"en","duration_ms":1000,"words":[{"text":"hi","start_ms":0,"end_ms":400,"confidence":0.5,"end_source":"next_token_start"}],"vad":[{"start_ms":0,"end_ms":600}],"model":{"id":"m","revision":"r","backend":"b"}}"#;
    let strict = Transcript::from_json_strict(strict_doc).unwrap();
    assert_eq!(strict.source_blake3, "abc123");
    assert_eq!(strict.words[0].confidence, Some(0.5));
    assert_eq!(strict.words[0].end_source, EndSource::NextTokenStart);
    assert_eq!(
        strict.vad,
        vec![Span {
            start_ms: 0,
            end_ms: 600
        }]
    );
    assert_eq!(Transcript::from_json(strict_doc).unwrap(), strict);
    assert!(matches!(
        Transcript::from_json_strict(
            &strict_doc.replace("\"language\":\"en\",", "\"language\":\"en\",\"extra\":1,")
        ),
        Err(TranscriptError::Json(_))
    ));
    assert_eq!(
        Transcript::from_json_strict(&strict_doc.replace("\"abc123\"", "\"\"")),
        Err(TranscriptError::MissingField {
            field: "source_blake3"
        })
    );
    assert_eq!(
        Transcript::from_json_strict(&strict_doc.replace("\"language\":\"en\",", "")),
        Err(TranscriptError::MissingField { field: "language" })
    );
    assert_eq!(
        Transcript::from_json_strict(&strict_doc.replace(",\"revision\":\"r\"", "")),
        Err(TranscriptError::MissingField {
            field: "model.revision"
        })
    );

    // Validation error paths, each by exact variant.
    let word = |text: &str, start_ms: u64, end_ms: u64| Word {
        text: text.into(),
        start_ms,
        end_ms,
        confidence: None,
        end_source: EndSource::Model,
    };
    let base = |words: Vec<Word>, vad: Vec<Span>| Transcript {
        schema: SCHEMA.into(),
        source_blake3: "ab".into(),
        language: "en".into(),
        duration_ms: 1000,
        words,
        vad,
        model: ModelRef::default(),
    };
    assert_eq!(
        base(vec![word("a", 100, 100)], vec![]).validate(),
        Err(TranscriptError::EmptyWord { index: 0 })
    );
    assert_eq!(
        base(vec![word("a", 0, 500), word("b", 400, 600)], vec![]).validate(),
        Err(TranscriptError::Unsorted { index: 1 })
    );
    assert_eq!(
        base(vec![word("a", 0, 1200)], vec![]).validate(),
        Err(TranscriptError::PastEnd {
            kind: "word",
            index: 0
        })
    );
    assert_eq!(
        base(
            vec![],
            vec![Span {
                start_ms: 500,
                end_ms: 400
            }]
        )
        .validate(),
        Err(TranscriptError::BadVad { index: 0 })
    );
    assert_eq!(
        base(
            vec![],
            vec![Span {
                start_ms: 0,
                end_ms: 2000
            }]
        )
        .validate(),
        Err(TranscriptError::PastEnd {
            kind: "vad span",
            index: 0
        })
    );
    let mut foreign = base(vec![], vec![]);
    foreign.schema = "foreign.v1".into();
    assert_eq!(
        foreign.validate(),
        Err(TranscriptError::Schema("foreign.v1".into()))
    );
    assert!(matches!(
        Transcript::from_json("not json"),
        Err(TranscriptError::Json(_))
    ));
    assert!(matches!(
        Transcript::from_json(&json_text.replace("rightkit.transcript.v1", "foreign.v1")),
        Err(TranscriptError::Schema(_))
    ));

    // Duration clamp: never grows, keeps every word, marks the words it moved, drops empty VAD.
    let mut clamped = Transcript::from_words(
        &[
            TimedWord::new("one", 0, 400),
            TimedWord::new("two", 400, 800),
            TimedWord::new("three", 800, 1000),
        ],
        "en",
        Some(1000),
        ModelRef::default(),
        EndSource::Model,
    );
    clamped.vad = vec![
        Span {
            start_ms: 0,
            end_ms: 900,
        },
        Span {
            start_ms: 950,
            end_ms: 1000,
        },
    ];
    clamped.constrain_duration(900).unwrap();
    assert_eq!(clamped.duration_ms, 900);
    assert_eq!(clamped.words[2].end_ms, 900);
    assert_eq!(clamped.words[2].end_source, EndSource::SourceBoundary);
    assert_eq!(clamped.words[0].end_source, EndSource::Model);
    assert_eq!(
        clamped.vad,
        vec![Span {
            start_ms: 0,
            end_ms: 900
        }]
    );
    assert!(clamped.to_json().contains("\"source_boundary\""));
    clamped.constrain_duration(5000).unwrap();
    assert_eq!(
        clamped.duration_ms, 900,
        "the clamp never grows the duration"
    );
    assert_eq!(
        clamped.constrain_duration(2),
        Err(TranscriptError::PastEnd {
            kind: "word",
            index: 0
        }),
        "less room than words is refused"
    );
    assert_eq!(
        clamped.duration_ms, 900,
        "a refused clamp leaves the transcript untouched"
    );
}

fn cues_and_exports() {
    let words = sample_words();
    let cues = build_cues(&words, &CaptionConfig::default());
    assert_eq!(
        cues.iter().map(Cue::text).collect::<Vec<_>>(),
        ["Good morning everyone.", "Today we ship captions."]
    );
    assert_eq!(
        cues.iter()
            .map(|c| (c.start_ms, c.end_ms, c.first_word, c.last_word))
            .collect::<Vec<_>>(),
        [(0, 1520, 0, 2), (3000, 4900, 3, 6)]
    );
    assert_eq!(
        check_alignment(&cues, &words, ALIGNMENT_TOLERANCE_MS),
        vec![]
    );

    // Profiles carry their documented limits and are all deterministic.
    for (profile, chars, cue_ms) in [
        (Profile::Standard, 42, 7000),
        (Profile::Vertical, 20, 4500),
        (Profile::Compact, 24, 4500),
    ] {
        let cfg = CaptionConfig::for_profile(profile);
        assert_eq!(
            (cfg.max_chars_per_line, cfg.max_lines, cfg.max_cue_ms),
            (chars, 2, cue_ms)
        );
        let built = build_cues(&words, &cfg);
        assert_eq!(built, build_cues(&words, &cfg));
        assert!(built.iter().all(|c| c.lines.len() <= cfg.max_lines));
        assert!(built
            .iter()
            .all(|c| c.lines.iter().all(|l| l.chars().count() <= chars)));
        assert_eq!(
            check_alignment(&built, &words, ALIGNMENT_TOLERANCE_MS),
            vec![]
        );
    }
    assert_eq!(
        CaptionConfig::default(),
        CaptionConfig::for_profile(Profile::Standard)
    );
    assert!(build_cues(&[], &CaptionConfig::default()).is_empty());

    let srt = to_srt(&cues, Newline::Lf);
    assert_eq!(
        srt,
        "1\n00:00:00,000 --> 00:00:01,520\nGood morning everyone.\n\n2\n00:00:03,000 --> 00:00:04,900\nToday we ship captions.\n\n"
    );
    assert_eq!(to_srt(&cues, Newline::Crlf), srt.replace('\n', "\r\n"));
    assert_eq!(
        to_vtt(&cues, Newline::Lf),
        "WEBVTT\n\n00:00:00.000 --> 00:00:01.520\nGood morning everyone.\n\n00:00:03.000 --> 00:00:04.900\nToday we ship captions.\n\n"
    );
    assert_eq!(
        to_plain_text(&cues),
        "Good morning everyone.\nToday we ship captions."
    );
    let timing: serde_json::Value = serde_json::from_str(&to_timing_json(&cues, &words)).unwrap();
    assert_eq!(timing["segments"].as_array().unwrap().len(), 2);
    assert_eq!(timing["words"].as_array().unwrap().len(), 7);
    assert_eq!(timing["segments"][1]["start"], 3.0);
    assert_eq!(
        timing["words"][1],
        json!({"text": "morning", "start": 0.4, "end": 0.88})
    );

    // Export makes cue text safe: `-->` cannot read as a timing line, VTT escapes markup.
    let hostile = vec![Cue {
        start_ms: 0,
        end_ms: 1000,
        lines: vec!["a --> b".into(), "x < y & z".into(), "   ".into()],
        first_word: 0,
        last_word: 0,
    }];
    assert_eq!(
        to_srt(&hostile, Newline::Lf),
        "1\n00:00:00,000 --> 00:00:01,000\na -> b\nx < y & z\n\n"
    );
    assert_eq!(
        to_vtt(&hostile, Newline::Lf),
        "WEBVTT\n\n00:00:00.000 --> 00:00:01.000\na -&gt; b\nx &lt; y &amp; z\n\n"
    );

    // Alignment reports each issue kind by variant.
    let off = vec![
        Cue {
            start_ms: 5000,
            end_ms: 6000,
            lines: vec!["x".into()],
            first_word: 0,
            last_word: 0,
        },
        Cue {
            start_ms: 5500,
            end_ms: 5500,
            lines: vec!["y".into()],
            first_word: 0,
            last_word: 0,
        },
    ];
    let issues = check_alignment(&off, &words, ALIGNMENT_TOLERANCE_MS);
    assert!(issues.contains(&AlignmentIssue::StartNotOnWord {
        cue: 0,
        start_ms: 5000
    }));
    assert!(issues.contains(&AlignmentIssue::EndNotOnWord {
        cue: 0,
        end_ms: 6000
    }));
    assert!(issues.contains(&AlignmentIssue::Empty { cue: 1 }));
    assert!(issues.contains(&AlignmentIssue::Overlap { cue: 1 }));
}

fn import_and_round_trip() {
    let words = sample_words();
    let cues = build_cues(&words, &CaptionConfig::default());

    // SRT written by this crate parses back to the same times and lines.
    let parsed = parse_srt(&to_srt(&cues, Newline::Lf)).unwrap();
    assert_eq!(parsed.len(), 2);
    assert_eq!((parsed[0].start_ms, parsed[0].end_ms), (0, 1520));
    assert_eq!(parsed[1].lines, ["Today we ship captions."]);
    let parsed_vtt = parse_vtt(&to_vtt(&cues, Newline::Crlf)).unwrap();
    assert_eq!(parsed_vtt, parsed);

    // Interpolated words tile each cue exactly, and the round trip keeps the text.
    let interpolated = words_from_cues(&parsed);
    assert_eq!(interpolated.first().unwrap().start_ms, 0);
    assert_eq!(interpolated.iter().filter(|w| w.end_ms <= 1520).count(), 3);
    assert_eq!(interpolated.last().unwrap().end_ms, 4900);
    assert!(interpolated
        .windows(2)
        .all(|p| p[0].end_ms <= p[1].start_ms));
    let reimported = Transcript::from_words(
        &interpolated,
        "en",
        None,
        ModelRef::default(),
        EndSource::Interpolated,
    );
    assert_eq!(
        reimported.text(),
        "Good morning everyone. Today we ship captions."
    );
    reimported.validate().unwrap();

    // Real-world quirks: BOM, CRLF, markup tags, entities, cue settings, NOTE blocks, short stamps.
    let srt = "\u{feff}1\r\n00:00:01,000 --> 00:00:03,500\r\n<i>Hello</i> &amp; welcome\r\nto the show\r\n\r\n2\r\n00:00:04.000 --> 00:00:05.250\r\nSecond cue.\r\n\r\n";
    let quirky = parse_srt(srt).unwrap();
    assert_eq!(quirky.len(), 2);
    assert_eq!((quirky[0].start_ms, quirky[0].end_ms), (1000, 3500));
    assert_eq!(quirky[0].lines, ["Hello & welcome", "to the show"]);
    let vtt = "WEBVTT - title\n\nNOTE a comment\n\nintro\n00:01.000 --> 00:02.500 align:start position:0%\n<v Ann>Hi there</v>\n\n01:00:00.000 --> 01:00:01.000\nLate\n";
    let quirky_vtt = parse_vtt(vtt).unwrap();
    assert_eq!(quirky_vtt.len(), 2);
    assert_eq!(
        (
            quirky_vtt[0].start_ms,
            quirky_vtt[0].end_ms,
            quirky_vtt[0].lines[0].as_str()
        ),
        (1000, 2500, "Hi there")
    );
    assert_eq!(quirky_vtt[1].start_ms, 3_600_000);
    assert_eq!(
        parse_srt("1\n00:00:xx,000 --> 00:00:01,000\nbad\n"),
        Err(ParseError::Timestamp("00:00:xx,000 ".into()))
    );
    assert!(parse_srt("no cues here").unwrap().is_empty());

    // Timing JSON: seconds on the wire, milliseconds in memory.
    let timing = to_timing_json(&cues, &words);
    assert_eq!(parse_timing_json(&timing).unwrap(), words);
    assert!(matches!(parse_timing_json("{}"), Err(ParseError::Json(_))));
    assert!(matches!(
        parse_timing_json("nope"),
        Err(ParseError::Json(_))
    ));
    assert!(matches!(
        parse_timing_json(r#"{"words":[{"text":"x","start":0.0}]}"#),
        Err(ParseError::Json(_))
    ));
}

fn measured_layout_and_grouping() {
    // A fixed width table stands in for a renderer: wide letters cost more than narrow ones.
    let measure = |text: &str| -> Result<u64, String> {
        Ok(text
            .chars()
            .map(|c| match c {
                'W' | 'M' => 10,
                'i' | 'l' => 1,
                ' ' => 2,
                _ => 5,
            })
            .sum())
    };
    let metrics = LineMetrics {
        measure_width: &measure,
        max_width_px: 40,
        uppercase: false,
    };
    let wide: Vec<TimedWord> = ["WWWW", "WWWM", "WWMW", "WMWW"]
        .iter()
        .enumerate()
        .map(|(i, t)| TimedWord::new(*t, 200 + i as u64 * 800, 1000 + i as u64 * 800))
        .collect();
    let narrow: Vec<TimedWord> = ["iiii", "iiil", "iili", "ilii"]
        .iter()
        .enumerate()
        .map(|(i, t)| TimedWord::new(*t, 200 + i as u64 * 800, 1000 + i as u64 * 800))
        .collect();
    let one_line = CaptionConfig {
        max_lines: 1,
        ..CaptionConfig::default()
    };
    // Character count alone cannot tell these apart; the measured width can.
    assert_eq!(
        build_cues(&wide, &one_line).len(),
        build_cues(&narrow, &one_line).len()
    );
    let wide_cues = build_cues_with_metrics(&wide, &one_line, Some(&metrics)).unwrap();
    let narrow_cues = build_cues_with_metrics(&narrow, &one_line, Some(&metrics)).unwrap();
    assert_eq!(wide_cues.len(), 4);
    assert_eq!(narrow_cues.len(), 1);
    assert!(wide_cues
        .iter()
        .flat_map(|c| &c.lines)
        .all(|l| measure(l).unwrap() <= 40));
    assert_eq!(
        check_alignment(&wide_cues, &wide, ALIGNMENT_TOLERANCE_MS),
        vec![]
    );
    assert_eq!(
        build_cues_with_metrics(&wide, &one_line, None).unwrap(),
        build_cues(&wide, &one_line),
        "no metrics is the plain builder"
    );
    assert_eq!(wrap_lines("", 10, None).unwrap(), Vec::<String>::new());
    assert_eq!(
        wrap_lines("alpha beta gamma", 10, None).unwrap(),
        ["alpha beta", "gamma"]
    );
    assert_eq!(
        wrap_lines("overlongwordhere", 5, None).unwrap(),
        ["overlongwordhere"]
    );
    assert_eq!(
        wrap_lines("WWWWWWWW", 20, Some(&metrics)).unwrap(),
        ["WWWW", "WWWW"]
    );

    // A failing width callback surfaces unchanged; a glyph wider than the line is refused.
    let failing = |_: &str| -> Result<u64, String> { Err("font unavailable".into()) };
    let broken = LineMetrics {
        measure_width: &failing,
        max_width_px: 40,
        uppercase: false,
    };
    assert_eq!(
        wrap_lines("hello", 10, Some(&broken)),
        Err("font unavailable".into())
    );
    let narrow_box = LineMetrics {
        measure_width: &measure,
        max_width_px: 4,
        uppercase: false,
    };
    assert!(wrap_lines("WW", 10, Some(&narrow_box))
        .unwrap_err()
        .contains("exceeds the safe line width"));

    // Engine-side grouper: word count, span, gap and sentence rules, with a minimum cue width.
    let rules = GroupRules {
        max_words: 3,
        max_span_ms: 5000,
        max_gap_ms: 700,
        sentence_min_words: 2,
        min_cue_ms: 600,
    };
    let grouped = group_words(&sample_words(), &rules);
    assert_eq!(
        grouped.iter().map(Cue::text).collect::<Vec<_>>(),
        ["Good morning everyone.", "Today we ship", "captions."]
    );
    assert!(grouped.iter().all(|c| c.end_ms - c.start_ms >= 600));
    let spans = vec![
        TimedWord::new("one sentence span", 100, 200),
        TimedWord::new("another", 300, 2000),
    ];
    let from_spans = cues_from_spans(&spans, 600);
    assert_eq!(from_spans.len(), 2);
    assert_eq!(from_spans[0].lines, ["one sentence span"]);
    assert_eq!(
        from_spans[0].end_ms - from_spans[0].start_ms,
        600,
        "short spans widen to the minimum"
    );
    assert_eq!((from_spans[1].first_word, from_spans[1].last_word), (1, 1));
}

fn cut_boundary_mapping() {
    // A cut removes 1000..2000 from the source; later words shift left by one second.
    let transcript = Transcript::from_words(
        &[
            TimedWord::new("edge", 800, 1000),
            TimedWord::new("gone", 1100, 1500),
            TimedWord::new("after", 2000, 2400),
        ],
        "en",
        Some(3000),
        ModelRef::default(),
        EndSource::Model,
    );
    let mapped = words_from_transcript(&transcript, |t| match t {
        0..1000 => Some(t),
        1000..2000 => None,
        _ => Some(t - 1000),
    });
    assert_eq!(
        mapped.len(),
        2,
        "the word inside the removed range is dropped"
    );
    assert_eq!(mapped[0].source_ms, Some((800, 1000)));
    assert_eq!((mapped[1].start_ms, mapped[1].end_ms), (1000, 1400));
    assert_eq!(mapped[1].source_ms, Some((2000, 2400)));
    // The source span survives normalisation, cue building and a JSON round trip.
    let normalised = normalize_words(&mapped);
    assert_eq!(normalised, mapped);
    let cues = build_cues(&mapped, &CaptionConfig::default());
    assert_eq!(normalised[cues[0].first_word].source_ms, Some((800, 1000)));
    let back: Vec<TimedWord> =
        serde_json::from_str(&serde_json::to_string(&normalised).unwrap()).unwrap();
    assert_eq!(back, normalised);

    // A word whose mapped length differs by more than the tolerance is dropped; at it, kept.
    let single = Transcript::from_words(
        &[TimedWord::new(" shifted ", 100, 200)],
        "en",
        Some(1000),
        ModelRef::default(),
        EndSource::Model,
    );
    let shifted = words_from_transcript(&single, |t| Some(t + 500));
    assert_eq!(
        (
            shifted[0].text.as_str(),
            shifted[0].start_ms,
            shifted[0].end_ms
        ),
        ("shifted", 600, 700)
    );
    let at_limit = words_from_transcript(&single, |t| {
        Some(if t == 200 {
            t - MAP_DURATION_TOLERANCE_MS
        } else {
            t
        })
    });
    assert_eq!(at_limit.len(), 1);
    let beyond = words_from_transcript(&single, |t| {
        Some(if t == 200 {
            t - MAP_DURATION_TOLERANCE_MS - 1
        } else {
            t
        })
    });
    assert!(beyond.is_empty());
}

fn main() {
    schema_constants_and_word_construction();
    normalisation();
    transcript_contract();
    cues_and_exports();
    import_and_round_trip();
    measured_layout_and_grouping();
    cut_boundary_mapping();
    println!("OK rightkit-transcript 0.4.0 (registry consumer)");
}

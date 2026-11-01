use rightkit_captions::*;
use rightkit_timeline::{Fps, Preset, TitleAssets};
use rightkit_transcript::{EndSource, ModelRef, Transcript, Word};

/// ~14 s of narration with a sentence break, clause commas and a pause.
fn fixture() -> Vec<TimedWord> {
    let spec: &[(&str, u64, u64)] = &[
        ("Welcome", 500, 900),
        ("back", 950, 1200),
        ("to", 1250, 1350),
        ("the", 1400, 1500),
        ("channel.", 1550, 2100),
        ("Today", 2300, 2700),
        ("we", 2750, 2850),
        ("are", 2900, 3000),
        ("building", 3050, 3500),
        ("a", 3550, 3600),
        ("video", 3650, 4000),
        ("editor,", 4050, 4500),
        ("and", 4550, 4700),
        ("it", 4750, 4850),
        ("runs", 4900, 5200),
        ("entirely", 5250, 5800),
        ("on", 5850, 5950),
        ("your", 6000, 6250),
        ("own", 6300, 6600),
        ("computer.", 6650, 7300),
        // pause > max_gap
        ("No", 9000, 9200),
        ("cloud.", 9250, 9800),
        ("Ready?", 9900, 10400),
        ("Let's", 10500, 10800),
        ("go!", 10850, 11300),
    ];
    spec.iter()
        .map(|&(t, s, e)| TimedWord::new(t, s, e))
        .collect()
}

fn golden(name: &str) -> String {
    let p = format!("{}/tests/golden/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{p}: {e}"))
}

fn check_golden(name: &str, actual: &str) {
    // Delete a golden file to regenerate it (the run then fails once, by design).
    let p = format!("{}/tests/golden/{name}", env!("CARGO_MANIFEST_DIR"));
    if !std::path::Path::new(&p).exists() {
        std::fs::write(&p, actual).unwrap();
        panic!("created golden {name}; review it and rerun");
    }
    assert_eq!(actual, golden(name), "golden {name}");
}

#[test]
fn youtube_segmentation_and_alignment() {
    let words = fixture();
    let cfg = config_for_preset(Preset::YouTube);
    let cues = build_cues(&words, &cfg);
    assert!(cues.len() >= 3);
    for c in &cues {
        assert!(c.lines.len() <= cfg.max_lines);
        for l in &c.lines {
            assert!(l.chars().count() <= cfg.max_chars_per_line, "{l}");
        }
        assert!(c.end_ms - c.start_ms <= cfg.max_cue_ms);
    }
    // The pause forces a new cue; sentence ends never sit mid-cue.
    assert!(cues.iter().any(|c| c.start_ms == 9000));
    for c in &cues {
        let text = c.text();
        let body = text.trim_end_matches(['.', '?', '!']);
        assert!(!body.contains(". "), "sentence end inside cue: {text}");
    }
    assert_eq!(
        check_alignment(&cues, &words, ALIGNMENT_TOLERANCE_MS),
        vec![]
    );
}

#[test]
fn reels_is_narrower_and_aligned() {
    let words = fixture();
    let cfg = config_for_preset(Preset::Reels);
    let cues = build_cues(&words, &cfg);
    let yt = build_cues(&words, &config_for_preset(Preset::YouTube));
    assert!(cues.len() > yt.len());
    for c in &cues {
        assert!(c.lines.len() <= 2);
        assert!(
            c.lines.iter().all(|l| l.chars().count() <= 24),
            "{:?}",
            c.lines
        );
    }
    assert_eq!(
        check_alignment(&cues, &words, ALIGNMENT_TOLERANCE_MS),
        vec![]
    );
    // Layout limit agrees with the segmentation limit on both presets.
    for p in [Preset::YouTube, Preset::Reels, Preset::TikTok] {
        let lay = CaptionLayout::for_preset(p);
        let max = lay.max_chars(p.width() as i64, p.height() as i64);
        assert!(
            config_for_preset(p).max_chars_per_line <= max,
            "{p:?} {max}"
        );
    }
}

#[test]
fn cues_never_overlap_even_with_overlapping_input() {
    let words = vec![
        TimedWord::new("one", 0, 400),
        TimedWord::new("two", 300, 700), // overlaps "one"
        TimedWord::new("three", 700, 900),
        TimedWord::new("", 900, 1000),
        TimedWord::new("zero", 1000, 1000),
    ];
    let cues = build_cues(&words, &config_for_preset(Preset::YouTube));
    for w in cues.windows(2) {
        assert!(w[0].end_ms <= w[1].start_ms);
    }
    assert_eq!(cues.len(), 1);
    assert_eq!(cues[0].text(), "one two three");
}

#[test]
fn short_cue_is_held_but_not_into_next() {
    let words = vec![
        TimedWord::new("Hi.", 1000, 1200),
        TimedWord::new("There", 1250, 1900),
        TimedWord::new("again,", 1950, 2600),
        TimedWord::new("friend.", 2650, 3300),
    ];
    let cues = build_cues(&words, &config_for_preset(Preset::YouTube));
    assert_eq!(cues.len(), 2);
    assert_eq!(cues[0].start_ms, 1000);
    // Held toward the 600 ms minimum, but capped by the next cue's start.
    assert_eq!(cues[0].end_ms, 1250);
    assert!(cues[0].end_ms <= cues[1].start_ms);
    assert_eq!(
        check_alignment(&cues, &words, ALIGNMENT_TOLERANCE_MS),
        vec![]
    );
}

#[test]
fn two_line_break_prefers_clause() {
    let words: Vec<TimedWord> = "When we cut the silence, the pace picks up nicely"
        .split(' ')
        .enumerate()
        .map(|(i, t)| TimedWord::new(t, i as u64 * 300, i as u64 * 300 + 280))
        .collect();
    let cues = build_cues(&words, &config_for_preset(Preset::Reels));
    let first = &cues[0];
    assert!(first.lines.len() <= 2);
    if first.lines.len() == 2 && first.text().contains("silence,") {
        assert!(first.lines[0].ends_with("silence,"), "{:?}", first.lines);
    }
}

#[test]
fn alignment_check_flags_bad_cues() {
    let words = fixture();
    let bad = vec![
        Cue {
            start_ms: 700,
            end_ms: 1000,
            lines: vec!["x".into()],
            first_word: 0,
            last_word: 0,
        },
        Cue {
            start_ms: 900,
            end_ms: 900,
            lines: vec!["y".into()],
            first_word: 0,
            last_word: 0,
        },
    ];
    let issues = check_alignment(&bad, &words, ALIGNMENT_TOLERANCE_MS);
    assert!(issues.contains(&AlignmentIssue::StartNotOnWord {
        cue: 0,
        start_ms: 700
    }));
    assert!(issues.contains(&AlignmentIssue::Overlap { cue: 1 }));
    assert!(issues.contains(&AlignmentIssue::Empty { cue: 1 }));
}

#[test]
fn srt_vtt_golden() {
    let words = fixture();
    let cues = build_cues(&words, &config_for_preset(Preset::YouTube));
    let srt = to_srt(&cues, Newline::Lf);
    check_golden("youtube.srt", &srt);
    check_golden("youtube.vtt", &to_vtt(&cues, Newline::Lf));
    let rcues = build_cues(&words, &config_for_preset(Preset::Reels));
    check_golden("reels.srt", &to_srt(&rcues, Newline::Lf));
    // CRLF output is the LF output with every newline doubled up, nothing else.
    let crlf = to_srt(&cues, Newline::Crlf);
    assert_eq!(crlf, srt.replace('\n', "\r\n"));
    assert!(!crlf.replace("\r\n", "").contains(['\r', '\n']));
}

#[test]
fn export_sanitizes_text_and_formats_stamps() {
    let cues = vec![Cue {
        start_ms: 3_723_004,
        end_ms: 3_724_999,
        lines: vec!["a --> b & <i>".into(), "".into(), "line\r".into()],
        first_word: 0,
        last_word: 0,
    }];
    let srt = to_srt(&cues, Newline::Lf);
    assert_eq!(
        srt,
        "1\n01:02:03,004 --> 01:02:04,999\na -> b & <i>\nline\n\n"
    );
    let vtt = to_vtt(&cues, Newline::Lf);
    assert_eq!(
        vtt,
        "WEBVTT\n\n01:02:03.004 --> 01:02:04.999\na -&gt; b &amp; &lt;i&gt;\nline\n\n"
    );
    // UTF-8 passes through untouched.
    let u = vec![Cue {
        start_ms: 0,
        end_ms: 1,
        lines: vec!["caf\u{e9} \u{1f600}".into()],
        first_word: 0,
        last_word: 0,
    }];
    assert!(to_srt(&u, Newline::Lf).contains("caf\u{e9} \u{1f600}"));
}

fn transcript(words: &[(&str, u64, u64)]) -> Transcript {
    Transcript {
        schema: rightkit_transcript::LEGACY_SCHEMA.into(),
        source_blake3: "0".repeat(64),
        language: "en".into(),
        duration_ms: 20_000,
        words: words
            .iter()
            .map(|&(t, s, e)| Word {
                text: t.into(),
                start_ms: s,
                end_ms: e,
                confidence: None,
                end_source: EndSource::Model,
            })
            .collect(),
        vad: vec![],
        model: ModelRef {
            id: "m".into(),
            revision: "r".into(),
            backend: "b".into(),
        },
    }
}

#[test]
fn transcript_mapping_drops_removed_words() {
    // Source 1000..2000 removed; later material shifts left by 1000.
    let map = |t: u64| -> Option<u64> {
        if t < 1000 {
            Some(t)
        } else if t < 2000 {
            None
        } else {
            Some(t - 1000)
        }
    };
    let tr = transcript(&[
        ("keep", 0, 500),
        ("gone", 1100, 1500),
        ("edge", 800, 1000), // ends exactly on the cut: end maps via end-1
        ("straddle", 900, 1200),
        ("after", 2000, 2400),
        ("", 2400, 2500),
    ]);
    let out = words_from_transcript(&tr, map);
    let got: Vec<(&str, u64, u64)> = out
        .iter()
        .map(|w| (w.text.as_str(), w.start_ms, w.end_ms))
        .collect();
    assert_eq!(
        got,
        vec![("keep", 0, 500), ("edge", 800, 1000), ("after", 1000, 1400)]
    );
    let cues = build_cues(&out, &config_for_preset(Preset::YouTube));
    assert_eq!(cue_source_spans(&out, &cues), vec![Some((0, 2400))]);
    let ids = cue_ids(&out, &cues, "en", &tr.source_blake3);
    let shifted: Vec<_> = out
        .iter()
        .cloned()
        .map(|mut word| {
            word.start_ms += 4000;
            word.end_ms += 4000;
            word
        })
        .collect();
    let shifted_cues = build_cues(&shifted, &config_for_preset(Preset::YouTube));
    assert_eq!(
        ids,
        cue_ids(&shifted, &shifted_cues, "en", &tr.source_blake3)
    );
}

#[test]
fn burn_filters_are_frame_exact_and_content_addressed() {
    let words = fixture();
    let fps = Fps::new(30, 1).unwrap();
    for p in [Preset::YouTube, Preset::Reels, Preset::TikTok] {
        let cues = build_cues(&words, &config_for_preset(p));
        let assets = TitleAssets {
            font: std::path::Path::new("/f/font.ttf"),
            text_dir: std::path::Path::new("/t"),
        };
        let (filters, files) = caption_filters(&cues, p, fps, 30 * 12, &assets).unwrap();
        assert_eq!(filters.len(), cues.len());
        assert_eq!(files.len(), cues.len());
        assert!(filters[0].contains("expansion=none"));
        assert!(filters[0].contains("textfile=/t/caption-"));
        // 500 ms -> frame 15; end 1200 ms hold -> frame 36 exclusive.
        let first_start = (cues[0].start_ms * 30 + 500) / 1000;
        assert!(
            filters[0].contains(&format!("between(n,{first_start},")),
            "{}",
            filters[0]
        );
        // Deterministic.
        let again = caption_filters(&cues, p, fps, 30 * 12, &assets).unwrap();
        assert_eq!(again, (filters, files));
    }
    // Cues after the end are skipped.
    let cues = build_cues(&words, &config_for_preset(Preset::YouTube));
    let assets = TitleAssets {
        font: std::path::Path::new("/f.ttf"),
        text_dir: std::path::Path::new("/t"),
    };
    let (f, _) = caption_filters(&cues, Preset::YouTube, fps, 30, &assets).unwrap();
    assert!(f.len() < cues.len());
}

/// Overlapping or out-of-order cues are all kept: the earlier one is cut back
/// to where the next starts instead of the later one being dropped.
#[test]
fn overlapping_cues_are_trimmed_not_dropped() {
    use rightkit_captions::burn::caption_filters;
    use rightkit_timeline::{Fps, Preset, TitleAssets};
    let cue = |s: u64, e: u64, t: &str| rightkit_captions::Cue {
        start_ms: s,
        end_ms: e,
        lines: vec![t.to_string()],
        first_word: 0,
        last_word: 0,
    };
    let cues = vec![
        cue(2000, 3000, "b"),
        cue(0, 2500, "a"),
        cue(2900, 4000, "c"),
    ];
    let dir = std::env::temp_dir();
    let font = dir.join("font.ttf");
    let assets = TitleAssets {
        font: &font,
        text_dir: &dir,
    };
    let (filters, files) = caption_filters(
        &cues,
        Preset::YouTube,
        Fps::new(30, 1).unwrap(),
        300,
        &assets,
    )
    .unwrap();
    assert_eq!(filters.len(), 3, "{filters:#?}");
    assert_eq!(files.len(), 3);
    assert!(filters[0].contains("between(n,0,59)"), "{}", filters[0]);
}

// Malformed/overlapping word input is not accepted by the engine transcript contract;
// exercise metadata pairing directly through caption export instead of an engine fixture.
#[test]
fn normalized_source_spans_survive_overlap_clamping_and_duplicate_starts() {
    let mut words = vec![
        TimedWord::new("discarded", 0, 400),
        TimedWord::new("  keep   this ", 0, 500),
        TimedWord::new("", 200, 300),
        TimedWord::new("word.", 400, 900),
    ];
    for (i, word) in words.iter_mut().enumerate() {
        word.source_ms = Some((1000 + i as u64 * 500, 1400 + i as u64 * 500));
    }
    let cues = build_cues(&words, &config_for_preset(Preset::YouTube));
    assert_eq!(cues.len(), 1);
    assert_eq!((cues[0].first_word, cues[0].last_word), (0, 1));
    assert_eq!(cue_source_spans(&words, &cues), vec![Some((1500, 2900))]);
    assert!(to_srt(&cues, Newline::Lf).contains("keep this word."));
    assert!(check_alignment(&cues, &words, ALIGNMENT_TOLERANCE_MS).is_empty());
}

//! Font-independent equivalents of CutRight's measured-layout scenarios.
use rightkit_transcript::*;

// Distinct advances expose character-only layout & uppercase measurement bugs.
fn measure(text: &str) -> Result<u64, String> {
    Ok(text
        .chars()
        .map(|c| match c {
            'W' | 'M' => 10,
            'I' | 'L' => 4,
            'i' | 'l' => 1,
            ' ' => 2,
            _ => 5,
        })
        .sum())
}

fn words(text: &str) -> Vec<TimedWord> {
    text.split_whitespace()
        .enumerate()
        .map(|(i, word)| TimedWord::new(word, 200 + i as u64 * 800, 1000 + i as u64 * 800))
        .collect()
}

#[test]
fn wide_and_narrow_words_use_measured_layout_with_aligned_indices() {
    let wide = words("WWWW WWWM WWMW WMWW");
    let narrow = words("iiii iiil iili ilii");
    let cfg = CaptionConfig {
        max_lines: 1,
        ..CaptionConfig::default()
    };
    let metrics = LineMetrics {
        measure_width: &measure,
        max_width_px: 40,
        uppercase: false,
    };
    assert_eq!(
        build_cues(&wide, &cfg).len(),
        build_cues(&narrow, &cfg).len()
    );
    let wide_cues = build_cues_with_metrics(&wide, &cfg, Some(&metrics)).unwrap();
    let narrow_cues = build_cues_with_metrics(&narrow, &cfg, Some(&metrics)).unwrap();
    assert_eq!(wide_cues.len(), 4);
    assert_eq!(narrow_cues.len(), 1);
    for (cues, input) in [(&wide_cues, &wide), (&narrow_cues, &narrow)] {
        assert!(check_alignment(cues, input, ALIGNMENT_TOLERANCE_MS).is_empty());
        let covered: Vec<_> = cues
            .iter()
            .flat_map(|c| c.first_word..=c.last_word)
            .collect();
        assert_eq!(covered, (0..input.len()).collect::<Vec<_>>());
        for cue in cues {
            for line in &cue.lines {
                assert!(measure(line).unwrap() <= metrics.max_width_px);
                assert!(line.chars().count() <= cfg.max_chars_per_line);
            }
        }
    }
}

#[test]
fn uppercase_character_caps_and_long_words_preserve_text_and_timing() {
    let metrics = LineMetrics {
        measure_width: &measure,
        max_width_px: 40,
        uppercase: true,
    };
    let input = words("iiii iiil iili ilii");
    let cues = build_cues_with_metrics(&input, &CaptionConfig::default(), Some(&metrics)).unwrap();
    assert_eq!(cues.len(), 1);
    assert_eq!(cues[0].lines, ["iiii iiil", "iili ilii"]);
    let capped = wrap_lines("iiii iiil iili ilii", 8, Some(&metrics)).unwrap();
    assert!(capped.iter().all(|l| l.chars().count() <= 8));
    let long = words("WWWWWWWW");
    let cues = build_cues_with_metrics(&long, &CaptionConfig::default(), Some(&metrics)).unwrap();
    assert_eq!(cues[0].lines, ["WWWW", "WWWW"]);
    assert_eq!((cues[0].first_word, cues[0].last_word), (0, 0));
    assert_eq!((cues[0].start_ms, cues[0].end_ms), (200, 1000));
}

#[test]
fn fallback_matches_existing_builder_for_every_profile() {
    let input = words("When we cut the silence, the pace picks up nicely.");
    for profile in [Profile::Standard, Profile::Vertical, Profile::Compact] {
        let cfg = CaptionConfig::for_profile(profile);
        assert_eq!(
            build_cues_with_metrics(&input, &cfg, None).unwrap(),
            build_cues(&input, &cfg)
        );
    }
    assert!(
        build_cues_with_metrics(&[], &CaptionConfig::default(), None)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn cut_boundary_mapping_preserves_source_identity_through_layout_and_json() {
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
    assert_eq!(mapped.len(), 2);
    assert_eq!(mapped[0].source_ms, Some((800, 1000)));
    assert_eq!(mapped[1].source_ms, Some((2000, 2400)));
    assert_eq!((mapped[1].start_ms, mapped[1].end_ms), (1000, 1400));
    let normalized = normalize_words(&mapped);
    let metrics = LineMetrics {
        measure_width: &measure,
        max_width_px: 80,
        uppercase: false,
    };
    let cues = build_cues_with_metrics(&mapped, &CaptionConfig::default(), Some(&metrics)).unwrap();
    assert_eq!(normalized[cues[0].first_word].source_ms.unwrap().0, 800);
    assert_eq!(
        normalized[cues.last().unwrap().last_word]
            .source_ms
            .unwrap()
            .1,
        2400
    );
    assert!(check_alignment(&cues, &mapped, ALIGNMENT_TOLERANCE_MS).is_empty());
    let back: Vec<TimedWord> =
        serde_json::from_str(&serde_json::to_string(&normalized).unwrap()).unwrap();
    assert_eq!(back, normalized);
}

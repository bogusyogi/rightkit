// Display-side detokenization helpers: TDT piece → display text and word
// grouping, both extracted so the contract is unit-testable without a model.
use super::model::{TimedPiece, TimedTok};

/// Assemble decoded TDT pieces into display text with timings. Extracted from
/// `transcribe_pieces_timed` so the detokenization contract is unit testable
/// without a loaded Core ML model.
///
/// The vocabulary carries only the bare single digits `0`-`9`, so every number
/// arrives as consecutive digit pieces: "thirteen point two" decodes to
/// `▁ 1 3 . 2`. Whether two adjacent digits belong to the same number is decided
/// solely by the `▁` word boundary, and that signal is gone once this function
/// returns a string, so the join must happen here and cannot be repaired
/// downstream.
///
/// SentencePiece omits `▁` before a digit that follows a word ("at60"), so a
/// space is inserted for that case only. The guard mirrors the ONNX decoder in
/// the ONNX detokenizer: insert only when the preceding text ends in
/// letters, never when it ends in a digit. Without it the rule fired on every
/// digit piece and split every number above 9.
/// macOS's ONE detokenization entry point.
///
/// Delegates to `rightkit_asr::parakeet_core::assemble_timed_pieces`, the same
/// function the ONNX path calls, and does nothing but map the core's
/// `TimedToken` onto the `TimedPiece` this crate exposes. Every rule — the
/// digit-boundary guard, the `▁` word rule, special-token skipping and N0 —
/// must live in this one place for all three platforms; a second,
/// hand-assembled implementation here will drift from the core silently.
pub(crate) fn assemble_display_pieces(
    hits: &[(usize, f32, usize)],
    pieces: &[String],
) -> Vec<TimedPiece> {
    assemble_display_pieces_with_policy(
        hits,
        pieces,
        rightkit_asr::parakeet_core::DetokPolicy::canonical_rust(),
    )
}

pub(crate) fn assemble_display_pieces_with_policy(
    hits: &[(usize, f32, usize)],
    pieces: &[String],
    policy: rightkit_asr::parakeet_core::DetokPolicy,
) -> Vec<TimedPiece> {
    const FRAME_SECONDS: f32 = 0.08;
    let decoded: Vec<rightkit_asr::parakeet_core::DecodedPiece> = hits
        .iter()
        .map(
            |&(id, start, dur_frames)| rightkit_asr::parakeet_core::DecodedPiece {
                token_id: id,
                piece: pieces.get(id).cloned().unwrap_or_default(),
                start,
                end: start + dur_frames as f32 * FRAME_SECONDS,
            },
        )
        .collect();
    rightkit_asr::parakeet_core::assemble_timed_pieces(&decoded, policy)
        .tokens
        .into_iter()
        .map(|tok| TimedPiece {
            text: tok.text,
            start: tok.start,
            duration: (tok.end - tok.start).max(0.0),
        })
        .collect()
}

/// Group TDT subword pieces into words, preserving the model's own predicted
/// durations. Extracted from `transcribe_timed` so the timing contract is unit
/// testable without a loaded Core ML model; mirrors `asr::timed_tokens_to_words`
/// so the short-file and windowed decode paths agree.
pub(crate) fn group_timed_words(hits: &[(usize, f32, usize)], pieces: &[String]) -> Vec<TimedTok> {
    const FRAME_SECONDS: f32 = 0.08;
    // Group subword pieces into words. A piece beginning with `▁` opens a new
    // word; the word's start is that piece's frame time.
    let mut words: Vec<TimedTok> = Vec::new();
    for &(id, start, dur_frames) in hits {
        let piece = pieces.get(id).map(|s| s.as_str()).unwrap_or("");
        let starts_word = piece.starts_with('\u{2581}');
        let clean = piece.replace('\u{2581}', "");
        if starts_word || words.is_empty() {
            words.push(TimedTok {
                text: clean,
                start,
                end: start + dur_frames as f32 * FRAME_SECONDS,
            });
        } else if let Some(last) = words.last_mut() {
            last.text.push_str(&clean);
            last.end = start + dur_frames as f32 * FRAME_SECONDS;
        }
    }
    // Word ends come from the model's own TDT-predicted durations, matching
    // `coreml_parakeet_result` + `timed_tokens_to_words` so short-file and
    // windowed decodes share ONE timing contract. A word's `end` must not
    // stretch to the next word's start -- that erases the inter-word silence
    // paragraph/routing logic needs. Two clamps only: never shorter than one
    // 80 ms frame (zero-width cues are invalid in SRT), and never past the
    // next word's start (per-token durations can otherwise overhang).
    let n = words.len();
    for i in 0..n {
        let floor = words[i].start + 0.08;
        let mut end = words[i].end.max(floor);
        if i + 1 < n {
            end = end.min(words[i + 1].start.max(floor));
        }
        words[i].end = end;
        words[i].text = words[i].text.trim().to_string();
    }
    words.retain(|w| !w.text.is_empty());
    words
}
#[cfg(test)]
mod assemble_display_pieces_tests {
    use super::assemble_display_pieces;

    /// Build a (vocab, hits) pair from token strings so cases read as the token
    /// stream the model actually emits. `_` stands for the `▁` word boundary.
    fn run(tokens: &[&str]) -> String {
        let vocab: Vec<String> = tokens.iter().map(|t| t.replace('_', "\u{2581}")).collect();
        let hits: Vec<(usize, f32, usize)> = (0..vocab.len())
            .map(|i| (i, i as f32 * 0.08, 1usize))
            .collect();
        assemble_display_pieces(&hits, &vocab)
            .into_iter()
            .map(|p| p.text)
            .collect::<String>()
            .trim()
            .to_string()
    }

    // --- N0, asserted HERE, on the production assembler ---
    //
    // These belong on `assemble_display_pieces` and not on
    // `assemble_timed_pieces`, because that is the mistake this suite exists to
    // stop repeating: N0 was wired into `assemble_timed_pieces`, tested there,
    // declared done, and shipped in 0.1.154 — while macOS production went
    // through THIS function, which never consulted the policy. Field
    // transcripts still carried five "uh" per utterance into L1.

    #[test]
    fn n0_strips_hesitations_on_the_production_path() {
        assert_eq!(run(&["_hello", "_uh", "_world"]), "hello world");
        assert_eq!(run(&["_and", "_um", "_then"]), "and then");
    }

    #[test]
    fn n0_takes_the_bracketing_comma_with_the_filler() {
        assert_eq!(run(&["_Uh", ",", "_hello"]), "hello");
    }

    #[test]
    fn n0_handles_a_filler_split_across_pieces() {
        assert_eq!(run(&["_u", "m", "_yes"]), "yes");
    }

    #[test]
    fn n0_keeps_words_that_merely_contain_a_filler() {
        assert_eq!(run(&["_umbrella"]), "umbrella");
        assert_eq!(run(&["_uhuru"]), "uhuru");
    }

    #[test]
    fn n0_leaves_unmeasured_hesitations_alone() {
        assert_eq!(run(&["_hmm", "_ok"]), "hmm ok");
    }

    #[test]
    fn n0_keeps_surviving_timings_aligned() {
        // The filler's timing must go with it; the words after must keep theirs.
        let vocab: Vec<String> = ["\u{2581}a", "\u{2581}uh", "\u{2581}b"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let hits: Vec<(usize, f32, usize)> = vec![(0, 0.0, 1), (1, 0.8, 1), (2, 1.6, 1)];
        let out = super::assemble_display_pieces(&hits, &vocab);
        let texts: Vec<String> = out.iter().map(|p| p.text.trim().to_string()).collect();
        assert_eq!(texts, ["a", "b"]);
        assert_eq!(out[1].start, 1.6, "surviving piece keeps its own start");
    }

    /// The shipped vocabulary holds only bare digits `0`-`9`, so every number
    /// above 9 arrives as consecutive digit pieces. Splitting them corrupted
    /// 68% of numeric dictations; these are the forms observed in the corpus.
    #[test]
    fn joins_split_integers() {
        assert_eq!(run(&["_static", "_", "1", "5"]), "static 15");
        assert_eq!(run(&["_is", "_", "1", "0"]), "is 10");
        assert_eq!(run(&["_last", "_", "1", "3"]), "last 13");
        assert_eq!(run(&["_the", "_", "2", "6"]), "the 26");
        assert_eq!(run(&["_hit", "_", "9", "0", "%"]), "hit 90%");
        assert_eq!(run(&["_a", "_", "2", "7", "_billion"]), "a 27 billion");
    }

    #[test]
    fn joins_split_decimals_and_versions() {
        assert_eq!(run(&["_minimal", "_", "1", "3", ".", "2"]), "minimal 13.2");
        // Was `["_uh", ...] -> "uh 13.6"`. The leading "uh" was incidental to
        // what this case covers (decimal joining); N0 now removes it, and the
        // word before the number is what actually exercises the boundary guard.
        assert_eq!(run(&["_about", "_", "1", "3", ".", "6"]), "about 13.6");
        assert_eq!(run(&["_on", "_", "1", "1", ".", "2", "4"]), "on 11.24");
        assert_eq!(
            run(&["_Herd", "ride", "_", "0", ".", "1", ".", "1", "3", "4"]),
            "Herdride 0.1.134"
        );
    }

    /// Single-digit decimals were split too: only a lone digit with no digit or
    /// decimal point beside it survived the old rule.
    #[test]
    fn joins_single_digit_decimals() {
        assert_eq!(run(&["_to", "_", "3", ".", "6"]), "to 3.6");
        assert_eq!(run(&["_GLM", "_", "5", ".", "3"]), "GLM 5.3");
        assert_eq!(run(&["_", "0", ".", "1"]), "0.1");
    }

    /// A `▁` between numbers means they are separate values. This is the signal
    /// that only exists in the token stream, and the reason the join cannot be
    /// done downstream.
    #[test]
    fn word_boundary_still_separates_distinct_numbers() {
        assert_eq!(
            run(&["_Gwen", "_", "3", ".", "8", "_", "2", "7"]),
            "Gwen 3.8 27"
        );
        assert_eq!(
            run(&["_", "5", "5", "5", "_", "1", "2", "3", "4"]),
            "555 1234"
        );
        assert_eq!(run(&["_one", ",", "_", "2", ",", "_", "3"]), "one, 2, 3");
    }

    /// The rule exists because SentencePiece omits `▁` before a digit that
    /// follows a word. That repair must still happen.
    #[test]
    fn still_repairs_missing_boundary_after_a_word() {
        assert_eq!(run(&["_at", "6", "0"]), "at 60");
        assert_eq!(run(&["_version", "1", "3"]), "version 13");
    }

    /// Single uppercase letters are labels; the lowercase article "a" is a word.
    #[test]
    fn distinguishes_labels_from_the_article() {
        assert_eq!(run(&["_A", "4"]), "A4");
        assert_eq!(run(&["_K", "3", ","]), "K3,");
        assert_eq!(run(&["_a", "2", "4"]), "a 24");
    }

    #[test]
    fn drops_special_tokens_but_keeps_unk() {
        assert_eq!(run(&["_hello", "<pad>", "_", "1", "3"]), "hello 13");
        assert_eq!(run(&["_a", "<unk>", "_b"]), "a<unk> b");
    }

    /// PARITY GUARD. `AsrRuntime::transcribe_under_lease` short-circuits short
    /// audio to `CoreMlParakeet::transcribe` (-> `detok`), while
    /// `transcribe_final_result_under_lease` — the production dictation path —
    /// always routes through `coreml_parakeet_result` (-> this function). Two
    /// APIs, same audio, two detokenizers. That divergence is how a broken digit
    /// join shipped for months while the canonical eval, which calls the former,
    /// stayed green.
    ///
    /// Full parity does not hold yet: `detok` concatenates blindly, so it keeps
    /// `<pad>`-style special tokens and never applies the missing-boundary
    /// repair. Those are separate contract questions. What must never diverge
    /// again is numeric assembly, so that is pinned here.
    #[test]
    fn agrees_with_detok_on_numeric_token_streams() {
        let cases: &[&[&str]] = &[
            &["_minimal", "_", "1", "3", ".", "2"],
            &["_static", "_", "1", "5"],
            &["_hit", "_", "9", "0"],
            &["_Gwen", "_", "3", ".", "8", "_", "2", "7"],
            &["_to", "_", "3", ".", "6"],
            &["_", "0", ".", "1"],
            &["_", "5", "5", "5", "_", "1", "2", "3", "4"],
        ];
        for tokens in cases {
            let vocab: Vec<String> = tokens.iter().map(|t| t.replace('_', "\u{2581}")).collect();
            let ids: Vec<usize> = (0..vocab.len()).collect();
            assert_eq!(
                run(tokens),
                crate::coreml_asr::detok::detok(&ids, &vocab),
                "detokenizer divergence on {tokens:?}"
            );
        }
    }

    /// Timings must survive the extraction unchanged.
    #[test]
    fn preserves_piece_timings() {
        let vocab: Vec<String> = ["\u{2581}hi", "1", "3"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let hits = vec![(0usize, 0.0f32, 4usize), (1, 1.0, 2), (2, 1.5, 3)];
        let pieces = assemble_display_pieces(&hits, &vocab);
        assert_eq!(pieces.len(), 3);
        assert!((pieces[0].duration - 0.32).abs() < 1e-6);
        assert!((pieces[1].start - 1.0).abs() < 1e-6);
        assert!((pieces[2].duration - 0.24).abs() < 1e-6);
        let text: String = pieces.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(text.trim(), "hi 13");
    }
}

#[cfg(test)]
mod group_timed_words_tests {
    use super::group_timed_words;

    fn pieces() -> Vec<String> {
        vec![
            "\u{2581}hello".to_string(),
            "\u{2581}world".to_string(),
            "ing".to_string(),
        ]
    }

    /// The short-file helper used to set `end = next word's start`, stretching
    /// every cue across the pause that followed it. Durations must survive.
    #[test]
    fn preserves_the_pause_between_words() {
        // "hello" lasts 320 ms, then a 1.68 s pause, then "world" for 240 ms.
        // durations are FRAME counts (0.08 s each): 4 frames = 320 ms, 3 = 240 ms.
        let hits = vec![(0usize, 0.0f32, 4usize), (1usize, 2.0f32, 3usize)];
        let words = group_timed_words(&hits, &pieces());
        assert_eq!(words.len(), 2);
        assert_eq!(words[0].text, "hello");
        // Must end at its own end (0.32), NOT stretch to 2.0.
        assert!((words[0].end - 0.32).abs() < 1e-4, "got {}", words[0].end);
        // Last word keeps its real duration instead of an arbitrary +0.4 s tail.
        assert!((words[1].end - 2.24).abs() < 1e-4, "got {}", words[1].end);
    }

    #[test]
    fn clamps_zero_width_and_overhanging_cues() {
        // A frame-sharing piece reports 0 duration -> must still get >= 80 ms.
        // An overlong duration must never overhang the next word's start.
        let hits = vec![(0usize, 0.0f32, 0usize), (1usize, 0.5f32, 118usize)];
        let words = group_timed_words(&hits, &pieces());
        assert!((words[0].end - 0.08).abs() < 1e-4, "got {}", words[0].end);
        assert!(words[0].end <= words[1].start + 1e-6);
    }

    #[test]
    fn subword_continuation_extends_the_same_word() {
        // "world" + "ing" is ONE word ending at the continuation's end.
        let hits = vec![(1usize, 1.0f32, 2usize), (2usize, 1.2f32, 4usize)];
        let words = group_timed_words(&hits, &pieces());
        assert_eq!(words.len(), 1);
        assert_eq!(words[0].text, "worlding");
        assert!((words[0].end - 1.52).abs() < 1e-4, "got {}", words[0].end);
    }
}

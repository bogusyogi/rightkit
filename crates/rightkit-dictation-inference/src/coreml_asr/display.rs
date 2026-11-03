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

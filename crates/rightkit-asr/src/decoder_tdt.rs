use crate::decoder::{TimedPiece, TranscriptionResult};
use crate::error::Result;
use crate::parakeet_core::detok::{assemble_pieces, is_skippable_special, kept_after_filler_strip, DetokPolicy};
use crate::vocab::Vocabulary;

/// TDT encoder subsampling: encoder frames per mel hop (8x).
pub(crate) const ENCODER_STRIDE: usize = 8;

/// Seconds at encoder frame `frame`: `frame * ENCODER_STRIDE * hop / sample_rate`
/// (0.08 s for a 10 ms hop at 16 kHz). Integer product, then one division, so
/// every caller rounds identically.
fn frame_seconds(frame: usize, hop_length: usize, sample_rate: usize) -> f32 {
    (frame * ENCODER_STRIDE * hop_length) as f32 / sample_rate as f32
}

/// `(start, end)` seconds of a TDT token emitted at encoder `frame` with the
/// predicted `duration` in frames. The end is `frame + duration`, clamped to the
/// next token's start only when the duration would overlap it. The last token
/// has no next start, so it keeps its duration; a zero-length last token gets
/// the 10 ms minimum the legacy decoder gave every token.
pub(crate) fn token_span(
    frame: usize,
    duration: usize,
    next_frame: Option<usize>,
    hop_length: usize,
    sample_rate: usize,
) -> (f32, f32) {
    let start = frame_seconds(frame, hop_length, sample_rate);
    let mut end = frame_seconds(frame + duration, hop_length, sample_rate);
    match next_frame {
        Some(next) => {
            let next_start = frame_seconds(next, hop_length, sample_rate);
            if end > next_start {
                end = next_start;
            }
        }
        None => {
            if end <= start {
                end = start + 0.01;
            }
        }
    }
    (start, end.max(start))
}

/// Legacy span for models without a duration head (RNN-T): the end is the next
/// token's start, or 10 ms after the last token's start.
fn legacy_span(frame: usize, next_frame: Option<usize>, hop_length: usize, sample_rate: usize) -> (f32, f32) {
    let start = frame_seconds(frame, hop_length, sample_rate);
    let end = next_frame
        .map(|n| frame_seconds(n, hop_length, sample_rate))
        .unwrap_or(start + 0.01);
    (start, end)
}

/// Spans for the beam search's hits `(token, start_seconds, duration_frames)`.
/// The beam writes starts as `frame * 0.08` (`parakeet_core::tdt_beam`); the
/// frame is recovered by rounding, then the same [`token_span`] arithmetic as
/// [`ParakeetTDTDecoder::decode_with_timestamps`] applies, so a width-1 beam and
/// the greedy path yield identical timings for the same lattice. With
/// `frames_only_ends`, each token ends at the next token's frame (the last one
/// after one frame) and the predicted durations are ignored.
pub(crate) fn beam_hit_spans(
    hits: &[(usize, f32, usize)],
    hop_length: usize,
    sample_rate: usize,
    frames_only_ends: bool,
) -> Vec<(f32, f32)> {
    let per_frame = (ENCODER_STRIDE * hop_length) as f32;
    let frames: Vec<usize> = hits
        .iter()
        .map(|h| (h.1 * sample_rate as f32 / per_frame).round().max(0.0) as usize)
        .collect();
    hits.iter()
        .enumerate()
        .map(|(k, &(_, _, dur))| {
            let frame = frames[k];
            let next = frames.get(k + 1).copied();
            let duration = if frames_only_ends {
                next.map(|n| n.saturating_sub(frame)).unwrap_or(1)
            } else {
                dur
            };
            token_span(frame, duration, next, hop_length, sample_rate)
        })
        .collect()
}

/// How a lattice entry's end is derived. Mirrors the three timing paths:
/// predicted TDT durations (greedy and beam), frame-only ends (`frames_only_ends`),
/// and next-start ends for models without a duration head (RNN-T).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DurationMode {
    Predicted,
    FramesOnly,
    NoDurations,
}

/// Integer sample span `(start_sample, end_sample)` of a token at `frame` with
/// `duration` frames. Same rule as [`token_span`], in exact integers: the end is
/// `(frame + duration) * 8 * hop` samples, clamped to the next token's start only
/// when it would overlap. A zero-length last token gets 10 ms (`sample_rate / 100`).
pub(crate) fn sample_span(
    frame: usize,
    duration: usize,
    next_frame: Option<usize>,
    hop_length: usize,
    sample_rate: usize,
) -> (u64, u64) {
    let per_frame = (ENCODER_STRIDE * hop_length) as u64;
    let start = frame as u64 * per_frame;
    let mut end = (frame + duration) as u64 * per_frame;
    match next_frame {
        Some(next) => {
            let next_start = next as u64 * per_frame;
            if end > next_start {
                end = next_start;
            }
        }
        None => {
            if end <= start {
                end = start + (sample_rate as u64 / 100);
            }
        }
    }
    (start, end.max(start))
}

/// Frame index of each beam hit, recovered from its `frame * 0.08` start seconds
/// by rounding (exact for the beam search's own starts).
pub(crate) fn beam_hit_frames(hits: &[(usize, f32, usize)], hop_length: usize, sample_rate: usize) -> Vec<usize> {
    let per_frame = (ENCODER_STRIDE * hop_length) as f32;
    hits.iter()
        .map(|h| (h.1 * sample_rate as f32 / per_frame).round().max(0.0) as usize)
        .collect()
}

/// Sample-exact pieces from lattice entries `(token_id, frame, predicted_duration)`,
/// computed before any detokenisation merges them, then filtered and texted by the
/// SAME detokenizer pass as the `TimedToken` output: one piece per token, same order
/// (`token_index` is the position), `text` equal to the token's text. `text_of` returns
/// the raw vocabulary piece; ids it cannot resolve follow `missing`. Spans use every
/// entry (including ones later dropped) as the next emission, as the token output does.
pub(crate) fn lattice_pieces(
    entries: &[(usize, usize, usize)],
    mode: DurationMode,
    text_of: &dyn Fn(usize) -> Option<String>,
    missing: MissingText,
    policy: DetokPolicy,
    hop_length: usize,
    sample_rate: usize,
) -> Vec<TimedPiece> {
    let frames: Vec<usize> = entries.iter().map(|e| e.1).collect();
    // (entry index, raw vocabulary piece)
    let mut cands: Vec<(usize, String)> = Vec::with_capacity(entries.len());
    for (k, &(id, _, _)) in entries.iter().enumerate() {
        match text_of(id) {
            Some(raw) => cands.push((k, raw)),
            None if missing == MissingText::Empty => cands.push((k, String::new())),
            None => {}
        }
    }
    if policy.strip_fillers {
        let texts: Vec<&str> = cands.iter().map(|c| c.1.as_str()).collect();
        let keep = kept_after_filler_strip(&texts);
        cands = keep.into_iter().map(|i| cands[i].clone()).collect();
    }
    cands.retain(|c| !is_skippable_special(&c.1, policy));
    let display = assemble_pieces(cands.iter().map(|c| c.1.as_str()), policy).display_pieces;
    let per_frame = (ENCODER_STRIDE * hop_length) as u32;
    cands
        .into_iter()
        .zip(display)
        .enumerate()
        .map(|(n, ((k, raw), text))| {
            let (id, frame, dur) = entries[k];
            let next = frames.get(k + 1).copied();
            let duration = match mode {
                DurationMode::Predicted => dur,
                DurationMode::FramesOnly => next.map(|n| n.saturating_sub(frame)).unwrap_or(1),
                DurationMode::NoDurations => next.map(|n| n.saturating_sub(frame)).unwrap_or(0),
            };
            let (start_sample, end_sample) = sample_span(frame, duration, next, hop_length, sample_rate);
            TimedPiece {
                token_id: id,
                text,
                raw,
                token_index: n as u32,
                frame: frame as u32,
                start_sample,
                end_sample,
                // The model's own duration; 0 for a model without a duration head.
                dur_frames: if mode == DurationMode::NoDurations { 0 } else { dur as u32 },
                frame_samples: per_frame,
                sample_rate: sample_rate as u32,
            }
        })
        .collect()
}

/// What a vocabulary id with no text becomes in [`lattice_pieces`]. Must match the
/// token output of the same decode so the one-piece-per-token invariant holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MissingText {
    /// Dropped (the greedy decoder drops it from its tokens too).
    Skip,
    /// Kept as a piece with empty `text` and `raw` (the beam and CoreML paths treat
    /// an unknown id as the empty string and keep it).
    Empty,
}

/// Detokenizer policy of [`ParakeetTDTDecoder::decode_with_timestamps`]: guarded digit
/// boundaries, special tokens skipped, no filler stripping.
pub(crate) const GREEDY_POLICY: DetokPolicy = DetokPolicy {
    guarded_digit_boundaries: true,
    skip_special_tokens: true,
    strip_fillers: false,
};

/// TDT greedy decoder for Parakeet TDT models
#[derive(Debug)]
pub struct ParakeetTDTDecoder {
    vocab: Vocabulary,
}

impl ParakeetTDTDecoder {
    /// Load decoder from vocab file
    pub fn from_vocab(vocab: Vocabulary) -> Self {
        Self { vocab }
    }

    /// Sample-exact pieces for the greedy output, computed from the frame indices
    /// before detokenisation. `durations` follows [`decode_with_timestamps`]: empty
    /// (RNN-T) gives next-start ends. The `TimedToken` output is unchanged.
    pub(crate) fn timed_pieces(
        &self,
        tokens: &[usize],
        frame_indices: &[usize],
        durations: &[usize],
        hop_length: usize,
        sample_rate: usize,
    ) -> Vec<TimedPiece> {
        let have_durations = durations.len() == tokens.len();
        let entries: Vec<(usize, usize, usize)> = tokens
            .iter()
            .enumerate()
            .map(|(i, &id)| (id, frame_indices[i], if have_durations { durations[i] } else { 0 }))
            .collect();
        let mode = if have_durations { DurationMode::Predicted } else { DurationMode::NoDurations };
        lattice_pieces(
            &entries,
            mode,
            &|id| self.vocab.id_to_text(id).map(str::to_string),
            MissingText::Skip,
            GREEDY_POLICY,
            hop_length,
            sample_rate,
        )
    }

    /// Decode tokens with timestamps. For TDT models, greedy decoding is done in
    /// the model; here we convert ids to text and assign spans.
    ///
    /// `durations` holds the predicted TDT duration (frames) per token. A token
    /// ends at `(frame + duration)` frames, clamped to the next token's start only
    /// when it would overlap (see [`token_span`]). Pass an EMPTY `durations` slice
    /// for models without a duration head (RNN-T): each token then ends at the next
    /// token's start (the 0.1.0 behaviour). A slice whose length differs from
    /// `tokens` is treated as absent.
    pub fn decode_with_timestamps(
        &self,
        tokens: &[usize],
        frame_indices: &[usize],
        durations: &[usize],
        hop_length: usize,
        sample_rate: usize,
    ) -> Result<TranscriptionResult> {
        let mut result_tokens = Vec::new();
        let mut full_text = String::new();
        let have_durations = durations.len() == tokens.len();

        for (i, &token_id) in tokens.iter().enumerate() {
            if let Some(token_text) = self.vocab.id_to_text(token_id) {
                let frame = frame_indices[i];
                let next_frame = frame_indices.get(i + 1).copied();
                let (start, end) = if have_durations {
                    token_span(frame, durations[i], next_frame, hop_length, sample_rate)
                } else {
                    legacy_span(frame, next_frame, hop_length, sample_rate)
                };

                // Handle SentencePiece format (▁ prefix for word start)
                let mut display_text = token_text.replace('▁', " ");

                // Insert space before pure digit tokens following words.
                // SentencePiece digits lack the ▁ prefix, causing "at60" instead of "at 60".
                // Skip single uppercase letters (A4, B12) but allow lowercase 'a' (article).
                if !full_text.is_empty()
                    && !display_text.starts_with(' ')
                    && display_text.chars().all(|c| c.is_ascii_digit())
                {
                    let trailing_letters: usize = full_text
                        .chars()
                        .rev()
                        .take_while(|c| c.is_alphabetic())
                        .count();
                    let last_char = full_text.chars().last();
                    let is_article_a = trailing_letters == 1 && last_char == Some('a');
                    if trailing_letters > 1 || is_article_a {
                        display_text.insert(0, ' ');
                    }
                }

                // Skip special tokens
                if !(token_text.starts_with('<')
                    && token_text.ends_with('>')
                    && token_text != "<unk>")
                {
                    full_text.push_str(&display_text);

                    result_tokens.push(crate::decoder::TimedToken {
                        text: display_text,
                        start,
                        end,
                    });
                }
            }
        }

        Ok(TranscriptionResult {
            text: full_text.trim().to_string(),
            tokens: result_tokens,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_vocab(tokens: &[&str]) -> Vocabulary {
        Vocabulary {
            id_to_token: tokens.iter().map(|s| s.to_string()).collect(),
            _blank_id: 0,
        }
    }

    #[test]
    fn test_digit_spacing_after_word() {
        // Simulates "like 100" tokenized as ["▁like", "1", "0", "0"]
        let vocab = make_vocab(&["▁like", "1", "0", "0"]);
        let decoder = ParakeetTDTDecoder::from_vocab(vocab);
        let result = decoder
            .decode_with_timestamps(&[0, 1, 2, 3], &[0, 1, 2, 3], &[1, 1, 1, 1], 160, 16000)
            .unwrap();
        assert_eq!(result.text, "like 100");
    }

    #[test]
    fn test_digit_spacing_after_article_a() {
        // Simulates "a 24" tokenized as ["▁a", "2", "4"]
        let vocab = make_vocab(&["▁a", "2", "4"]);
        let decoder = ParakeetTDTDecoder::from_vocab(vocab);
        let result = decoder
            .decode_with_timestamps(&[0, 1, 2], &[0, 1, 2], &[1, 1, 1], 160, 16000)
            .unwrap();
        assert_eq!(result.text, "a 24");
    }

    #[test]
    fn test_no_spacing_after_single_uppercase() {
        // Simulates "A4" tokenized as ["▁A", "4"] - should stay together
        let vocab = make_vocab(&["▁A", "4"]);
        let decoder = ParakeetTDTDecoder::from_vocab(vocab);
        let result = decoder
            .decode_with_timestamps(&[0, 1], &[0, 1], &[1, 1], 160, 16000)
            .unwrap();
        assert_eq!(result.text, "A4");
    }

    #[test]
    fn test_no_spacing_after_symbol() {
        // Simulates "$100" tokenized as ["$", "1", "0", "0"]
        let vocab = make_vocab(&["$", "1", "0", "0"]);
        let decoder = ParakeetTDTDecoder::from_vocab(vocab);
        let result = decoder
            .decode_with_timestamps(&[0, 1, 2, 3], &[0, 1, 2, 3], &[1, 1, 1, 1], 160, 16000)
            .unwrap();
        assert_eq!(result.text, "$100");
    }

    #[test]
    fn test_spacing_in_phrase() {
        // Simulates "In 2021" tokenized as ["▁In", "2", "0", "2", "1"]
        let vocab = make_vocab(&["▁In", "2", "0", "2", "1"]);
        let decoder = ParakeetTDTDecoder::from_vocab(vocab);
        let result = decoder
            .decode_with_timestamps(
                &[0, 1, 2, 3, 4],
                &[0, 1, 2, 3, 4],
                &[1, 1, 1, 1, 1],
                160,
                16000,
            )
            .unwrap();
        assert_eq!(result.text, "In 2021");
    }

    #[test]
    fn test_tokens_have_correct_spacing() {
        // Verify individual token texts have spacing applied
        let vocab = make_vocab(&["▁like", "1", "0", "0"]);
        let decoder = ParakeetTDTDecoder::from_vocab(vocab);
        let result = decoder
            .decode_with_timestamps(&[0, 1, 2, 3], &[0, 1, 2, 3], &[1, 1, 1, 1], 160, 16000)
            .unwrap();

        // Check token texts - first digit should have space prepended
        assert_eq!(result.tokens[0].text, " like");
        assert_eq!(result.tokens[1].text, " 1"); // Space added by heuristic
        assert_eq!(result.tokens[2].text, "0");
        assert_eq!(result.tokens[3].text, "0");
    }

    #[test]
    fn test_full_flow_with_timestamp_processing() {
        use crate::timestamps::{process_timestamps, TimestampMode};

        let vocab = make_vocab(&["▁like", "1", "0", "0", "▁bucks"]);
        let decoder = ParakeetTDTDecoder::from_vocab(vocab);
        let result = decoder
            .decode_with_timestamps(
                &[0, 1, 2, 3, 4],
                &[0, 1, 2, 3, 4],
                &[1, 1, 1, 1, 1],
                160,
                16000,
            )
            .unwrap();

        // Test Words mode (what Undertone likely uses)
        let words = process_timestamps(&result.tokens, TimestampMode::Words);
        let text: String = words
            .iter()
            .map(|t| t.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(text, "like 100 bucks");

        // Test Tokens mode
        let tokens_text: String = result.tokens.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(tokens_text.trim(), "like 100 bucks");
    }

    const HOP: usize = 160;
    const SR: usize = 16_000;

    fn close(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-6, "{a} != {b}");
    }

    /// Synthetic TDT lattice: tokens at frames 0, 2, 5 with predicted durations 2, 3, 1.
    #[test]
    fn synthetic_lattice_ends_use_predicted_durations() {
        let d = ParakeetTDTDecoder::from_vocab(make_vocab(&["▁a", "▁b", "▁c"]));
        let r = d.decode_with_timestamps(&[0, 1, 2], &[0, 2, 5], &[2, 3, 1], HOP, SR).unwrap();
        assert_eq!(r.tokens.len(), 3);
        // a: 0.00 -> (0+2) frames = 0.16, next start 0.16 -> 0.16
        close(r.tokens[0].start, 0.0);
        close(r.tokens[0].end, 0.16);
        // b: 0.16 -> (2+3) frames = 0.40, next start 0.40 -> 0.40
        close(r.tokens[1].start, 0.16);
        close(r.tokens[1].end, 0.40);
        // c: 0.40 -> (5+1) frames = 0.48; last token keeps its duration
        close(r.tokens[2].start, 0.40);
        close(r.tokens[2].end, 0.48);
    }

    #[test]
    fn overlapping_duration_is_clamped_to_next_start() {
        // a predicts 3 frames but b starts at frame 1 (0.08 s): a ends at b's start.
        let d = ParakeetTDTDecoder::from_vocab(make_vocab(&["▁a", "▁b"]));
        let r = d.decode_with_timestamps(&[0, 1], &[0, 1], &[3, 1], HOP, SR).unwrap();
        close(r.tokens[0].end, 0.08);
        close(r.tokens[1].start, 0.08);
        close(r.tokens[1].end, 0.16);
    }

    #[test]
    fn rnn_t_without_durations_ends_at_next_start() {
        let d = ParakeetTDTDecoder::from_vocab(make_vocab(&["▁a", "▁b", "▁c"]));
        let r = d.decode_with_timestamps(&[0, 1, 2], &[0, 2, 5], &[], HOP, SR).unwrap();
        close(r.tokens[0].end, 0.16);
        close(r.tokens[1].end, 0.40);
        close(r.tokens[2].end, 0.41);
    }

    #[test]
    fn zero_length_last_token_gets_the_minimum_width() {
        let d = ParakeetTDTDecoder::from_vocab(make_vocab(&["▁a"]));
        let r = d.decode_with_timestamps(&[0], &[0], &[0], HOP, SR).unwrap();
        close(r.tokens[0].start, 0.0);
        close(r.tokens[0].end, 0.01);
    }

    /// The beam path (width 1) reports the same lattice as hits with starts at
    /// `frame * 0.08`; its spans must equal the greedy spans bit for bit.
    #[test]
    fn beam_width_one_matches_greedy_timings_exactly() {
        let d = ParakeetTDTDecoder::from_vocab(make_vocab(&["▁a", "▁b", "▁c"]));
        let greedy = d
            .decode_with_timestamps(&[0, 1, 2], &[0, 2, 5], &[2, 3, 1], HOP, SR)
            .unwrap();
        let hits = vec![(0usize, 0.0f32, 2usize), (1, 2.0 * 0.08, 3), (2, 5.0 * 0.08, 1)];
        let beam = beam_hit_spans(&hits, HOP, SR, false);
        let greedy_spans: Vec<(f32, f32)> = greedy.tokens.iter().map(|t| (t.start, t.end)).collect();
        assert_eq!(beam, greedy_spans);
    }

    #[test]
    fn beam_frames_only_ends_ignores_predicted_durations() {
        let hits = vec![(0usize, 0.0f32, 2usize), (1, 2.0 * 0.08, 3), (2, 5.0 * 0.08, 1)];
        let spans = beam_hit_spans(&hits, HOP, SR, true);
        close(spans[0].1, 0.16); // next token's frame
        close(spans[1].1, 0.40);
        close(spans[2].1, 0.48); // last token: one frame after its start
    }

    /// The same synthetic lattice as above: frames 0, 2, 5 with durations 2, 3, 1.
    /// One frame is 1280 samples at hop 160.
    #[test]
    fn pieces_carry_exact_sample_spans_and_match_the_token_seconds() {
        let d = ParakeetTDTDecoder::from_vocab(make_vocab(&["▁a", "▁b", "▁c"]));
        let pieces = d.timed_pieces(&[0, 1, 2], &[0, 2, 5], &[2, 3, 1], HOP, SR);
        assert_eq!(pieces.len(), 3);
        let spans: Vec<(u64, u64)> = pieces.iter().map(|p| (p.start_sample, p.end_sample)).collect();
        assert_eq!(spans, vec![(0, 2560), (2560, 6400), (6400, 7680)]);
        assert_eq!(pieces.iter().map(|p| p.frame).collect::<Vec<_>>(), vec![0, 2, 5]);
        assert!(pieces.iter().all(|p| p.sample_rate == 16_000));
        assert_eq!(pieces[0].text, " a", "SentencePiece marker shown as a space");
        assert_eq!(pieces[2].token_id, 2);

        // Seconds from the samples agree with the TimedToken output of the same decode.
        let tokens = d.decode_with_timestamps(&[0, 1, 2], &[0, 2, 5], &[2, 3, 1], HOP, SR).unwrap();
        for (p, t) in pieces.iter().zip(&tokens.tokens) {
            close(p.start_sample as f32 / SR as f32, t.start);
            close(p.end_sample as f32 / SR as f32, t.end);
        }
    }

    #[test]
    fn pieces_skip_special_tokens_like_the_token_output() {
        let d = ParakeetTDTDecoder::from_vocab(make_vocab(&["<pad>", "▁x"]));
        let pieces = d.timed_pieces(&[0, 1], &[0, 1], &[1, 1], HOP, SR);
        assert_eq!(pieces.len(), 1);
        assert_eq!(pieces[0].token_id, 1);
        assert_eq!(pieces[0].text, " x");
        assert_eq!((pieces[0].start_sample, pieces[0].end_sample), (1280, 2560));
    }

    #[test]
    fn rnn_t_pieces_end_at_the_next_start_and_the_last_gets_ten_ms() {
        let d = ParakeetTDTDecoder::from_vocab(make_vocab(&["▁a", "▁b", "▁c"]));
        let pieces = d.timed_pieces(&[0, 1, 2], &[0, 2, 5], &[], HOP, SR);
        let spans: Vec<(u64, u64)> = pieces.iter().map(|p| (p.start_sample, p.end_sample)).collect();
        assert_eq!(spans, vec![(0, 2560), (2560, 6400), (6400, 6560)]);
    }

    /// The beam path (width 1) produces identical sample spans for the same lattice.
    #[test]
    fn beam_pieces_match_greedy_pieces_bit_for_bit() {
        let d = ParakeetTDTDecoder::from_vocab(make_vocab(&["▁a", "▁b", "▁c"]));
        let greedy = d.timed_pieces(&[0, 1, 2], &[0, 2, 5], &[2, 3, 1], HOP, SR);
        let hits = vec![(0usize, 0.0f32, 2usize), (1, 2.0 * 0.08, 3), (2, 5.0 * 0.08, 1)];
        let frames = beam_hit_frames(&hits, HOP, SR);
        let entries: Vec<(usize, usize, usize)> =
            hits.iter().zip(frames).map(|(&(id, _, dur), f)| (id, f, dur)).collect();
        let beam = lattice_pieces(
            &entries,
            DurationMode::Predicted,
            &|id| d.vocab.id_to_text(id).map(str::to_string),
            MissingText::Skip,
            GREEDY_POLICY,
            HOP,
            SR,
        );
        assert_eq!(beam, greedy);
    }

    #[test]
    fn sample_span_clamps_overlap_and_keeps_the_last_duration() {
        // a predicts 3 frames, b starts at frame 1: a ends at b's start (1280).
        assert_eq!(sample_span(0, 3, Some(1), HOP, SR), (0, 1280));
        // last token with duration 2: (frame + 2) * 1280.
        assert_eq!(sample_span(1, 2, None, HOP, SR), (1280, 3840));
        // zero-length last token: 10 ms = 160 samples.
        assert_eq!(sample_span(4, 0, None, HOP, SR), (5120, 5280));
    }

    // ---- TimedPiece raw end, duration and text parity ----

    fn pieces_of(d: &ParakeetTDTDecoder, ids: &[usize], frames: &[usize], durs: &[usize]) -> Vec<TimedPiece> {
        d.timed_pieces(ids, frames, durs, HOP, SR)
    }

    #[test]
    fn raw_end_matches_clamped_end_when_duration_fits_before_the_next_piece() {
        let d = ParakeetTDTDecoder::from_vocab(make_vocab(&["▁a", "▁b"]));
        let p = pieces_of(&d, &[0, 1], &[0, 4], &[2, 1]);
        assert_eq!((p[0].start_sample, p[0].end_sample, p[0].dur_frames), (0, 2560, 2));
        assert_eq!(p[0].raw_end_sample(), 2560);
        assert_eq!(p[0].frame_samples, 1280);
    }

    #[test]
    fn zero_duration_followed_by_another_emission_ends_at_its_start_but_raw_end_is_one_frame() {
        let d = ParakeetTDTDecoder::from_vocab(make_vocab(&["▁a", "▁b"]));
        let p = pieces_of(&d, &[0, 1], &[0, 2], &[0, 1]);
        assert_eq!((p[0].start_sample, p[0].end_sample), (0, 0), "clamped end keeps its meaning");
        assert_eq!(p[0].dur_frames, 0);
        assert_eq!(p[0].raw_end_sample(), 1280, "start + max(0,1) * 1280");
    }

    #[test]
    fn zero_duration_last_piece_gets_160_clamped_and_one_frame_raw() {
        let d = ParakeetTDTDecoder::from_vocab(make_vocab(&["▁a"]));
        let p = pieces_of(&d, &[0], &[4], &[0]);
        assert_eq!((p[0].start_sample, p[0].end_sample), (5120, 5280));
        assert_eq!(p[0].raw_end_sample(), 5120 + 1280);
    }

    #[test]
    fn overlap_clamps_the_end_but_not_the_raw_end() {
        let d = ParakeetTDTDecoder::from_vocab(make_vocab(&["▁a", "▁b"]));
        let p = pieces_of(&d, &[0, 1], &[0, 1], &[3, 1]);
        assert_eq!(p[0].end_sample, 1280, "clamped to the next start");
        assert_eq!(p[0].raw_end_sample(), 3 * 1280, "unclamped, past the next start");
        assert!(p[0].raw_end_sample() > p[1].start_sample);
    }

    #[test]
    fn frame_samples_follows_the_hop_not_a_constant() {
        let d = ParakeetTDTDecoder::from_vocab(make_vocab(&["▁a"]));
        let p = d.timed_pieces(&[0], &[1], &[2], 256, 16_000);
        assert_eq!(p[0].frame_samples, 2048);
        assert_eq!(p[0].raw_end_sample(), 2048 + 2 * 2048);
    }

    #[test]
    fn digit_boundary_guard_gives_the_piece_the_same_text_as_its_token() {
        // "like" + "1": the raw piece alone would weld to "like1".
        let d = ParakeetTDTDecoder::from_vocab(make_vocab(&["▁like", "1", "0"]));
        let r = d.decode_with_timestamps(&[0, 1, 2], &[0, 1, 2], &[1, 1, 1], HOP, SR).unwrap();
        let p = pieces_of(&d, &[0, 1, 2], &[0, 1, 2], &[1, 1, 1]);
        assert_eq!(p[1].text, " 1");
        assert_eq!(p[1].raw, "1", "raw vocabulary piece is untouched");
        assert_eq!(p[0].raw, "▁like");
        for (piece, tok) in p.iter().zip(&r.tokens) {
            assert_eq!(piece.text, tok.text);
        }
    }

    #[test]
    fn text_less_id_is_dropped_from_both_outputs_by_the_greedy_decoder() {
        let d = ParakeetTDTDecoder::from_vocab(make_vocab(&["▁a", "▁b"]));
        let ids = [0usize, 99, 1]; // 99 has no text
        let r = d.decode_with_timestamps(&ids, &[0, 1, 2], &[1, 1, 1], HOP, SR).unwrap();
        let p = pieces_of(&d, &ids, &[0, 1, 2], &[1, 1, 1]);
        assert_eq!(p.len(), r.tokens.len());
        assert_eq!(p.len(), 2);
        assert_eq!(p.iter().map(|x| x.token_id).collect::<Vec<_>>(), vec![0, 1]);
    }

    /// Mirrors the beam and CoreML paths: an unknown id is the empty string and is
    /// KEPT by the detokenizer, so the piece must be kept too (empty text).
    #[test]
    fn text_less_id_becomes_an_empty_piece_where_the_detokenizer_keeps_it() {
        use crate::parakeet_core::detok::assemble_timed_pieces;
        use crate::parakeet_core::types::DecodedPiece;
        let table = ["▁a", "▁b"];
        let ids = [0usize, 99, 1];
        let policy = DetokPolicy::canonical_rust();
        let entries: Vec<(usize, usize, usize)> = ids.iter().enumerate().map(|(i, &id)| (id, i, 1)).collect();
        let pieces = lattice_pieces(
            &entries,
            DurationMode::Predicted,
            &|id| Some(table.get(id).map(|t| t.to_string()).unwrap_or_default()),
            MissingText::Empty,
            policy,
            HOP,
            SR,
        );
        let decoded: Vec<DecodedPiece> = ids
            .iter()
            .enumerate()
            .map(|(i, &id)| DecodedPiece {
                token_id: id,
                piece: table.get(id).map(|t| t.to_string()).unwrap_or_default(),
                start: i as f32,
                end: i as f32 + 1.0,
            })
            .collect();
        let result = assemble_timed_pieces(&decoded, policy);
        assert_eq!(result.tokens.len(), 3, "the detokenizer keeps the text-less id");
        assert_eq!(pieces.len(), 3);
        assert_eq!(pieces[1].text, "");
        assert_eq!(pieces[1].raw, "");
        assert_eq!(pieces[1].token_id, 99);
    }

    /// Specials, fillers and digits in one stream: count, order, index and text all
    /// agree with the detokenized output under the canonical policy.
    #[test]
    fn piece_count_order_index_and_text_equal_the_token_output() {
        use crate::parakeet_core::detok::assemble_timed_pieces;
        use crate::parakeet_core::types::DecodedPiece;
        let table = ["▁so", "<pad>", "▁uh", ",", "▁at", "6", "0", "▁a", "2", "▁A", "4"];
        let ids: Vec<usize> = (0..table.len()).collect();
        let policy = DetokPolicy::canonical_rust();
        let entries: Vec<(usize, usize, usize)> = ids.iter().map(|&i| (i, i * 2, 1)).collect();
        let pieces = lattice_pieces(
            &entries,
            DurationMode::Predicted,
            &|id| Some(table[id].to_string()),
            MissingText::Empty,
            policy,
            HOP,
            SR,
        );
        let decoded: Vec<DecodedPiece> = ids
            .iter()
            .map(|&i| DecodedPiece { token_id: i, piece: table[i].to_string(), start: 0.0, end: 0.0 })
            .collect();
        let result = assemble_timed_pieces(&decoded, policy);
        assert_eq!(pieces.len(), result.tokens.len());
        assert!(pieces.len() < table.len(), "special and filler were dropped");
        for (i, (p, t)) in pieces.iter().zip(&result.tokens).enumerate() {
            assert_eq!(p.token_index as usize, i);
            assert_eq!(p.text, t.text, "piece {i}");
        }
        assert!(pieces.iter().any(|p| p.text == " 6"), "digit guard applied");
        assert!(pieces.iter().any(|p| p.text == "4"), "single uppercase letter keeps A4");
    }
}

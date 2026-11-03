use crate::decoder::{TimedPiece, TranscriptionResult};
use crate::error::{Error, Result};
use crate::parakeet_core::detok::{assemble_pieces, is_skippable_special, kept_after_filler_strip, DetokPolicy};
use crate::vocab::Vocabulary;

/// TDT encoder subsampling: encoder frames per mel hop (8x).
pub(crate) const ENCODER_STRIDE: usize = 8;

/// Width, in milliseconds, given to a last token whose predicted duration is zero.
///
/// A zero-length span (start == end) cannot be shown as a subtitle cue or
/// highlighted, so the decoder widens such a last token to this width. The value
/// is the 10 ms minimum the legacy decoder gave every token. Only the last token
/// is widened: an earlier token ends at the next token's start, and is zero-length
/// only when the next token starts on the same frame.
pub const LAST_TOKEN_MIN_DURATION_MS: u64 = 10;

/// Seconds at encoder frame `frame`: `frame * ENCODER_STRIDE * hop / sample_rate`
/// (0.08 s for a 10 ms hop at 16 kHz). Integer product, then one division, so
/// every caller rounds identically.
fn frame_seconds(frame: usize, hop_length: usize, sample_rate: usize) -> f32 {
    (frame * ENCODER_STRIDE * hop_length) as f32 / sample_rate as f32
}

/// `(start, end)` seconds of a TDT token emitted at encoder `frame` with the
/// predicted `duration` in frames. The end is `frame + duration`, clamped to the
/// next token's start only when the duration would overlap it. The last token
/// has no next start, so it keeps its duration; a zero-length last token is
/// widened to [`LAST_TOKEN_MIN_DURATION_MS`].
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
                end = start + LAST_TOKEN_MIN_DURATION_MS as f32 / 1000.0;
            }
        }
    }
    (start, end.max(start))
}

/// Legacy span for models without a duration head (RNN-T): the end is the next
/// token's start, or [`LAST_TOKEN_MIN_DURATION_MS`] after the last token's start.
fn legacy_span(frame: usize, next_frame: Option<usize>, hop_length: usize, sample_rate: usize) -> (f32, f32) {
    let start = frame_seconds(frame, hop_length, sample_rate);
    let end = next_frame
        .map(|n| frame_seconds(n, hop_length, sample_rate))
        .unwrap_or(start + LAST_TOKEN_MIN_DURATION_MS as f32 / 1000.0);
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
/// when it would overlap. A zero-length last token is widened to
/// `LAST_TOKEN_MIN_DURATION_MS` (`sample_rate * LAST_TOKEN_MIN_DURATION_MS / 1000`
/// samples, which is `sample_rate / 100` at that value).
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
                end = start + sample_rate as u64 * LAST_TOKEN_MIN_DURATION_MS / 1000;
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

/// Shape and rate checks for [`ParakeetTDTDecoder::decode_with_timestamps`], run
/// before any indexing. The decode reads `frame_indices[i]` for every token `i`
/// and the next token's frame as `frame_indices[i + 1]`, so the lattice must satisfy:
///
/// - `frames == tokens`: exactly one encoder frame per token;
/// - `durations` is empty (a model without a duration head, RNN-T: each token ends
///   at the next token's start) or `durations == tokens` (one predicted duration per
///   token). Any other length is an error, never treated as absent;
/// - `hop_length > 0` and `sample_rate > 0`, so the frame step (`ENCODER_STRIDE *
///   hop_length` samples) and the seconds per sample are non-zero.
///
/// The lattice is model output, so a shape fault is `Error::Model`. A zero hop or
/// sample rate comes from the preprocessor config, so it is `Error::Config`.
fn validate_lattice(
    tokens: usize,
    frames: usize,
    durations: usize,
    hop_length: usize,
    sample_rate: usize,
) -> Result<()> {
    if frames != tokens {
        return Err(Error::Model(format!(
            "TDT lattice has {frames} frame indices for {tokens} tokens; expected one per token"
        )));
    }
    if durations != 0 && durations != tokens {
        return Err(Error::Model(format!(
            "TDT lattice has {durations} durations for {tokens} tokens; expected none or one per token"
        )));
    }
    if hop_length == 0 {
        return Err(Error::Config("TDT hop_length must be non-zero".into()));
    }
    if sample_rate == 0 {
        return Err(Error::Config("TDT sample_rate must be non-zero".into()));
    }
    Ok(())
}

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
    /// Indexes `frame_indices[i]` for every token, so the lattice must already pass
    /// the checks in `validate_lattice`; `decode_with_timestamps` runs them first.
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
    /// `frame_indices` holds one encoder frame per token. `durations` holds the
    /// predicted TDT duration (frames) per token. A token ends at `(frame + duration)`
    /// frames, clamped to the next token's start only when it would overlap (see
    /// [`token_span`]). Pass an EMPTY `durations` slice for models without a duration
    /// head (RNN-T): each token then ends at the next token's start (the 0.1.0
    /// behaviour).
    ///
    /// Returns `Error::Model` unless `frame_indices.len() == tokens.len()` and
    /// `durations` is empty or has `tokens.len()` entries. Returns `Error::Config`
    /// when `hop_length` or `sample_rate` is zero. Nothing is indexed before these
    /// checks pass.
    pub fn decode_with_timestamps(
        &self,
        tokens: &[usize],
        frame_indices: &[usize],
        durations: &[usize],
        hop_length: usize,
        sample_rate: usize,
    ) -> Result<TranscriptionResult> {
        validate_lattice(tokens.len(), frame_indices.len(), durations.len(), hop_length, sample_rate)?;
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

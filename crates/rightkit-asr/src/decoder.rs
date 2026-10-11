// Token with its timestamp information
// start and end are in seconds
#[derive(Debug, Clone, PartialEq)]
pub struct TimedToken {
    pub text: String,
    pub start: f32,
    pub end: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptionResult {
    pub text: String,
    pub tokens: Vec<TimedToken>,
}

/// One emitted model symbol with exact sample positions. Sibling of
/// [`TimedToken`], which is unchanged and still returned by every existing
/// method. Built from the decoder's frame indices BEFORE detokenisation, so it
/// is not merged or re-spaced: CutRight-style word-edge cuts can use
/// `start_sample` / `end_sample` directly.
///
/// # Invariant: one piece per token
///
/// For every single-window decode (`transcribe_result_with_pieces`,
/// `transcribe_window_beam_with_pieces`, `CoreMlParakeet::transcribe_window_with_pieces`)
/// `pieces.len() == result.tokens.len()`, in the same order, and
/// `pieces[i].text == result.tokens[i].text` (same detokenizer policy, including the
/// guarded digit boundary, special-token skip and filler stripping).
/// `pieces[i].token_index == i`. A vocabulary id with no text yields a piece whose
/// `text` and `raw` are empty on the paths whose token output keeps such an id
/// (beam and CoreML); the greedy ORT decoder drops it from both outputs.
/// For long-form decodes `token_index` is offset by the pieces of earlier windows,
/// so it indexes the concatenation of every window's tokens before seam handling
/// (equal to the result token index under `SeamPolicy::Exact`).
///
/// - `frame`: encoder frame index relative to the decoded window (for a whole
///   recording decode, the recording's frame index). One frame is
///   `frame_samples` = `8 * hop_length` samples (80 ms at a 10 ms hop, 16 kHz).
/// - `start_sample` / `end_sample`: absolute samples in the recording at
///   `sample_rate`. Integer arithmetic only; no float round-trip. `end_sample`
///   is CLAMPED: to the next emission's start when the duration would overlap
///   it, and a zero-length last piece gets 10 ms (160 samples at 16 kHz).
/// - `dur_frames`: the model's own duration for this emission, in encoder
///   frames, unclamped. `0` when the model has no duration head (RNN-T).
/// - `frame_samples`: samples per encoder frame for the model that produced
///   the piece.
/// - `raw_end_sample()`: unclamped end, `start + max(dur_frames, 1) * frame_samples`,
///   the end CutRight's pre-port assembly used. It can exceed the next piece's
///   `start_sample`.
/// - `text`: detokenized text, identical to the matching [`TimedToken::text`]
///   (SentencePiece `▁` becomes a space; a bare digit after a word gets a leading
///   space). Special tokens (`<...>`, except `<unk>`) are not emitted, as in `TimedToken`.
/// - `raw`: the vocabulary piece as stored (`▁` kept), before any detokenizer edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimedPiece {
    pub token_id: usize,
    pub text: String,
    pub raw: String,
    pub token_index: u32,
    pub frame: u32,
    pub start_sample: u64,
    pub end_sample: u64,
    pub dur_frames: u32,
    pub frame_samples: u32,
    pub sample_rate: u32,
}

impl TimedPiece {
    /// Unclamped end sample: `start_sample + max(dur_frames, 1) * frame_samples`.
    pub fn raw_end_sample(&self) -> u64 {
        self.start_sample + u64::from(self.dur_frames.max(1)) * u64::from(self.frame_samples)
    }
}

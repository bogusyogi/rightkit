//! TDT/RNN-T beam search: hypothesis representation, candidate expansion and the
//! emission-reward score policy.
//!
//! FIRST-PARTY, PLATFORM-NEUTRAL, AND DELIBERATELY MAC-SHAPED.
//!
//! This is the search machinery the macOS CoreML loop drives. It lives here
//! rather than inside `heardright-inference::coreml_asr` because that module is
//! `#[cfg(target_os = "macos")]`, which trapped HeardRight's own decode policy
//! behind a platform gate where it could be neither reused nor inspected from
//! anywhere else.
//!
//! `BeamHyp` carries `sym` (the max-symbols cap) and `hits` (token id, start
//! second, duration frames) because that is what the CoreML loop needs. It is
//! NOT a universal cross-platform hypothesis and does not try to be. The ONNX
//! path keeps `h`/`c` as ndarray tensors, has no `sym`, and represents timing
//! differently; forcing one struct over both would invent the kind of false
//! equivalence this migration exists to remove. Share what is genuinely common,
//! diverge explicitly where the platforms differ, and extract later only what
//! measurement proves identical.
//!
//! The score policy here is macOS's: emission reward 0.15, beam width 2,
//! duration by table lookup, no skip cap. Windows deliberately differs (emit
//! reward 0.10, index cap 2). Those are policy VALUES; the meaning of each
//! setting is what is shared.

/// Index of the maximum value; ties keep the FIRST (lowest) index, matching the
/// strict `>` left-to-right scan the pre-beam decoder used.
fn argmax(v: &[f32]) -> usize {
    let mut best = 0usize;
    let mut best_v = f32::NEG_INFINITY;
    for (i, &x) in v.iter().enumerate() {
        if x > best_v {
            best_v = x;
            best = i;
        }
    }
    best
}

/// One active beam hypothesis during decode. Carries everything needed to
/// resume decoding independently of every other hypothesis: its own LSTM
/// predictor state, its own TDT time index (hypotheses can and do sit at
/// different frames — see `tdt_step`), and its own emitted-token history
/// (used both for final output and for evaluating context-bias matches).
#[derive(Clone, Debug)]
pub struct BeamHyp {
    pub h: Vec<f32>,
    pub c: Vec<f32>,
    pub last_tok: i32,
    /// Encoder frame index this hypothesis will read next.
    pub t: usize,
    /// Symbols already emitted at frame `t` (TDT/RNN-T `max_symbols` cap).
    pub sym: usize,
    /// Cumulative log-probability (+ context-bias bonuses) of this
    /// hypothesis's token choices so far. Log-softmax based so scores stay
    /// comparable across hypotheses that may sit at different `t`/`sym`
    /// depths. At beam width 1 there is only ever one hypothesis, so this
    /// value never influences which token is chosen — see `decode_window`'s
    /// doc comment for why that keeps width 1 bit-identical to the original
    /// greedy decode.
    pub score: f32,
    pub hits: Vec<(usize, f32, usize)>,
}

// Per-emission score reward, counteracting beam under-generation.
//
// `score` is a raw sum of log-probabilities, so it shrinks with every step a
// hypothesis takes. Hypotheses do NOT take the same number of steps: a blank
// advances `t` by up to `max(tdt_durations)` frames in one cheap decision,
// while transcribing the same span costs one negative log-prob per emitted
// token. A path that skips content therefore reaches `nframes` in fewer steps,
// pays fewer penalties, and can outscore the path that transcribes it. On the
// canonical corpus this dropped the entire first window on 7 of 91
// multi-window clips.
//
// Averaging per step does not fix it — blanks are confidently predicted, so a
// blank-heavy path has an average log-prob closest to zero and still wins. The
// bias is specifically against EMITTING, so the correction is a fixed reward
// per emitted token (a length reward / negative word-insertion penalty).
//
// Width 1 is unaffected either way: with one hypothesis, `score` never decides
// anything.
//
// This is a counterweight, not a cure. The bias is structural — hypotheses at
// different `t` are compared on sums over different step counts — and the
// principled fix is time-synchronous pruning (only compare hypotheses at equal
// `t`). This constant is also tuned on one corpus. Revisit both if the corpus
// or the model artifact changes.

/// Windows/ONNX value: 0.10, not macOS's 0.15. At 0.15, Windows field
/// recordings still paid for a spurious leading filler ("Okay. Testing one,
/// two, three." on a clip that is "Testing one, two, three."); 0.10 did not,
/// while sitting inside the macOS corpus's flat optimum described below.
/// See `policy::WINDOWS_EMIT_REWARD` for the full evidence.
#[cfg(target_os = "windows")]
const DEFAULT_EMIT_REWARD: f32 = super::policy::WINDOWS_EMIT_REWARD;
/// macOS's validated value. Tuned on the canonical 224-clip corpus,
/// production finalization path, beam width 2. The curve is unimodal with a
/// flat optimum: 7.24% at 0.0 (no reward), 6.94% at 0.05, 6.66% across
/// 0.10-0.20, then 6.78% / 6.86% / 7.21% / 7.53% at 0.25 / 0.40 / 0.50 / 0.75
/// as over-generation takes over. 0.15 is the centre of the flat region, so
/// the value is insensitive to a 2x error in either direction. Every other
/// non-Windows target keeps this same value.
#[cfg(not(target_os = "windows"))]
const DEFAULT_EMIT_REWARD: f32 = super::policy::MACOS_POST_C0239763_EMIT_REWARD;

/// Platform default emission reward. Callers may override it through
/// `DecodePolicy`; RightKit reads no product-prefixed environment variables.
pub const fn default_emit_reward() -> f32 {
    DEFAULT_EMIT_REWARD
}

/// Numerically-stable log-softmax, delegated to the shared first-party core.
pub fn log_softmax(logits: &[f32]) -> Vec<f32> {
    super::scoring::log_softmax(logits)
}

/// Rank candidate token indices by `logit + context-bias bonus`, descending,
/// returning the top `k`. Ranks on the RAW logit (not a log-softmax
/// transform) plus the bonus, which is exactly the arithmetic the old
/// `argmax`/`biased_argmax` used — so `rank_top_k(_, _, 1)` always picks the
/// same index those functions did. Ties keep the FIRST (lowest) index: the
/// sort is stable and candidates are enumerated in ascending index order, so
/// a tied group keeps its original relative order — same convention as
/// `argmax`'s strict `>` left-to-right scan (a later equal value never
/// displaces an earlier one).
pub fn rank_top_k(
    logits: &[f32],
    bonuses: &std::collections::HashMap<usize, f32>,
    k: usize,
) -> Vec<usize> {
    // Ranking is on the RAW logit plus the context-bias bonus, never on the
    // log-softmax score — same arithmetic the pre-beam `argmax`/`biased_argmax`
    // used. `top_k_indices` keeps the LOWEST index on ties, which is what this
    // function's stable `sort_by` did.
    let scored: Vec<f32> = logits
        .iter()
        .enumerate()
        .map(|(i, &l)| l + bonuses.get(&i).copied().unwrap_or(0.0))
        .collect();
    super::scoring::top_k_indices(&scored, k.max(1))
}

/// Force a hypothesis stuck at its `max_symbols` cap to advance one frame
/// without another decoder/joint call — mirrors the old greedy loop's
/// top-of-loop `if sym >= max_symbols { t += 1; break; }` guard, which never
/// re-queries the model for a step it's about to discard.
pub fn force_time_advance(hyp: &BeamHyp) -> BeamHyp {
    let mut nh = hyp.clone();
    nh.t += 1;
    nh.sym = 0;
    nh
}

/// Expand one hypothesis's TDT joint output into candidate successors (up to
/// `width`, one per top-ranked next token). Pure — no CoreML calls — so this
/// is the "scoring core" the width-1 equivalence tests below drive directly
/// with synthetic logits; `decode_window` calls the exact same function.
///
/// `new_h`/`new_c` are the decoder's updated LSTM state after consuming
/// `hyp.last_tok` — every candidate that stems from this step's joint output
/// shares this same state, because the predictor network's next state
/// depends only on the token ALREADY consumed (`hyp.last_tok`), never on
/// which token comes next. That's what makes it correct to spend exactly one
/// decoder/joint call per active hypothesis per step, regardless of `width`.
#[allow(clippy::too_many_arguments)]
pub fn tdt_step(
    hyp: &BeamHyp,
    logits: &[f32],
    nd: usize,
    durations: &[i64],
    blank: i32,
    max_symbols: usize,
    width: usize,
    emit_reward: f32,
    win_start_sec: f32,
    bonuses: &std::collections::HashMap<usize, f32>,
    new_h: &[f32],
    new_c: &[f32],
) -> Vec<BeamHyp> {
    let tok_logits = &logits[..logits.len() - nd];
    let dur_logits = &logits[logits.len() - nd..];
    // Duration is never biased and never beamed — it's the acoustic timing
    // prediction for THIS step's joint output, independent of which token
    // candidate a hypothesis picks, identical to the pre-beam decode.
    let dur = durations[argmax(dur_logits)] as usize;
    let log_probs = log_softmax(tok_logits);
    rank_top_k(tok_logits, bonuses, width)
        .into_iter()
        .map(|tok| {
            let bonus = bonuses.get(&tok).copied().unwrap_or(0.0);
            let mut nh = hyp.clone();
            // `score_emission` is used for BOTH branches on purpose. The core's
            // `score_blank` takes no bias bonus, but this decoder adds `bonus`
            // to every candidate including blank. Blank can never carry a bonus
            // today (phrase encoding skips `<...>` pieces), but "true today"
            // is exactly the assumption class that produced the macOS/Windows
            // divergences, so the arithmetic is reproduced rather than assumed.
            // Duration is not scored by this decoder, hence `duration_logp` 0.0.
            let is_emission = tok as i32 != blank;
            nh.score = super::scoring::score_emission(
                nh.score,
                log_probs[tok],
                0.0,
                if is_emission { emit_reward } else { 0.0 },
                bonus,
            );
            if is_emission {
                nh.hits
                    .push((tok, win_start_sec + hyp.t as f32 * 0.08, dur));
                nh.last_tok = tok as i32;
                nh.h = new_h.to_vec();
                nh.c = new_c.to_vec();
                nh.sym += 1;
                if dur == 0 && nh.sym < max_symbols {
                    // Another symbol is emitted at this same frame — stay put.
                } else {
                    nh.t += dur.max(1);
                    nh.sym = 0;
                }
            } else {
                // Blank: no emission, predictor state does NOT advance (last_tok/h/c
                // stay as they entered this step) — only the time index moves.
                nh.t += dur.max(1);
                nh.sym = 0;
            }
            nh
        })
        .collect()
}

/// Expand one hypothesis's plain RNN-T joint output (no duration head) into
/// candidate successors. Same shape as `tdt_step`, minus duration handling:
/// blank advances one frame with no state update; a non-blank token emits
/// and chains at the SAME frame until blank or the `max_symbols` cap forces
/// (via `force_time_advance`, driven by the caller) an advance.
#[allow(clippy::too_many_arguments)]
pub fn rnnt_step(
    hyp: &BeamHyp,
    logits: &[f32],
    blank: i32,
    width: usize,
    emit_reward: f32,
    win_start_sec: f32,
    bonuses: &std::collections::HashMap<usize, f32>,
    new_h: &[f32],
    new_c: &[f32],
) -> Vec<BeamHyp> {
    let log_probs = log_softmax(logits);
    rank_top_k(logits, bonuses, width)
        .into_iter()
        .map(|tok| {
            let bonus = bonuses.get(&tok).copied().unwrap_or(0.0);
            let mut nh = hyp.clone();
            // See `tdt_step`: both branches go through `score_emission` so the
            // uniform bias bonus is preserved. Duration is unscored here too.
            let is_blank = tok as i32 == blank;
            nh.score = super::scoring::score_emission(
                nh.score,
                log_probs[tok],
                0.0,
                if is_blank { 0.0 } else { emit_reward },
                bonus,
            );
            if is_blank {
                nh.t += 1;
                nh.sym = 0;
            } else {
                nh.hits.push((tok, win_start_sec + hyp.t as f32 * 0.08, 1));
                nh.last_tok = tok as i32;
                nh.h = new_h.to_vec();
                nh.c = new_c.to_vec();
                nh.sym += 1;
            }
            nh
        })
        .collect()
}


#if !os(watchOS) // RightKitSpeech is iOS/macOS; the watch recorder is RightKitAudio
import Foundation

// Pure beam-search scoring core for the Parakeet TDT/RNN-T decode. Parity
// port of macOS's `heardright-parakeet-core/src/tdt_beam.rs` (`BeamHyp`,
// `tdt_step`, `rnnt_step`, `rank_top_k`, `log_softmax`, `emit_reward`,
// `DEFAULT_EMIT_REWARD`, `force_time_advance`). No CoreML calls happen here —
// `ParakeetDecoder.swift`'s `decodeWindow` supplies one decoder/joint call's
// logits per active hypothesis per round and this file turns that into
// scored candidate successors. Kept CoreML-free and dependency-light on
// purpose so the beam/bias arithmetic is directly unit-testable with
// synthetic logits (see `Tests/parakeet_beam_bias_smoke.swift`), the same way
// the Rust tests in `tdt_beam.rs` drive `tdt_step`/`rnnt_step` directly.

/// Per-emission score reward, counteracting beam under-generation. Parity port of Rust's
/// `DEFAULT_EMIT_REWARD`; tuned on the canonical corpus, do not re-tune here.
let parakeetDefaultEmitReward: Float = 0.15

/// Beam width is clamped to `1...8`. Width 1 is bit-identical to greedy decode.
func parakeetClampBeamWidth(_ width: Int) -> Int { min(max(width, 1), 8) }

/// One active beam hypothesis during decode. Parity port of Rust's
/// `BeamHyp`: carries its own predictor LSTM state, its own TDT time index
/// (hypotheses sit at DIFFERENT `t` — never force them into lockstep), and
/// its own emitted-token history (used for both final output and evaluating
/// context-bias matches).
struct ParakeetBeamHyp {
    var h: [Float]
    var c: [Float]
    var lastTok: Int32
    /// Encoder frame index this hypothesis will read next.
    var t: Int
    /// Symbols already emitted at frame `t` (TDT/RNN-T `max_symbols` cap).
    var sym: Int
    /// Cumulative log-probability (+ context-bias bonuses) of this
    /// hypothesis's token choices so far. At beam width 1 there is only ever
    /// one hypothesis, so this value never influences which token is chosen.
    var score: Float
    /// (pieceID, absoluteStartSeconds, durationFrames) per emitted token.
    var hits: [(id: Int, startSec: Float, durationFrames: Int)]
}

/// Numerically-stable log-softmax over raw logits. Parity port of Rust's
/// `log_softmax`.
func parakeetLogSoftmax(_ logits: [Float]) -> [Float] {
    let maxValue = logits.max() ?? -Float.infinity
    let sumExp = logits.reduce(Float(0)) { $0 + expf($1 - maxValue) }
    let logSumExp = maxValue + logf(sumExp)
    return logits.map { $0 - logSumExp }
}

/// Rank candidate token indices by `logit + context-bias bonus`, descending,
/// returning the top `k`. Ranks on the RAW logit (not a log-softmax
/// transform) plus the bonus — exactly the arithmetic the old
/// greedy/biased-argmax used, so `rankTopK(_, _, k: 1)` always picks the same
/// index those did. Ties explicitly prefer FIRST (lowest) index because
/// Swift's sort is unstable; this matches argmax's strict `>` left-to-right
/// scan & Rust's `rank_top_k` policy.
func parakeetRankTopK(_ logits: [Float], bonuses: [Int: Float], k: Int) -> [Int] {
    var scored: [(index: Int, value: Float)] = []
    scored.reserveCapacity(logits.count)
    for (index, logit) in logits.enumerated() {
        scored.append((index, logit + (bonuses[index] ?? 0)))
    }
    // Swift's sort is not stable. Make Rust's shared-core tie policy explicit:
    // equal scores retain the lowest token id, including blank-vs-token ties.
    scored.sort {
        if $0.value == $1.value {
            return $0.index < $1.index
        }
        return $0.value > $1.value
    }
    return scored.prefix(max(k, 1)).map(\.index)
}

/// Index of the maximum value, strict `>` so ties keep the first (lowest)
/// index. Used only for the duration head, which is never biased and never
/// beamed. Parity port of the `argmax` the Rust decode calls for durations.
func parakeetArgmax(_ values: [Float]) -> Int {
    var bestIndex = 0
    var bestValue = -Float.infinity
    for (index, value) in values.enumerated() where value > bestValue {
        bestIndex = index
        bestValue = value
    }
    return bestIndex
}

/// Force a hypothesis stuck at its `max_symbols` cap to advance one frame
/// without another decoder/joint call. Parity port of Rust's
/// `force_time_advance`.
func parakeetForceTimeAdvance(_ hyp: ParakeetBeamHyp) -> ParakeetBeamHyp {
    var next = hyp
    next.t += 1
    next.sym = 0
    return next
}

/// Expand one hypothesis's TDT joint output into candidate successors (up to
/// `width`, one per top-ranked next token). Parity port of Rust's
/// `tdt_step`.
///
/// `newH`/`newC` are the decoder's updated LSTM state after consuming
/// `hyp.lastTok` — every candidate stemming from this step's joint output
/// shares this same state, because the predictor network's next state
/// depends only on the token ALREADY consumed, never on which token comes
/// next.
///
/// AT WIDTH 1 THIS IS BIT-IDENTICAL TO THE ORIGINAL GREEDY DECODE:
/// `parakeetRankTopK` ranks on the raw `logit + bias-bonus` sum, never on the
/// log-softmax `score` field, which only compares ALTERNATIVE hypotheses and
/// is a no-op with only one hypothesis in the beam.
func parakeetTdtStep(
    hyp: ParakeetBeamHyp,
    logits: [Float],
    numDurations: Int,
    durations: [Int],
    blank: Int32,
    maxSymbols: Int,
    width: Int,
    winStartSec: Float,
    frameSeconds: Float,
    bonuses: [Int: Float],
    emitReward: Float = parakeetDefaultEmitReward,
    newH: [Float],
    newC: [Float]
) -> [ParakeetBeamHyp] {
    let tokenLogits = Array(logits[0..<(logits.count - numDurations)])
    let durationLogits = Array(logits[(logits.count - numDurations)...])
    // Duration is never biased and never beamed — the acoustic timing
    // prediction for THIS step's joint output, independent of which token
    // candidate a hypothesis picks, identical to the pre-beam decode.
    let duration = durations[parakeetArgmax(durationLogits)]
    let logProbs = parakeetLogSoftmax(tokenLogits)
    return parakeetRankTopK(tokenLogits, bonuses: bonuses, k: width).map { tok in
        let bonus = bonuses[tok] ?? 0
        var next = hyp
        next.score += logProbs[tok] + bonus
        if Int32(tok) != blank {
            next.score += emitReward
            next.hits.append((tok, winStartSec + Float(hyp.t) * frameSeconds, duration))
            next.lastTok = Int32(tok)
            next.h = newH
            next.c = newC
            next.sym += 1
            if duration == 0 && next.sym < maxSymbols {
                // Another symbol is emitted at this same frame — stay put.
            } else {
                next.t += max(duration, 1)
                next.sym = 0
            }
        } else {
            // Blank: no emission, predictor state does NOT advance — only
            // the time index moves.
            next.t += max(duration, 1)
            next.sym = 0
        }
        return next
    }
}

/// Expand one hypothesis's plain RNN-T joint output (no duration head) into
/// candidate successors. Same shape as `parakeetTdtStep`, minus duration
/// handling: blank advances one frame with no state update; a non-blank
/// token emits and chains at the SAME frame until blank or the
/// `max_symbols` cap forces (via `parakeetForceTimeAdvance`, driven by the
/// caller) an advance. Parity port of Rust's `rnnt_step`.
func parakeetRnntStep(
    hyp: ParakeetBeamHyp,
    logits: [Float],
    blank: Int32,
    width: Int,
    winStartSec: Float,
    frameSeconds: Float,
    bonuses: [Int: Float],
    emitReward: Float = parakeetDefaultEmitReward,
    newH: [Float],
    newC: [Float]
) -> [ParakeetBeamHyp] {
    let logProbs = parakeetLogSoftmax(logits)
    return parakeetRankTopK(logits, bonuses: bonuses, k: width).map { tok in
        let bonus = bonuses[tok] ?? 0
        var next = hyp
        next.score += logProbs[tok] + bonus
        if Int32(tok) == blank {
            next.t += 1
            next.sym = 0
        } else {
            next.score += emitReward
            next.hits.append((tok, winStartSec + Float(hyp.t) * frameSeconds, 1))
            next.lastTok = Int32(tok)
            next.h = newH
            next.c = newC
            next.sym += 1
        }
        return next
    }
}
#endif

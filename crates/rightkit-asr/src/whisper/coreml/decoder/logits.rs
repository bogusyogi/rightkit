// WhisperKit LogitsFilter chain (SuppressBlank → TimestampRules) plus argmax.
// The token-id constants it guards live in `engine.rs` next to the prompt build.
use super::{BEGIN_SUPPRESS, EOT, NO_TS, TS_BEGIN, VOCAB};

const NEG: f32 = f32::NEG_INFINITY;

/// WhisperKit's LogitsFilter chain for one content step, in order (matches
/// TextDecoder.createLogitsFilters): SuppressBlank → SuppressTokens → TimestampRules.
/// `current` is the full token list (prompt + sampled); `init_prompt` is the prompt
/// length (4 in timestamp mode), which is both the SuppressBlank trigger point and
/// the TimestampRules sampleBegin (= max(transcribeIdx+1, initialPromptIndex)).
pub(crate) fn apply_filters(logits: &mut [f32], current: &[i32], init_prompt: usize, ts_mode: bool) {
    // NOTE: WhisperKit's default `suppressTokens` is EMPTY (the non-speech list is an
    // unimplemented item in DecodingOptions), and the cli ships []. Suppressing the 88
    // generation_config ids here corrupted real words (e.g. "metro" → "mê trông"), so
    // we match WhisperKit and suppress nothing beyond blank + timestamp rules.
    // SuppressBlankFilter — at the first sampled token, suppress [whitespace, eot].
    if current.len() == init_prompt {
        for &t in BEGIN_SUPPRESS {
            logits[t] = NEG;
        }
    }
    // TimestampRulesFilter — only added when decoding WITH timestamps.
    if ts_mode {
        apply_timestamp_rules(logits, &current[init_prompt..]);
    } else {
        // no-ts: model is prompted with <|notimestamps|>; forbid timestamps explicitly.
        for logit in logits.iter_mut().take(VOCAB).skip(TS_BEGIN as usize) {
            *logit = NEG;
        }
    }
}

/// WhisperKit `TimestampRulesFilter` (port of openai/whisper decoding.py): timestamps
/// appear in pairs and never decrease. Probability-mass forcing is deliberately
/// omitted for dictation. Initial timestamp is forced via the
/// prompt's <|0.00|> (WhisperKit disabled the in-filter init-ts block).
fn apply_timestamp_rules(logits: &mut [f32], sampled: &[i32]) {
    logits[NO_TS as usize] = NEG; // never emit <|notimestamps|>
    let n = sampled.len();
    if n > 0 {
        let last_ts = sampled[n - 1] >= TS_BEGIN;
        // WhisperKit: count < 2 ⇒ penultimate-was-timestamp = true.
        let penult_ts = n < 2 || sampled[n - 2] >= TS_BEGIN;
        if last_ts {
            if penult_ts {
                // has to be non-timestamp: suppress timestamps
                for logit in logits.iter_mut().take(VOCAB).skip(TS_BEGIN as usize) {
                    *logit = NEG;
                }
            } else {
                // cannot be normal text: suppress text (0..eot)
                for logit in logits.iter_mut().take(EOT as usize) {
                    *logit = NEG;
                }
            }
        }
        // non-decreasing + nonzero segment length: closing ts ≥ last; else strictly >
        if let Some(&lt) = sampled.iter().rev().find(|&&t| t >= TS_BEGIN) {
            let lo = if last_ts && !penult_ts { lt } else { lt + 1 };
            for logit in logits
                .iter_mut()
                .take(lo as usize)
                .skip(TS_BEGIN as usize)
            {
                *logit = NEG;
            }
        }
    }
    // Do not force timestamps from their combined probability mass. That rule
    // can mask the best speech token & end transcription before spoken words.
    // Keep timestamp pairing/monotonicity; let individual logits select speech.
    // Evidence: docs/experiments/2026-09-27-whisper-policy-comparison.md
}

pub(crate) fn argmax(v: &[f32]) -> usize {
    let mut bi = 0usize;
    let mut bv = f32::NEG_INFINITY;
    for (i, &x) in v.iter().enumerate() {
        if x > bv {
            bv = x;
            bi = i;
        }
    }
    bi
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_filter_keeps_best_speech_when_timestamp_mass_is_larger() {
        let mut logits = vec![NEG; VOCAB];
        let speech = 1176; // Start of omitted speech in captured row-300 trace.
        logits[speech] = 2.0;
        logits[TS_BEGIN as usize + 1] = 1.5;
        logits[TS_BEGIN as usize + 2] = 1.5;
        let current = [50258, 50259, 50360, TS_BEGIN, 100];
        apply_filters(&mut logits, &current, 4, true);
        assert_eq!(argmax(&logits), speech);
        assert!(logits[TS_BEGIN as usize + 1].is_finite());
    }

    #[test]
    fn production_filter_preserves_timestamp_pairing_and_monotonicity() {
        let mut logits = vec![0.0; VOCAB];
        let current = [50258, 50259, 50360, TS_BEGIN, 100, TS_BEGIN + 10];
        apply_filters(&mut logits, &current, 4, true);
        assert_eq!(logits[100], NEG);
        assert_eq!(logits[NO_TS as usize], NEG);
        assert_eq!(logits[TS_BEGIN as usize + 9], NEG);
        assert!(logits[EOT as usize].is_finite());
        assert!(logits[TS_BEGIN as usize + 10].is_finite());
    }
}

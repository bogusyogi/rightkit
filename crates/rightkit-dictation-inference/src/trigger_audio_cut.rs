use crate::asr;

const SAMPLE_RATE: usize = 16_000;
const COMMAND_PREROLL_SAMPLES: usize = SAMPLE_RATE / 4;

/// `cut_source` of a cut made at the KWS onset because no pause preceded it.
pub const KWS_ONSET_CUT_SOURCE: &str = "kws_onset";

#[derive(Debug, PartialEq)]
pub enum TriggerAudioCut {
    Cut {
        onset_sample: usize,
        cut_at: usize,
        cut_source: &'static str,
        seam_rms: Option<f32>,
        noise_floor_rms: Option<f32>,
    },
    FullBuffer {
        reason: &'static str,
    },
}

/// Resolves a safe low-energy cut before a confirmed control utterance.
pub fn resolve_trigger_audio_cut(
    trigger_onset_sample: Option<usize>,
    onset_skip_reason: Option<&'static str>,
    speech_start_sample: Option<usize>,
    samples: &[f32],
) -> TriggerAudioCut {
    let Some(onset) = trigger_onset_sample else {
        return TriggerAudioCut::FullBuffer {
            reason: onset_skip_reason.unwrap_or("no_timestamps"),
        };
    };
    let floor = speech_start_sample.unwrap_or(0);
    let max_vad_preroll = COMMAND_PREROLL_SAMPLES + SAMPLE_RATE / 32;
    if onset == 0 || onset > samples.len() || onset.saturating_add(max_vad_preroll) < floor {
        return TriggerAudioCut::FullBuffer {
            reason: "implausible",
        };
    }
    match asr::confident_static15_terminal_cut(samples, floor, onset) {
        Some(seam) if seam.cut_at > floor => TriggerAudioCut::Cut {
            onset_sample: onset,
            cut_at: seam.cut_at,
            cut_source: "static15_seam",
            seam_rms: Some(seam.seam_rms),
            noise_floor_rms: Some(seam.noise_floor_rms),
        },
        // No pause before the command: cut exactly at the spotter's onset.
        // The spotter fires slightly late, so the caller must strip the
        // "zephyr" fragment with `strip_kws_onset_cut_residue` (measured on
        // 103 no-pause recordings, 2026-09-23).
        _ if onset > floor => TriggerAudioCut::Cut {
            onset_sample: onset,
            cut_at: onset,
            cut_source: KWS_ONSET_CUT_SOURCE,
            seam_rms: None,
            noise_floor_rms: None,
        },
        _ => TriggerAudioCut::FullBuffer {
            reason: "no_precommand_speech",
        },
    }
}

/// Drops every whole word that STARTS at or after exact `boundary_sec`
/// from the final text. The boundary comes from a forced alignment of the
/// spotter-confirmed command, so no spelling is consulted. Trims trailing words
/// of `text` by count and only when they agree with the token words; at most
/// eight words. Non-finite boundaries or token timestamps are rejected.
/// `None` means "leave the text alone".
pub fn drop_words_from_boundary(
    text: &str,
    tokens: &[crate::TimedToken],
    boundary_sec: f32,
) -> Option<(String, Vec<crate::TimedToken>)> {
    if !boundary_sec.is_finite() {
        return None;
    }
    let mut words: Vec<(usize, f32, String)> = Vec::new();
    let mut previous_start = None;
    for (index, token) in tokens.iter().enumerate() {
        if !token.start.is_finite()
            || !token.end.is_finite()
            || token.start < 0.0
            || token.end < 0.0
            || token.end < token.start
            || previous_start.is_some_and(|start| token.start < start)
        {
            return None;
        }
        previous_start = Some(token.start);
        if token.text.starts_with(char::is_whitespace) || words.is_empty() {
            words.push((index, token.start, token.text.trim().to_string()));
        } else if let Some(word) = words.last_mut() {
            word.2.push_str(&token.text);
        }
    }
    let boundary = words.iter().position(|word| word.1 >= boundary_sec)?;
    if words[boundary..].iter().any(|word| word.1 < boundary_sec) {
        return None;
    }
    let dropped = words.len() - boundary;
    if dropped == 0 || dropped > 8 {
        return None;
    }
    let plain = |word: &str| {
        word.chars()
            .filter(|c| c.is_alphanumeric())
            .collect::<String>()
            .to_lowercase()
    };
    let text_words: Vec<&str> = text.split_whitespace().collect();
    if text_words.len() < dropped
        || text_words[text_words.len() - dropped..]
            .iter()
            .zip(&words[boundary..])
            .any(|(a, b)| plain(a) != plain(&b.2))
    {
        return None;
    }
    let mut end = text.trim_end().len();
    for _ in 0..dropped {
        let head = text[..end].trim_end();
        end = head.rfind(char::is_whitespace).unwrap_or(0);
    }
    Some((
        text[..end].trim_end().to_string(),
        tokens[..words[boundary].0].to_vec(),
    ))
}

#[cfg(test)]
mod boundary_trim_tests {
    use super::*;

    fn tok(text: &str, start: f32, end: f32) -> crate::TimedToken {
        crate::TimedToken {
            text: text.into(),
            start,
            end,
        }
    }

    #[test]
    fn only_words_starting_at_or_after_the_boundary_go_and_short_words_before_it_stay() {
        let tokens = [
            tok("care", 9.00, 9.20),
            tok(" fix", 9.30, 9.54),
            tok(" it", 9.54, 9.62),
            tok(" Jeff", 9.78, 10.0),
            tok("erson", 10.0, 10.26),
        ];
        let (text, kept) =
            drop_words_from_boundary("care fix it Jefferson", &tokens, 9.76).unwrap();
        assert_eq!(text, "care fix it");
        assert_eq!(kept.len(), 3);
        // Boundary after every word: nothing to drop.
        assert!(drop_words_from_boundary("care fix it", &tokens[..3], 12.0).is_none());
        // Exact boundary semantics: a word beginning 40 ms before boundary is kept.
        assert!(drop_words_from_boundary("care fix it Jefferson", &tokens, 9.80).is_none());
        // Text and tokens disagree at the tail: hands off.
        assert!(drop_words_from_boundary("care fix it Jackson", &tokens, 9.76).is_none());
        // No timings: hands off.
        assert!(drop_words_from_boundary("x", &[], 1.0).is_none());
        // Malformed timing metadata: hands off without mutating canonical text.
        assert!(drop_words_from_boundary(
            "care fix it Jefferson",
            &[tok("care", 9.0, 9.2), tok(" fix", f32::NAN, 9.54)],
            9.76
        )
        .is_none());
        assert!(drop_words_from_boundary(
            "care fix it Jefferson",
            &[tok("care", 9.0, 9.2), tok(" fix", 9.30, f32::INFINITY)],
            9.76
        )
        .is_none());
        assert!(drop_words_from_boundary("care fix it Jefferson", &tokens, f32::NAN).is_none());
        // A command-shaped token followed by a later genuine word before the
        // boundary is malformed timing; never drop that word by suffix count.
        assert!(drop_words_from_boundary(
            "care zephyr stop answer",
            &[
                tok("care", 8.0, 8.2),
                tok(" zephyr", 10.0, 10.2),
                tok(" stop", 10.2, 10.4),
                tok(" answer", 9.0, 9.2),
            ],
            10.0,
        )
        .is_none());
        assert!(drop_words_from_boundary(
            "care zephyr stop",
            &[tok("care", -0.1, 0.2), tok(" zephyr", 1.0, 1.2)],
            1.0,
        )
        .is_none());
    }
}

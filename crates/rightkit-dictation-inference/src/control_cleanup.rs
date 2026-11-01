//! Cleanup of a confirmed terminal control, retaining original ASR context.
//! Call only after KWS stop/send; manual stops & spoken cancel use their own paths.
use crate::{TimedToken, TranscriptionResult};
use rightkit_text_polish::text_pipeline::{strip_fired_control_tail, ControlIntent};

pub struct Alignment {
    pub boundary_sec: f32,
    pub accepted: bool,
    pub method: &'static str,
    pub score: f32,
    pub joint_calls: usize,
}

pub struct CleanupResult {
    pub transcript: TranscriptionResult,
    pub method: &'static str,
    pub boundary_sec: Option<f32>,
    pub score: Option<f32>,
    pub joint_calls: usize,
    pub error: Option<String>,
}

// TDT word starts trail the acoustic command onset by up to two encoder frames.
// A word that starts that close before the boundary and runs past it is the
// misrecognised first command word ("Zachary send" field case: word 35.38-36.02 s,
// boundary 35.42 s), so the cut moves to its start.
const STRADDLE_START_TOLERANCE_SEC: f32 = 0.16;

fn snap_boundary_to_straddling_word(tokens: &[TimedToken], boundary: f32) -> f32 {
    let mut words: Vec<(f32, f32)> = Vec::new();
    for token in tokens {
        if !token.start.is_finite() || !token.end.is_finite() || token.end < token.start {
            return boundary;
        }
        if token.text.starts_with(char::is_whitespace) || words.is_empty() {
            words.push((token.start, token.end));
        } else if let Some((_, end)) = words.last_mut() {
            *end = token.end;
        }
    }
    words
        .iter()
        .find(|(start, end)| {
            *start < boundary
                && boundary - *start <= STRADDLE_START_TOLERANCE_SEC
                && *end > boundary
        })
        .map_or(boundary, |(start, _)| *start)
}

fn cleaned_text(
    original: &str,
    tokens: &[TimedToken],
    intent: ControlIntent,
    boundary: f32,
) -> String {
    let boundary = snap_boundary_to_straddling_word(tokens, boundary);
    let trimmed = crate::trigger_audio_cut::drop_words_from_boundary(original, tokens, boundary)
        .map(|(text, _)| text)
        .unwrap_or_else(|| original.to_string());
    let cleaned = strip_fired_control_tail(&trimmed, intent);
    recover_rendering(original, tokens, intent, boundary).unwrap_or(cleaned)
}

// Compare words, not punctuation: strip_fired_control_tail also trims punctuation
// when it finds no command. That alone must not suppress acoustic fallback.
fn words(text: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut word = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_alphanumeric()
            || (matches!(c, '\'' | '’')
                && !word.is_empty()
                && chars.peek().is_some_and(|next| next.is_alphanumeric()))
        {
            word.extend(c.to_lowercase());
        } else if !word.is_empty() {
            result.push(std::mem::take(&mut word));
        }
    }
    if !word.is_empty() {
        result.push(word);
    }
    result
}

fn replace_text(full: &mut TranscriptionResult, text: String) {
    if full.text != text {
        full.tokens.clear(); // Timing no longer describes exactly this rendered text.
        full.text = text;
    }
}

// Fast alignment can start inside or just after first ASR command word. A
// nearby retained word makes partial removal ambiguous; retry stronger acoustic
// alignment. This guard triggers verification, never a backward text cut.
fn retained_word_near_boundary(tokens: &[TimedToken], boundary: f32) -> bool {
    let mut groups = Vec::new();
    for token in tokens {
        if !token.start.is_finite()
            || !token.end.is_finite()
            || token.start < 0.0
            || token.end < token.start
        {
            return false;
        }
        if token.text.starts_with(char::is_whitespace) || groups.is_empty() {
            groups.push((token.start, token.end));
        } else if let Some((_, end)) = groups.last_mut() {
            *end = token.end;
        }
    }
    let first_after = groups.iter().position(|(start, _)| *start >= boundary);
    first_after
        .and_then(|index| index.checked_sub(1).map(|previous| (previous, index)))
        .is_some_and(|(previous, _)| {
            let (start, _) = groups[previous];
            start < boundary && boundary - start <= 0.32
        })
}

/// Production cleanup policy; the callback borrows the already loaded ASR model.
/// Audio is raw 16kHz mono, onset is absolute in that same buffer.
fn cleanup_confirmed_control_impl(
    mut full: TranscriptionResult,
    raw_audio: &[f32],
    intent: ControlIntent,
    onset: usize,
    mut align: impl FnMut(&[f32], &str, f32, bool) -> Result<Alignment, String>,
    allow_exact_retry: bool,
) -> CleanupResult {
    let original = full.text.clone();
    let stripped = strip_fired_control_tail(&original, intent);
    let mut outcome = CleanupResult {
        transcript: TranscriptionResult {
            text: String::new(),
            tokens: Vec::new(),
        },
        method: "text_strip",
        boundary_sec: None,
        score: None,
        joint_calls: 0,
        error: None,
    };
    if words(&stripped) != words(&original) || matches!(intent, ControlIntent::Cancel) {
        replace_text(&mut full, stripped);
        outcome.transcript = full;
        return outcome;
    }
    let start = raw_audio.len().saturating_sub(14 * 16_000);
    let end = onset.saturating_add(2 * 16_000).min(raw_audio.len());
    if onset == 0 || onset > raw_audio.len() || onset < start || start >= end {
        replace_text(&mut full, stripped);
        outcome.method = "invalid_onset";
        outcome.transcript = full;
        return outcome;
    }
    let audio = rightkit_dictation_core::audio_conditioning::condition_for_asr(
        &raw_audio[start..end],
        16_000,
        "default",
    );
    let command = match intent {
        ControlIntent::Send => "zephyr send",
        ControlIntent::Stop => "zephyr stop",
        ControlIntent::Cancel => unreachable!(),
    };
    match align(
        &audio,
        command,
        (onset - start) as f32 / 16_000.0 - 1.0,
        false,
    ) {
        Ok(mut found) => {
            outcome.method = found.method;
            outcome.score = found.score.is_finite().then_some(found.score);
            outcome.joint_calls = found.joint_calls;
            let boundary = start as f32 / 16_000.0 + found.boundary_sec;
            let valid = found.boundary_sec.is_finite()
                && found.boundary_sec >= 0.0
                && found.boundary_sec <= audio.len() as f32 / 16_000.0;
            if found.accepted && valid {
                outcome.boundary_sec = Some(boundary);
                let mut cleaned = cleaned_text(&original, &full.tokens, intent, boundary);
                let recovered_command =
                    recover_rendering(&original, &full.tokens, intent, boundary).is_some();
                if allow_exact_retry
                    && found.method == "onnx_fast"
                    && !recovered_command
                    && (words(&cleaned) == words(&original)
                        || retained_word_near_boundary(&full.tokens, boundary))
                {
                    match align(
                        &audio,
                        command,
                        (onset - start) as f32 / 16_000.0 - 1.0,
                        true,
                    ) {
                        Ok(exact) => {
                            outcome.joint_calls =
                                outcome.joint_calls.saturating_add(exact.joint_calls);
                            let exact_boundary = start as f32 / 16_000.0 + exact.boundary_sec;
                            let exact_valid = exact.boundary_sec.is_finite()
                                && exact.boundary_sec >= 0.0
                                && exact.boundary_sec <= audio.len() as f32 / 16_000.0;
                            if exact.accepted && exact_valid {
                                let exact_cleaned =
                                    cleaned_text(&original, &full.tokens, intent, exact_boundary);
                                if words(&exact_cleaned) != words(&original) {
                                    found = exact;
                                    outcome.method = found.method;
                                    outcome.score = found.score.is_finite().then_some(found.score);
                                    outcome.boundary_sec = Some(exact_boundary);
                                    cleaned = exact_cleaned;
                                }
                            }
                        }
                        Err(error) => outcome.error = Some(format!("exact retry: {error}")),
                    }
                }
                replace_text(&mut full, cleaned);
            } else {
                outcome.method = if valid {
                    "alignment_rejected"
                } else {
                    "invalid_boundary"
                };
                replace_text(&mut full, stripped);
            }
        }
        Err(error) => {
            outcome.method = "alignment_error";
            outcome.error = Some(error);
            replace_text(&mut full, stripped);
        }
    }
    outcome.transcript = full;
    outcome
}

pub fn cleanup_confirmed_control(
    full: TranscriptionResult,
    raw_audio: &[f32],
    intent: ControlIntent,
    onset: usize,
    mut align: impl FnMut(&[f32], &str, f32) -> Result<Alignment, String>,
) -> CleanupResult {
    cleanup_confirmed_control_impl(
        full,
        raw_audio,
        intent,
        onset,
        |audio, command, earliest, _| align(audio, command, earliest),
        false,
    )
}

pub fn cleanup_confirmed_control_with_exact_retry(
    full: TranscriptionResult,
    raw_audio: &[f32],
    intent: ControlIntent,
    onset: usize,
    align: impl FnMut(&[f32], &str, f32, bool) -> Result<Alignment, String>,
) -> CleanupResult {
    cleanup_confirmed_control_impl(full, raw_audio, intent, onset, align, true)
}

// Frozen whole-command renderings from the 310-recording experiment. Never
// delete standalone ordinary vocabulary. Require matching timed suffix crossing
// the measured command boundary (same -0.32/+0.16s guard as frozen scorer).
fn recover_rendering(
    raw: &str,
    tokens: &[TimedToken],
    intent: ControlIntent,
    boundary: f32,
) -> Option<String> {
    let patterns: &[&[&str]] = match intent {
        ControlIntent::Send => &[
            &["is", "that", "for", "send"],
            &["is", "that", "for", "sand"],
            &["is", "that", "for", "sun"],
            &["cephers", "and"],
            &["sephersan"],
        ],
        ControlIntent::Stop => &[
            &["is", "that", "for", "stop"],
            &["is", "that", "for", "stub"],
            &["is", "that", "for", "step"],
            &["is", "that", "first", "stop"],
            &["is", "that", "first", "stub"],
            &["is", "that", "first", "step"],
        ],
        ControlIntent::Cancel => return None,
    };
    let tail =
        raw.trim_end_matches(|c: char| c.is_whitespace() || matches!(c, '.' | ',' | '?' | '!'));
    let pieces: Vec<&str> = tail.split_whitespace().collect();
    let pattern = patterns.iter().find(|pattern| {
        pieces.len() >= pattern.len()
            && pieces[pieces.len() - pattern.len()..]
                .iter()
                .zip(pattern.iter())
                .all(|(word, expected)| word.eq_ignore_ascii_case(expected))
    })?;
    let first = pieces[pieces.len() - pattern.len()];
    let cut = first.as_ptr() as usize - raw.as_ptr() as usize;
    let normalize = |s: &str| {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    let mut groups: Vec<(String, f32, f32)> = Vec::new();
    for token in tokens {
        if !token.start.is_finite() || !token.end.is_finite() {
            return None;
        }
        if token.text.starts_with(char::is_whitespace) || groups.is_empty() {
            groups.push((token.text.trim().to_string(), token.start, token.end));
        } else {
            let group = groups.last_mut()?;
            group.0.push_str(&token.text);
            group.2 = token.end;
        }
    }
    let suffix = groups.get(groups.len().checked_sub(pattern.len())?..)?;
    if !suffix
        .iter()
        .zip(pattern.iter())
        .all(|(g, w)| normalize(&g.0) == *w)
    {
        return None;
    }
    let start = suffix.first()?.1;
    let end = suffix.last()?.2;
    if start < boundary - 0.32 || start > boundary + 0.16 || end < boundary {
        return None;
    }
    Some(
        raw[..cut]
            .trim_end_matches(|c: char| {
                c.is_whitespace() || matches!(c, ',' | '.' | '!' | '?' | ';' | ':')
            })
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn transcript(text: &str, words: &[(&str, f32)]) -> TranscriptionResult {
        TranscriptionResult {
            text: text.into(),
            tokens: words
                .iter()
                .map(|(text, start)| TimedToken {
                    text: (*text).into(),
                    start: *start,
                    end: *start + 0.1,
                })
                .collect(),
        }
    }
    #[test]
    fn production_entry_skips_model_when_words_strip_but_not_for_punctuation() {
        let clean = cleanup_confirmed_control(
            transcript("Keep this Zephyr send!", &[]),
            &[],
            ControlIntent::Send,
            0,
            |_, _, _| panic!("text cleanup must skip model"),
        );
        assert_eq!(clean.transcript.text, "Keep this");
        assert_eq!(clean.method, "text_strip");
        let mut called = false;
        let clean = cleanup_confirmed_control(
            transcript(
                "Keep this Jefferson!",
                &[("Keep", 0.0), (" this", 0.4), (" Jefferson", 1.0)],
            ),
            &vec![0.0; 32000],
            ControlIntent::Send,
            16000,
            |audio, command, earliest| {
                called = true;
                assert_eq!(command, "zephyr send");
                assert_eq!(audio.len(), 32000);
                assert_eq!(earliest, 0.0);
                Ok(Alignment {
                    boundary_sec: 0.96,
                    accepted: true,
                    method: "fast",
                    score: -8.0,
                    joint_calls: 2,
                })
            },
        );
        assert!(called);
        assert_eq!(clean.transcript.text, "Keep this");
        assert!(clean.transcript.tokens.is_empty());
    }

    #[test]
    fn exact_retry_entry_skips_model_when_text_strip_removes_words() {
        let clean = cleanup_confirmed_control_with_exact_retry(
            transcript("Keep this Zephyr send!", &[]),
            &[],
            ControlIntent::Send,
            0,
            |_, _, _, _| panic!("text cleanup must skip model"),
        );
        assert_eq!(clean.method, "text_strip");
        assert_eq!(clean.transcript.text, "Keep this");
    }

    #[test]
    fn legacy_entry_calls_fast_alignment_once() {
        let full = transcript(
            "Keep this Jefferson",
            &[("Keep", 0.0), (" this", 0.4), (" Jefferson", 1.0)],
        );
        let mut calls = 0;
        let out = cleanup_confirmed_control(
            full,
            &vec![0.0; 32_000],
            ControlIntent::Send,
            16_000,
            |_, _, _| {
                calls += 1;
                Ok(Alignment {
                    boundary_sec: 2.0,
                    accepted: true,
                    method: "onnx_fast",
                    score: -4.0,
                    joint_calls: 2,
                })
            },
        );
        assert_eq!(calls, 1);
        assert_eq!(out.transcript.text, "Keep this Jefferson");
    }

    #[test]
    fn punctuation_only_fast_result_still_retries_exact() {
        let full = transcript(
            "Keep this Jefferson!",
            &[("Keep", 0.0), (" this", 0.4), (" Jefferson", 1.0)],
        );
        let mut exact_calls = 0;
        let out = cleanup_confirmed_control_with_exact_retry(
            full,
            &vec![0.0; 32_000],
            ControlIntent::Send,
            16_000,
            |_, _, _, exact| {
                if exact {
                    exact_calls += 1;
                    Ok(Alignment {
                        boundary_sec: 1.0,
                        accepted: true,
                        method: "onnx_exact",
                        score: -4.0,
                        joint_calls: 3,
                    })
                } else {
                    Ok(Alignment {
                        boundary_sec: 2.0,
                        accepted: true,
                        method: "onnx_fast",
                        score: -4.0,
                        joint_calls: 2,
                    })
                }
            },
        );
        assert_eq!(exact_calls, 1);
        assert_eq!(out.transcript.text, "Keep this");
        assert_eq!(out.joint_calls, 5);
    }
    #[test]
    fn production_entry_preserves_speech_on_rejection_error_or_bad_boundary() {
        for mode in 0..3 {
            let out = cleanup_confirmed_control(
                transcript(
                    "Keep this Jefferson",
                    &[("Keep", 0.0), (" this", 0.4), (" Jefferson", 1.0)],
                ),
                &vec![0.0; 32000],
                ControlIntent::Send,
                16000,
                |_, _, _| {
                    if mode == 0 {
                        Err("model failure".into())
                    } else {
                        Ok(Alignment {
                            boundary_sec: if mode == 1 { f32::NAN } else { 0.96 },
                            accepted: mode == 1,
                            method: "full",
                            score: -10.0,
                            joint_calls: 5,
                        })
                    }
                },
            );
            assert_eq!(out.transcript.text, "Keep this Jefferson");
            assert!(out.boundary_sec.is_none());
        }
    }

    #[test]
    fn word_straddling_boundary_is_the_command() {
        // Field replay (Edge, Ultra): "Zachary Sen." with the exact boundary 40 ms into "Zachary".
        let tokens = [
            (" Che", 33.94, 34.1),
            ("ck", 34.1, 34.34),
            (" this", 34.34, 34.66),
            (" out", 34.66, 34.98),
            (".", 35.06, 35.38),
            (" Z", 35.38, 35.54),
            ("ach", 35.54, 35.7),
            ("ary", 35.86, 36.02),
            (" Sen", 36.18, 36.34),
            (".", 36.66, 36.74),
        ];
        let full = TranscriptionResult {
            text: "Check this out. Zachary Sen.".into(),
            tokens: tokens
                .iter()
                .map(|(text, start, end)| TimedToken {
                    text: (*text).into(),
                    start: *start,
                    end: *end,
                })
                .collect(),
        };
        let out = cleanup_confirmed_control(
            full,
            &vec![0.0; 37 * 16_000],
            ControlIntent::Send,
            35 * 16_000 + 5_000,
            // The window starts 14 s before the end (23 s); the field boundary is absolute 35.42 s.
            |_, _, _| {
                Ok(Alignment {
                    boundary_sec: 35.42 - 23.0,
                    accepted: true,
                    method: "onnx_forced_suffix",
                    score: -6.2,
                    joint_calls: 1,
                })
            },
        );
        assert_eq!(out.transcript.text, "Check this out");
    }

    #[test]
    fn word_well_before_boundary_is_kept() {
        let tokens = [(" Keep", 0.0, 0.3), (" Zachary", 0.6, 1.2)];
        assert_eq!(
            snap_boundary_to_straddling_word(
                &tokens
                    .iter()
                    .map(|(text, start, end)| TimedToken {
                        text: (*text).into(),
                        start: *start,
                        end: *end
                    })
                    .collect::<Vec<_>>(),
                0.9,
            ),
            0.9
        );
    }

    #[test]
    fn misrecognized_command_keeps_preceding_speech() {
        let out = cleanup_confirmed_control(
            transcript(
                "Keep this Zephyr sand",
                &[
                    ("Keep", 0.0),
                    (" this", 0.4),
                    (" Zephyr", 1.0),
                    (" sand", 1.2),
                ],
            ),
            &vec![0.0; 32_000],
            ControlIntent::Send,
            16_000,
            |_, _, _| Err("forced alignment rejected command".into()),
        );
        assert_eq!(out.transcript.text, "Keep this");
        assert_eq!(out.method, "text_strip");
    }

    #[test]
    fn uncertain_command_falls_back_to_confirmed_text_strip_only() {
        let out = cleanup_confirmed_control(
            transcript(
                "Keep this Jefferson",
                &[("Keep", 0.0), (" this", 0.4), (" Jefferson", 1.0)],
            ),
            &vec![0.0; 32_000],
            ControlIntent::Send,
            16_000,
            |_, _, _| {
                Ok(Alignment {
                    boundary_sec: 0.96,
                    accepted: false,
                    method: "onnx_forced_suffix",
                    score: -8.0,
                    joint_calls: 3,
                })
            },
        );
        assert_eq!(out.transcript.text, "Keep this Jefferson");
        assert_eq!(out.method, "alignment_rejected");
    }
    #[test]
    fn guarded_whole_command_retains_ordinary_is_before_alias() {
        let full = transcript(
            "is Is that for send",
            &[
                ("is", 0.0),
                (" Is", 1.0),
                (" that", 1.2),
                (" for", 1.4),
                (" send", 1.6),
            ],
        );
        let out = cleanup_confirmed_control(
            full,
            &vec![0.0; 32000],
            ControlIntent::Send,
            16000,
            |_, _, _| {
                Ok(Alignment {
                    boundary_sec: 1.2,
                    accepted: true,
                    method: "full",
                    score: -4.0,
                    joint_calls: 1,
                })
            },
        );
        assert_eq!(out.transcript.text, "is");
    }
    #[test]
    fn invalid_onset_and_cancel_never_align() {
        for (onset, intent) in [
            (0, ControlIntent::Send),
            (32001, ControlIntent::Stop),
            (16000, ControlIntent::Cancel),
        ] {
            let out = cleanup_confirmed_control(
                transcript("Preserve speech", &[]),
                &vec![0.0; 32000],
                intent,
                onset,
                |_, _, _| panic!("must not align"),
            );
            assert_eq!(out.transcript.text, "Preserve speech");
        }
    }

    #[test]
    fn exact_retry_runs_once_only_when_fast_cut_removes_nothing() {
        let full = transcript(
            "Keep this Jefferson",
            &[("Keep", 0.0), (" this", 0.4), (" Jefferson", 1.0)],
        );
        let mut modes = Vec::new();
        let out = cleanup_confirmed_control_with_exact_retry(
            full,
            &vec![0.0; 32_000],
            ControlIntent::Send,
            16_000,
            |_, _, _, exact| {
                modes.push(exact);
                Ok(Alignment {
                    boundary_sec: if exact { 1.0 } else { 2.0 },
                    accepted: true,
                    method: if exact { "onnx_exact" } else { "onnx_fast" },
                    score: -4.0,
                    joint_calls: 1,
                })
            },
        );
        assert_eq!(modes, vec![false, true]);
        assert_eq!(out.transcript.text, "Keep this");
        assert_eq!(out.method, "onnx_exact");
    }

    #[test]
    fn exact_retry_preserves_first_result_when_exact_is_rejected_or_ineffective() {
        for exact_mode in 0..3 {
            let full = transcript(
                "Keep this Jefferson",
                &[("Keep", 0.0), (" this", 0.4), (" Jef", 1.0)],
            );
            let mut calls = 0;
            let out = cleanup_confirmed_control_with_exact_retry(
                full,
                &vec![0.0; 32_000],
                ControlIntent::Send,
                16_000,
                |_, _, _, exact| {
                    calls += 1;
                    if exact {
                        match exact_mode {
                            0 => Err("exact failed".to_string()),
                            1 => Ok(Alignment {
                                boundary_sec: 1.0,
                                accepted: false,
                                method: "onnx_exact",
                                score: -9.0,
                                joint_calls: 4,
                            }),
                            _ => Ok(Alignment {
                                boundary_sec: 1.0,
                                accepted: true,
                                method: "onnx_exact",
                                score: -9.0,
                                joint_calls: 4,
                            }),
                        }
                    } else {
                        Ok(Alignment {
                            boundary_sec: 2.0,
                            accepted: true,
                            method: "onnx_fast",
                            score: -4.0,
                            joint_calls: 1,
                        })
                    }
                },
            );
            assert_eq!(calls, 2);
            assert_eq!(out.transcript.text, "Keep this Jefferson");
            assert_eq!(out.method, "onnx_fast");
            assert_eq!(out.error.is_some(), exact_mode == 0);
        }
    }

    #[test]
    fn effective_fast_cut_never_retries_exact() {
        let full = transcript(
            "Keep this Jefferson",
            &[("Keep", 0.0), (" this", 0.4), (" Jefferson", 1.0)],
        );
        let mut calls = 0;
        let out = cleanup_confirmed_control_with_exact_retry(
            full,
            &vec![0.0; 32_000],
            ControlIntent::Send,
            16_000,
            |_, _, _, exact| {
                calls += 1;
                assert!(!exact);
                Ok(Alignment {
                    boundary_sec: 1.0,
                    accepted: true,
                    method: "onnx_fast",
                    score: -4.0,
                    joint_calls: 1,
                })
            },
        );
        assert_eq!(calls, 1);
        assert_eq!(out.transcript.text, "Keep this");
    }

    #[test]
    fn overlapping_fast_word_retries_exact_and_removes_command_residue() {
        let full = transcript(
            "Keep Is that for a sound",
            &[
                ("Keep", 0.0),
                (" Is", 0.90),
                (" that", 1.05),
                (" for", 1.20),
                (" a", 1.35),
                (" sound", 1.50),
            ],
        );
        let mut modes = Vec::new();
        let out = cleanup_confirmed_control_with_exact_retry(
            full,
            &vec![0.0; 32_000],
            ControlIntent::Send,
            16_000,
            |_, _, _, exact| {
                modes.push(exact);
                Ok(Alignment {
                    boundary_sec: if exact { 0.80 } else { 0.95 },
                    accepted: true,
                    method: if exact { "onnx_exact" } else { "onnx_fast" },
                    score: -4.0,
                    joint_calls: 1,
                })
            },
        );
        assert_eq!(modes, vec![false, true]);
        assert_eq!(out.transcript.text, "Keep");
        assert_eq!(out.method, "onnx_exact");
    }

    #[test]
    fn exact_retry_does_not_remove_preceding_does_when_boundary_is_after_it() {
        let full = transcript(
            "Does Is that for send",
            &[
                ("Does", 0.0),
                (" Is", 1.0),
                (" that", 1.2),
                (" for", 1.4),
                (" send", 1.6),
            ],
        );
        let mut exact_calls = 0;
        let out = cleanup_confirmed_control_with_exact_retry(
            full,
            &vec![0.0; 32_000],
            ControlIntent::Send,
            16_000,
            |_, _, _, exact| {
                if exact {
                    exact_calls += 1;
                }
                Ok(Alignment {
                    boundary_sec: 1.2,
                    accepted: true,
                    method: if exact { "onnx_exact" } else { "onnx_fast" },
                    score: -4.0,
                    joint_calls: 1,
                })
            },
        );
        assert_eq!(exact_calls, 0);
        assert_eq!(out.transcript.text, "Does");
    }

    #[test]
    fn real_session18_gap_retries_exact_and_keeps_earlier_sense() {
        let full = TranscriptionResult {
            text: "sense Does that for send".into(),
            tokens: vec![
                TimedToken {
                    text: "sense".into(),
                    start: 12.0,
                    end: 12.3,
                },
                TimedToken {
                    text: " Do".into(),
                    start: 12.64,
                    end: 12.72,
                },
                TimedToken {
                    text: "es".into(),
                    start: 12.72,
                    end: 12.80,
                },
                TimedToken {
                    text: " that".into(),
                    start: 12.96,
                    end: 13.08,
                },
                TimedToken {
                    text: " for".into(),
                    start: 13.10,
                    end: 13.20,
                },
                TimedToken {
                    text: " send".into(),
                    start: 13.22,
                    end: 13.34,
                },
            ],
        };
        let mut modes = Vec::new();
        let out = cleanup_confirmed_control_with_exact_retry(
            full,
            &vec![0.0; 224_000],
            ControlIntent::Send,
            206_080,
            |_, _, _, exact| {
                modes.push(exact);
                Ok(Alignment {
                    boundary_sec: if exact { 12.48 } else { 12.88 },
                    accepted: true,
                    method: if exact { "onnx_exact" } else { "onnx_fast" },
                    score: if exact { -5.871 } else { -8.061 },
                    joint_calls: 1,
                })
            },
        );
        assert_eq!(modes, vec![false, true]);
        assert_eq!(out.transcript.text, "sense");
        assert_eq!(out.method, "onnx_exact");
    }

    #[test]
    fn nearby_partial_word_is_retained_when_exact_cannot_verify_command() {
        for accepted in [false, true] {
            let full = transcript(
                "Keep Does that for send",
                &[
                    ("Keep", 0.0),
                    (" Does", 0.8),
                    (" that", 1.04),
                    (" for", 1.2),
                    (" send", 1.4),
                ],
            );
            let mut calls = 0;
            let out = cleanup_confirmed_control_with_exact_retry(
                full,
                &vec![0.0; 32_000],
                ControlIntent::Send,
                16_000,
                |_, _, _, exact| {
                    calls += 1;
                    Ok(Alignment {
                        boundary_sec: if exact { f32::NAN } else { 1.0 },
                        accepted: !exact || accepted,
                        method: if exact { "onnx_exact" } else { "onnx_fast" },
                        score: -5.0,
                        joint_calls: 1,
                    })
                },
            );
            assert_eq!(calls, 2);
            assert_eq!(out.transcript.text, "Keep Does");
            assert_eq!(out.method, "onnx_fast");
        }
    }
}

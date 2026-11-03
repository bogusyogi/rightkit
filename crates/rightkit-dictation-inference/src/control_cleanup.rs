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

//! The deterministic L0 local-polish pipeline plus the continuation-casing
//! helpers shared by L0 and the post-L1 reconciliation in
//! `crate::text_polish::polish_dictation`.

use std::time::Instant;

/// Post-L1 continuation reconciliation: the local pass sent the model a
/// lowercase first word (continuation); if the model returned the same first
/// word merely recapitalized, downcase it back and re-run vocabulary restore
/// so saved terms (e.g. "the app") keep their exact casing. If the model
/// changed the first WORD itself (restructure, proper noun it knows), trust it.
pub(crate) fn reconcile_continuation_casing(cleaned: String, local: &str) -> String {
    let first_word = |s: &str| -> String {
        s.chars()
            .take_while(|c| !c.is_whitespace())
            .collect::<String>()
    };
    let local_first = first_word(local);
    let cleaned_first = first_word(&cleaned);
    let local_starts_lower = local_first.chars().next().is_some_and(|c| c.is_lowercase());
    let same_word_recapitalized = local_starts_lower
        && cleaned_first
            .chars()
            .next()
            .is_some_and(|c| c.is_uppercase())
        && cleaned_first.to_lowercase() == local_first.to_lowercase();
    if !same_word_recapitalized {
        return cleaned;
    }
    let fixed = lowercase_continuation_start(cleaned);
    rightkit_text_polish::text_pipeline::restore_vocabulary_casing(
        &fixed,
        &crate::vocabulary::terms(),
    )
}

/// Lowercase the first letter of a continuation, exempting the pronoun "I"
/// and its contractions ("I'm", "I've", "I'll", "I'd", "I're" is not a word
/// but the regex family is cheap). Proper nouns are restored downstream by
/// vocabulary restore and the L1 CONTINUATION CASING clause.
fn lowercase_continuation_start(mut text: String) -> String {
    let Some(first) = text.chars().next() else {
        return text;
    };
    if !first.is_uppercase() {
        return text;
    }
    // Exempt the pronoun-I family: "I", "I'm", "I've", "I'll", "I'd".
    if first == 'I' {
        let rest: String = text.chars().skip(1).take(3).collect();
        let is_pronoun = rest.is_empty()
            || rest.starts_with(' ')
            || rest.starts_with('\'')
            || rest.starts_with(',')
            || rest.starts_with('.');
        if is_pronoun {
            return text;
        }
    }
    let lowered = first.to_lowercase().to_string();
    text.replace_range(0..first.len_utf8(), &lowered);
    text
}

pub(crate) fn polish_local(input: &str) -> String {
    polish_local_with(input, true)
}

pub(crate) fn polish_local_with(input: &str, capitalize_start: bool) -> String {
    let total_started = Instant::now();
    let deterministic_started = Instant::now();
    // `deterministic_polish_tail` is the same pass with first-letter
    // capitalization off — used when the dictation continues a sentence the
    // user already typed in the field (see `continues_mid_sentence`).
    let deterministic = if capitalize_start {
        rightkit_text_polish::text_pipeline::deterministic_polish_with_brand(
            input,
            super::brand_homophones(),
        )
    } else {
        // ASR itself emits sentence-cased text ("Continuing a sentence..."),
        // so merely REFRAINING from capitalizing is not enough for a
        // continuation — actively lowercase the first letter.
        // The pronoun "I" family is exempt; saved vocabulary and the L1
        // model's proper-noun clause restore genuine proper nouns after.
        lowercase_continuation_start(
            rightkit_text_polish::text_pipeline::deterministic_polish_tail_with_brand(
                input,
                super::brand_homophones(),
            ),
        )
    };
    let deterministic_ms = deterministic_started.elapsed().as_millis() as u64;
    let cleanup_started = Instant::now();
    let mut polished = polish_local_after_deterministic(&deterministic);
    // In "aggressive" mode (the default), `polish_aggressive` -> `capitalize_first_alpha`
    // capitalizes the first letter unconditionally, undoing the continuation casing
    // chosen above. Restore the lowercase first letter BEFORE vocabulary restore, so
    // proper-noun terms still get their casing.
    if !capitalize_start {
        let restore = match (deterministic.chars().next(), polished.chars().next()) {
            (Some(d), Some(p)) => {
                d.is_lowercase() && p.is_uppercase() && p.to_lowercase().next() == Some(d)
            }
            _ => false,
        };
        if restore {
            let first = polished.chars().next().expect("checked above");
            polished.replace_range(0..first.len_utf8(), &first.to_lowercase().to_string());
        }
    }
    let cleanup_ms = cleanup_started.elapsed().as_millis() as u64;
    // In "aggressive" mode, `aggressive_speech_cleanup` -> `fix_punctuation_spacing`
    // inserts a space after any punctuation immediately followed by a letter, which
    // re-splits "example.com" into "example. com", undoing the deterministic domain
    // fix. Domain spacing is a fixed rule (L0), so re-tighten it as the final step.
    let domain_started = Instant::now();
    let tightened = rightkit_text_polish::text_pipeline::tighten_domain_spacing(&polished);
    let domain_ms = domain_started.elapsed().as_millis() as u64;
    // Saved vocabulary belongs at L0/L1: restore each term's exact spelling/casing that
    // upstream local-cleanup passes may have altered (local only — never sent to L2/L3).
    let vocabulary_started = Instant::now();
    let restored = rightkit_text_polish::text_pipeline::restore_vocabulary_casing(
        &tightened,
        &crate::vocabulary::terms(),
    );
    let vocabulary_ms = vocabulary_started.elapsed().as_millis() as u64;
    // User replacements last — the deterministic "always fix X to Y" layer wins
    // over everything upstream (local only, part of the raw/undo-AI text).
    let replacements = crate::settings::replacements();
    let replacements_started = Instant::now();
    let output = if replacements.is_empty() {
        restored
    } else {
        rightkit_text_polish::text_pipeline::apply_replacements(&restored, &replacements)
    };
    trace_local_polish_timing(
        deterministic_ms,
        cleanup_ms,
        domain_ms,
        vocabulary_ms,
        replacements_started.elapsed().as_millis() as u64,
        total_started,
    );
    // L0's final local result gets only visual readability chunks. L1/L2/L3
    // still receive their own source and own semantic formatting decisions.
    rightkit_text_polish::text_pipeline::visual_chunk_transcript(&output)
}

fn polish_local_after_deterministic(input: &str) -> String {
    match crate::settings::transcript_cleanup().as_str() {
        "clean" => polish_clean(input),
        // "aggressive" is the default.
        _ => polish_aggressive(input),
    }
}

// `clean` = the deterministic transcript normalizer, no spell-correction step.
// ASR (Parakeet) emits real dictionary words; its errors are word-choice
// mis-recognitions ("form" for "from"), which a spellchecker can't fix and
// would only risk corrupting valid rare/technical words (mic->mac). Contextual
// correction is the opt-in L1 app-aware AI layer's job; L0 deterministic handles the rest
// (dispatch #6 — removed harper-core + its burn/cubecl ML tree entirely).
pub(crate) fn polish_clean(input: &str) -> String {
    input.to_string()
}

// `aggressive` = extra speech-clutter removal wrapped around Clean.
pub(crate) fn polish_aggressive(input: &str) -> String {
    capitalize_first_alpha(&polish_aggressive_uncapitalized(input))
}

fn polish_aggressive_uncapitalized(input: &str) -> String {
    rightkit_text_polish::text_pipeline::aggressive_speech_cleanup(input)
}

fn capitalize_first_alpha(input: &str) -> String {
    let Some((idx, first)) = input.char_indices().find(|(_, ch)| ch.is_alphabetic()) else {
        return input.to_string();
    };
    let upper = first.to_uppercase().to_string();
    if upper == first.to_string() {
        return input.to_string();
    }
    let mut out = input.to_string();
    out.replace_range(idx..idx + first.len_utf8(), &upper);
    out
}

fn trace_local_polish_timing(
    deterministic_ms: u64,
    cleanup_ms: u64,
    domain_ms: u64,
    vocabulary_ms: u64,
    replacements_ms: u64,
    total_started: Instant,
) {
    tracing::info!(
        deterministic_ms,
        local_cleanup_ms = cleanup_ms,
        domain_spacing_ms = domain_ms,
        vocabulary_restore_ms = vocabulary_ms,
        replacements_ms,
        total_local_ms = total_started.elapsed().as_millis() as u64,
        "l0_local_polish_timing"
    );
}

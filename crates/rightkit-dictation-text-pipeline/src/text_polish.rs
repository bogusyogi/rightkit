//! Dictation polish: the public L0 entry points plus the L1 app-aware
//! orchestration. The deterministic L0 pipeline itself lives in
//! `text_polish/local.rs`; unit tests live in `text_polish/bench.rs`.

use std::time::Instant;

mod local;

use rightkit_text_polish::text_pipeline::BrandHomophones;

static BRAND_HOMOPHONES: std::sync::OnceLock<BrandHomophones<'static>> = std::sync::OnceLock::new();

/// Install the host's brand homophone table for local (L0) polish. Call once at startup; without
/// it L0 polish applies no brand repair. Returns false when a table was already installed.
pub fn configure_brand_homophones(rules: BrandHomophones<'static>) -> bool {
    BRAND_HOMOPHONES.set(rules).is_ok()
}

pub(crate) fn brand_homophones() -> Option<&'static BrandHomophones<'static>> {
    BRAND_HOMOPHONES.get()
}

#[cfg(test)]
mod bench;

pub(crate) use local::*;

/// Warm the deterministic polish path (one throwaway pass) so the FIRST real
/// dictation doesn't pay any cold cost. Called from the worker warmup alongside
/// the model warmup — off the critical path. (No spellchecker to load: ASR
/// output is real words, not typos — the spell-correction engine was removed
/// entirely, see `polish_clean` below.)
pub fn warm() {
    let _ = polish_local("Warm up one two three.");
    let _ = polish_local(
        "Please send support at sign example dot com the project invoice \
         for four thousand two hundred and fifty dollars tomorrow morning.",
    );
}

pub fn polish(input: &str) -> String {
    polish_local(input)
}

pub fn polish_local_only(input: &str) -> String {
    polish_local(input)
}

/// Local polish with explicit first-letter casing control. `capitalize_start:
/// false` = the dictation continues an unfinished sentence already in the
/// focused field, so the first word must stay lowercase (field text is
/// preserved as-is; we adapt to it, never the reverse).
pub fn polish_local_only_with(input: &str, capitalize_start: bool) -> String {
    polish_local_with(input, capitalize_start)
}

/// Empty-field placeholder strings exposed as editable text by AX/UIA. They
/// are UI chrome, not prior user text, so must never trigger continuation.
pub(crate) fn is_field_placeholder(text: &str) -> bool {
    const PLACEHOLDERS: [&str; 9] = [
        "type / for commands",
        "ask anything",
        "do anything",
        "send a message",
        "type a message",
        "write a message",
        "message",
        "reply",
        "search",
    ];
    let normalized = text
        .trim()
        .trim_end_matches(['…', '.'])
        .trim()
        .to_ascii_lowercase();
    PLACEHOLDERS
        .iter()
        .any(|placeholder| normalized == *placeholder)
}

/// True when the focused field already holds text whose tail is an unfinished
/// sentence — the insertion point is assumed at the end of the field (the
/// overwhelmingly common dictate-after-typing case). Trailing quotes/brackets
/// are skipped so `He said "stop."` still ends the sentence.
pub fn continues_mid_sentence(field_text: Option<&str>) -> bool {
    let Some(text) = field_text else {
        return false;
    };
    if is_field_placeholder(text) {
        return false;
    }
    let trimmed = text.trim_end();
    if trimmed.is_empty() {
        return false;
    }
    let last = trimmed
        .chars()
        .rev()
        .find(|c| !matches!(c, '"' | '\'' | ')' | ']' | '}' | '\u{201D}' | '\u{2019}'));
    !matches!(last, None | Some('.' | '!' | '?' | ':' | '\n'))
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DictationPolishContext<'a> {
    pub audio_secs: Option<f32>,
    pub app_name: Option<&'a str>,
    pub window_title: Option<&'a str>,
    /// Focused-field text captured at record start (macOS AX) — on-device
    /// context that grounds app-aware polish; sanitized before leaving device.
    pub field_text: Option<&'a str>,
    pub selected_text: Option<&'a str>,
    pub field_context_available: bool,
    pub writing_region: Option<&'a str>,
}

pub fn polish_dictation(input: &str, context: DictationPolishContext<'_>) -> String {
    polish_dictation_with_receipt(input, context, None)
}

pub fn polish_dictation_with_receipt(
    input: &str,
    context: DictationPolishContext<'_>,
    request_attempted: Option<&std::sync::atomic::AtomicBool>,
) -> String {
    polish_dictation_with_receipts(input, context, request_attempted, None)
}

pub fn polish_dictation_with_receipts(
    input: &str,
    context: DictationPolishContext<'_>,
    request_attempted: Option<&std::sync::atomic::AtomicBool>,
    original_binding: Option<&std::sync::atomic::AtomicBool>,
) -> String {
    let total_started = Instant::now();
    let local_started = Instant::now();
    // Preserve the user's typed field text as-is: if it ends mid-sentence, the
    // dictation is a continuation and must not be force-capitalized.
    let continuation = continues_mid_sentence(context.field_text);
    // L0 is computed locally as fallback only. L1 receives raw ASR so its
    // semantic correction decision is not constrained by deterministic edits.
    let local = polish_local_only_with(input, !continuation);
    let local_elapsed = local_started.elapsed();
    let local_ms = local_elapsed.as_millis() as u64;
    if !should_try_l1_app_polish(input, context) {
        trace_polish_timing(
            local_ms,
            local_elapsed.as_secs_f64() * 1_000.0,
            None,
            false,
            "local_only",
            total_started,
        );
        return local;
    }
    let l1_started = Instant::now();
    match l1_app_polish_outcome(input, &context, request_attempted, original_binding) {
        crate::l3_cleanup::CleanupOutcome::Cleaned(cleaned) => {
            // The model can recapitalize a continuation's
            // first word despite the prompt rule (lowercase
            // in, capitalized out). Reconcile deterministically instead of
            // trusting compliance: if this is a continuation and the model
            // returned the SAME first word merely capitalized, downcase it,
            // then re-run vocabulary restore so saved proper-noun terms keep
            // their exact casing.
            let cleaned = if continuation {
                reconcile_continuation_casing(cleaned, &local)
            } else {
                cleaned
            };
            trace_polish_timing(
                local_ms,
                local_elapsed.as_secs_f64() * 1_000.0,
                Some(l1_started.elapsed().as_millis() as u64),
                true,
                "l1_cleaned",
                total_started,
            );
            return cleaned;
        }
        crate::l3_cleanup::CleanupOutcome::Failed {
            error_class,
            circuit_open,
        } => {
            crate::l3_cleanup::record_local_fallback();
            tracing::warn!(
                error_class,
                circuit_open,
                "l1_app_polish_fallback_to_local_polish"
            );
            trace_polish_timing(
                local_ms,
                local_elapsed.as_secs_f64() * 1_000.0,
                Some(l1_started.elapsed().as_millis() as u64),
                false,
                "l1_failed",
                total_started,
            );
        }
        crate::l3_cleanup::CleanupOutcome::Skipped { .. } => {
            trace_polish_timing(
                local_ms,
                local_elapsed.as_secs_f64() * 1_000.0,
                Some(l1_started.elapsed().as_millis() as u64),
                false,
                "l1_skipped",
                total_started,
            );
        }
    }
    local
}

fn l1_app_polish_outcome(
    input: &str,
    context: &DictationPolishContext<'_>,
    request_attempted: Option<&std::sync::atomic::AtomicBool>,
    original_binding: Option<&std::sync::atomic::AtomicBool>,
) -> crate::l3_cleanup::CleanupOutcome {
    let context = crate::l3_cleanup::PolishContext {
        app_name: context.app_name.map(str::to_string),
        window_title: context.window_title.map(str::to_string),
        field_text: context.field_text.map(str::to_string),
        selected_text: context.selected_text.map(str::to_string),
        field_context_available: context.field_context_available,
        vocabulary: crate::vocabulary::semantic_terms(),
        writing_region: context.writing_region.map(str::to_string),
        sound_alikes: crate::vocabulary::sound_alike_pairs(),
    };
    crate::l3_cleanup::app_polish_outcome_with_receipts(
        input,
        &context,
        request_attempted,
        original_binding,
    )
}

fn trace_polish_timing(
    local_ms: u64,
    local_exact_ms: f64,
    l1_ms: Option<u64>,
    ai_used: bool,
    outcome: &'static str,
    total_started: Instant,
) {
    tracing::info!(
        local_polish_ms = local_ms,
        local_polish_exact_ms = local_exact_ms,
        l1_app_polish_ms = l1_ms,
        ai_used,
        outcome,
        total_polish_ms = total_started.elapsed().as_millis() as u64,
        "final_transcript_polish_timing"
    );
}

fn should_try_l1_app_polish(input: &str, context: DictationPolishContext) -> bool {
    let text = input.trim();
    if text.split_whitespace().count() < l3_min_words() {
        return false;
    }
    if context
        .audio_secs
        .is_some_and(|secs| secs < l3_min_audio_secs())
    {
        return false;
    }
    if rightkit_voice_commands::command_recognition::recognize_command(text).is_some()
        || rightkit_voice_commands::command_recognition::app_launch_query(text).is_some()
    {
        return false;
    }
    true
}

fn l3_min_audio_secs() -> f32 {
    rightkit_dictation_core::dev_var!("RIGHTKIT_L3_MIN_AUDIO_SECS")
        .ok()
        .and_then(|value| value.parse::<f32>().ok())
        .unwrap_or(3.0)
        .clamp(0.0, 30.0)
}

fn l3_min_words() -> usize {
    rightkit_dictation_core::dev_var!("RIGHTKIT_L3_MIN_WORDS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(8)
        .clamp(1, 200)
}

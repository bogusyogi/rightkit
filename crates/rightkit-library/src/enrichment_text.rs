pub(super) fn split_sentences(text: &str) -> Vec<String> {
    let mut sentences = Vec::new();
    let mut current = String::new();

    for ch in text.chars() {
        current.push(ch);
        if matches!(ch, '.' | '!' | '?') {
            let trimmed = current.trim().to_string();
            if !trimmed.is_empty() {
                sentences.push(trimmed);
            }
            current.clear();
        }
    }
    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() {
        sentences.push(trimmed);
    }

    // Also split on double newlines for transcript/carousel content
    let mut expanded = Vec::new();
    for s in &sentences {
        let parts: Vec<&str> = s.split("\n\n").collect();
        for part in parts {
            let trimmed = part.trim().to_string();
            if !trimmed.is_empty() {
                expanded.push(trimmed);
            }
        }
    }
    if expanded.is_empty() {
        // Fallback: split entire text on double newlines
        for part in text.split("\n\n") {
            let trimmed = part.trim().to_string();
            if !trimmed.is_empty() {
                expanded.push(trimmed);
            }
        }
    }

    expanded
}

/// Find the sentence containing a given substring.
pub(super) fn find_containing_sentence(
    original_text: &str,
    needle: &str,
    sentences: &[String],
) -> String {
    if let Some(s) = sentences.iter().find(|s| s.contains(needle)) {
        return s.clone();
    }
    // Fallback: return a window around the needle in the original text
    if let Some(pos) = original_text.find(needle) {
        let start = pos.saturating_sub(80);
        let end = (pos + needle.len() + 80).min(original_text.len());
        return original_text[start..end].to_string();
    }
    String::new()
}

// ---------------------------------------------------------------------------
// Regex helpers — use the `regex` crate if available, else simple fallbacks
// ---------------------------------------------------------------------------

use regex;

/// Compile-and-cache a regex by pattern. enrich() runs the same handful of
/// patterns on every job (40-100 reels per batch); compiling each one per call
/// was thousands of NFA builds per run. `regex::Regex` is Arc-backed, so the
/// cached clone is cheap.
pub(super) fn cached_regex(pattern: &str) -> Option<regex::Regex> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<String, regex::Regex>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = cache.lock().ok()?;
    if let Some(re) = map.get(pattern) {
        return Some(re.clone());
    }
    let re = regex::Regex::new(pattern).ok()?;
    map.insert(pattern.to_string(), re.clone());
    Some(re)
}

pub(super) fn regex_captures_iter<'a>(text: &'a str, pattern: &str) -> Vec<regex::Captures<'a>> {
    match cached_regex(pattern) {
        Some(re) => re.captures_iter(text).collect(),
        None => Vec::new(),
    }
}

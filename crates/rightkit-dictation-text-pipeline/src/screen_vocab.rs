//! Screen-context auto-vocabulary — AX-only harvest of proper nouns and
//! technical tokens from the frontmost window, used to bias the local ASR
//! decode toward the terms the user is looking at.
//!
//! Spec: docs/plans/screen_context_vocabulary_2026-07-19.md.
//! AX-only by design — no screenshots, no OCR, ever.
//! v1 extraction is the pure-Rust technical-token pass (capitalized words,
//! camelCase/snake_case identifiers, @handles, emails, hostnames); the
//! NLTagger NameType refinement is deferred — capitalization already covers
//! person/place/org names in practice, and keeping extraction in Rust keeps
//! it unit-testable.
//!
//! Privacy: harvested terms live in ONE process-global slot, are overwritten
//! on every recording start, and are never logged (term COUNT only), never
//! persisted, never sent anywhere. The bias mechanism they feed is fully
//! local decode-time biasing.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Cap on terms fed to the decoder per utterance — matches the spec (~50) and
/// stays far below the decoder's 512-term ceiling so user vocabulary always
/// fits alongside.
const MAX_TERMS: usize = 50;
/// AX walk character budget (spec: 8–12K).
// Consumed only by the macOS window-text harvest below.
#[cfg(target_os = "macos")]
const HARVEST_BUDGET_CHARS: usize = 12_000;
/// A harvest older than this is stale — the user has almost certainly changed
/// context. `take_terms` returns nothing rather than biasing toward a window
/// the user left.
const FRESH_FOR: Duration = Duration::from_secs(120);

struct Slot {
    terms: Vec<String>,
    at: Instant,
}

static SLOT: Mutex<Option<Slot>> = Mutex::new(None);

/// Kick off the harvest on a detached thread at recording start. Never blocks
/// the caller; decode reads whatever is ready when the utterance ends (the
/// walk finishes in well under a second — an utterance lasts seconds).
pub fn harvest_async() {
    #[cfg(target_os = "macos")]
    {
        let spawned = std::thread::Builder::new()
            .name("hr-screen-vocab".to_string())
            .spawn(|| {
                let started = Instant::now();
                let texts =
                    rightkit_automation::desktop::macos::window_text_harvest(HARVEST_BUDGET_CHARS);
                let terms = extract_terms(&texts);
                let count = terms.len();
                *SLOT.lock().unwrap_or_else(|e| e.into_inner()) = Some(Slot {
                    terms,
                    at: Instant::now(),
                });
                // Count and timing only — never the terms themselves.
                tracing::info!(
                    terms = count,
                    elements = texts.len(),
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "screen_vocab_harvest"
                );
            });
        if spawned.is_err() {
            tracing::warn!("screen_vocab_thread_spawn_failed");
        }
    }
}

/// Current fresh harvest, or empty. Leaves the slot in place: several decode
/// entry points (main utterance, probe) may read the same recording's harvest.
pub fn current_terms() -> Vec<String> {
    let guard = SLOT.lock().unwrap_or_else(|e| e.into_inner());
    match guard.as_ref() {
        Some(slot) if slot.at.elapsed() <= FRESH_FOR => slot.terms.clone(),
        _ => Vec::new(),
    }
}

/// Tokens that read as sentence furniture, not vocabulary — capitalized purely
/// because of position or convention. Keep this list short and obvious; the
/// mid-sentence-capital rule below does the heavy lifting.
const STOPLIST: &[&str] = &[
    "The",
    "This",
    "That",
    "These",
    "Those",
    "There",
    "Then",
    "They",
    "Them",
    "A",
    "An",
    "And",
    "But",
    "Or",
    "So",
    "If",
    "In",
    "On",
    "At",
    "To",
    "For",
    "Of",
    "With",
    "From",
    "By",
    "As",
    "Is",
    "Are",
    "Was",
    "Were",
    "Be",
    "Been",
    "I",
    "It",
    "Its",
    "We",
    "You",
    "He",
    "She",
    "My",
    "Your",
    "Our",
    "His",
    "Her",
    "Not",
    "No",
    "Yes",
    "OK",
    "Okay",
    "Hi",
    "Hello",
    "Hey",
    "Thanks",
    "Thank",
    "Please",
    "New",
    "All",
    "Any",
    "Some",
    "More",
    "Most",
    "Other",
    "What",
    "When",
    "Where",
    "Which",
    "Who",
    "Why",
    "How",
    "Can",
    "Will",
    "Would",
    "Could",
    "Should",
    "May",
    "Might",
    "Do",
    "Does",
    "Did",
    "Done",
    "Today",
    "Tomorrow",
    "Yesterday",
    "Now",
    "Here",
    "Just",
    "Also",
    "Very",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
    "January",
    "February",
    "March",
    "April",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
    "Send",
    "Reply",
    "Edit",
    "Copy",
    "Paste",
    "Delete",
    "Cancel",
    "Close",
    "Open",
    "Save",
    "Search",
    "File",
    "View",
    "Window",
    "Help",
    "Back",
    "Next",
    "Continue",
    "Settings",
    "About",
    "Home",
];

fn stoplisted(token: &str) -> bool {
    STOPLIST.iter().any(|s| s.eq_ignore_ascii_case(token))
}

/// True for tokens that are interesting REGARDLESS of position: identifiers,
/// handles, emails, hostnames.
fn technical_token(token: &str) -> bool {
    if token.starts_with('@') && token.len() > 2 {
        return true;
    }
    if token.contains('@') && token.contains('.') {
        return true; // email-shaped
    }
    if token.contains('_') && token.chars().any(|c| c.is_alphabetic()) {
        return true; // snake_case
    }
    // hostname/domain-shaped: letters + dot + letters, no spaces
    if token.matches('.').count() >= 1
        && !token.starts_with('.')
        && !token.ends_with('.')
        && token
            .chars()
            .all(|c| c.is_alphanumeric() || c == '.' || c == '-')
        && token.chars().any(|c| c.is_alphabetic())
        && token.len() >= 5
    {
        return true;
    }
    // camelCase / mixed-case identifier: lowercase followed by uppercase
    let mut prev_lower = false;
    for c in token.chars() {
        if c.is_uppercase() && prev_lower {
            return true;
        }
        prev_lower = c.is_lowercase();
    }
    false
}

fn strip_edge_punct(token: &str) -> &str {
    token.trim_matches(|c: char| !(c.is_alphanumeric() || c == '@' || c == '_'))
}

/// Extract candidate vocabulary from harvested strings. Order of `texts` is
/// the proximity order the AX walk produced, so earlier occurrences carry
/// implicit priority; scoring = first-seen order refined by frequency.
pub fn extract_terms(texts: &[String]) -> Vec<String> {
    use std::collections::HashMap;
    // (first_index, count) per case-preserved term
    let mut stats: HashMap<String, (usize, usize)> = HashMap::new();
    let mut order = 0usize;

    for text in texts {
        let mut prev_ended_sentence = true;
        for raw in text.split_whitespace() {
            let token = strip_edge_punct(raw);
            if token.len() < 3 || token.len() > 40 || token.chars().all(|c| c.is_numeric()) {
                prev_ended_sentence = raw.ends_with(['.', '!', '?', ':', '\n']);
                continue;
            }
            let is_capitalized = token.chars().next().is_some_and(|c| c.is_uppercase())
                && token.chars().skip(1).any(|c| c.is_lowercase());
            // Capitalized is only a signal MID-sentence — sentence-initial
            // capitals are grammar, not names.
            let interesting = technical_token(token)
                || (is_capitalized && !prev_ended_sentence && !stoplisted(token));
            if interesting {
                let entry = stats.entry(token.to_string()).or_insert_with(|| {
                    order += 1;
                    (order, 0)
                });
                entry.1 += 1;
            }
            prev_ended_sentence = raw.ends_with(['.', '!', '?', ':', '\n']);
        }
    }

    let mut terms: Vec<(String, (usize, usize))> = stats.into_iter().collect();
    // Frequency first (a term all over the window matters), proximity order as
    // the tiebreak (stable: earlier in the walk = closer to the caret).
    terms.sort_by(|a, b| b.1 .1.cmp(&a.1 .1).then(a.1 .0.cmp(&b.1 .0)));
    terms.truncate(MAX_TERMS);
    terms.into_iter().map(|(t, _)| t).collect()
}

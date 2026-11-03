use std::sync::{Mutex, OnceLock};

/// Sound-alike alias pair `(term, sounds_like)`. Empty alias lists stay in
/// the mirror even after a subsequent replace so the LLM prompt block
/// reflects whatever the shell most recently synced.
pub type SoundAlikePair = (String, Vec<String>);

static VOCABULARY_TERMS: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
static VOCABULARY_SOUND_ALIKES: OnceLock<Mutex<Vec<SoundAlikePair>>> = OnceLock::new();

pub fn replace_terms(terms: Vec<String>) {
    let cell = VOCABULARY_TERMS.get_or_init(|| Mutex::new(Vec::new()));
    if let Ok(mut slot) = cell.lock() {
        *slot = terms;
    }
    // A bare-terms replace clears the alias mirror so callers cannot see
    // orphaned aliases that no longer map to a live term.
    if let Some(cell) = VOCABULARY_SOUND_ALIKES.get() {
        if let Ok(mut slot) = cell.lock() {
            slot.clear();
        }
    }
}

pub fn replace_terms_with_aliases(terms: Vec<String>, sound_alikes: Vec<SoundAlikePair>) {
    let cell = VOCABULARY_TERMS.get_or_init(|| Mutex::new(Vec::new()));
    if let Ok(mut slot) = cell.lock() {
        *slot = terms;
    }
    let aliases_cell = VOCABULARY_SOUND_ALIKES.get_or_init(|| Mutex::new(Vec::new()));
    if let Ok(mut slot) = aliases_cell.lock() {
        *slot = sound_alikes;
    }
}

/// The host's built-in product vocabulary: brand and wake-word terms both the ASR context-bias
/// layer and the semantic (LLM) polish layer should know without user-saved vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProductTerms {
    /// Every built-in term, including wake-command variants (ASR context bias).
    pub built_in: &'static [&'static str],
    /// Product name terms only, for semantic polish.
    pub core: &'static [&'static str],
}

static PRODUCT_TERMS: std::sync::OnceLock<ProductTerms> = std::sync::OnceLock::new();

const UNCONFIGURED: ProductTerms = ProductTerms {
    built_in: &[],
    core: &[],
};

/// Install the host's product vocabulary once at startup. Returns false when one was already set.
pub fn configure_product_terms(terms: ProductTerms) -> bool {
    PRODUCT_TERMS.set(terms).is_ok()
}

fn product_terms() -> &'static ProductTerms {
    PRODUCT_TERMS.get().unwrap_or(&UNCONFIGURED)
}

/// Canonical built-in product vocabulary (empty until [`configure_product_terms`]).
pub fn built_in_terms() -> &'static [&'static str] {
    product_terms().built_in
}

/// Core product name terms only (no wake-command variants), for surfaces
/// like semantic polish that should not see ASR-only command phrases.
pub fn core_product_terms() -> &'static [&'static str] {
    product_terms().core
}

/// Merge core product terms with user vocabulary, preserving canonical
/// product casing when a user entry differs only by case.
pub fn semantic_terms() -> Vec<String> {
    merge_with_core_terms(terms())
}

fn merge_with_core_terms(user_terms: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut merged = Vec::new();
    for term in core_product_terms()
        .iter()
        .map(|term| (*term).to_string())
        .chain(user_terms)
    {
        if seen.insert(term.to_lowercase()) {
            merged.push(term);
        }
    }
    merged
}

pub fn terms() -> Vec<String> {
    VOCABULARY_TERMS
        .get()
        .and_then(|cell| cell.lock().ok().map(|slot| slot.clone()))
        .unwrap_or_default()
}

pub fn sound_alike_pairs() -> Vec<SoundAlikePair> {
    VOCABULARY_SOUND_ALIKES
        .get()
        .and_then(|cell| cell.lock().ok().map(|slot| slot.clone()))
        .unwrap_or_default()
}

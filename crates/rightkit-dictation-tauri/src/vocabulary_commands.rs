//! The bodies of the vocabulary commands: validate the renderer's input, mutate the app's
//! vocabulary store, then let the engine observe the change.
//!
//! The store is the application's (its file location and write function are configuration), so it
//! arrives as a `VocabularyCommandStore`. The limits are the application's `Limits`. The Tauri
//! command entry points, and the move onto a blocking thread (the snapshot sync takes the runtime
//! lock and makes a blocking sidecar call), stay in the application.

use rightkit_text_polish::vocabulary::VocabularyEntry;
use rightkit_text_polish::vocabulary_limits as limits;

use crate::sidecar::sync_sidecar_runtime_snapshot_best_effort;

/// The persisted vocabulary the commands mutate. Every method is the store's own operation, with
/// its own error text.
pub trait VocabularyCommandStore {
    /// Add a term with its sounds-like aliases.
    fn add_with_aliases(
        &self,
        term: &str,
        sounds_like: Vec<String>,
    ) -> Result<VocabularyEntry, String>;
    /// Accept a suggestion by term; `None` when there is no such suggestion.
    fn accept_suggestion(&self, term: &str) -> Result<Option<VocabularyEntry>, String>;
    /// Remove an entry by id; whether one was removed.
    fn remove(&self, id: u64) -> Result<bool, String>;
    /// Import many terms at once.
    fn import_terms(&self, terms: Vec<String>) -> Result<Vec<VocabularyEntry>, String>;
}

/// Validate the term and aliases against `limits`, add the entry, then sync the engine.
pub fn add_vocabulary<S: VocabularyCommandStore>(
    store: &S,
    limits: &limits::Limits,
    term: &str,
    sounds_like: Option<Vec<String>>,
) -> Result<VocabularyEntry, String> {
    limits::validate_term(term, limits)?;
    let aliases = match sounds_like {
        Some(aliases) => limits::validate_aliases(&aliases, limits)?,
        None => Vec::new(),
    };
    let entry = store.add_with_aliases(term, aliases)?;
    sync_sidecar_runtime_snapshot_best_effort("add_vocabulary");
    Ok(entry)
}

/// Accept a suggestion; sync the engine only when one was accepted.
pub fn accept_vocab_suggestion<S: VocabularyCommandStore>(
    store: &S,
    term: &str,
) -> Result<Option<VocabularyEntry>, String> {
    let accepted = store.accept_suggestion(term)?;
    if accepted.is_some() {
        sync_sidecar_runtime_snapshot_best_effort("accept_vocab_suggestion");
    }
    Ok(accepted)
}

/// Remove an entry; sync the engine only when one was removed.
pub fn remove_vocabulary<S: VocabularyCommandStore>(store: &S, id: u64) -> Result<bool, String> {
    let changed = store.remove(id)?;
    if changed {
        sync_sidecar_runtime_snapshot_best_effort("remove_vocabulary");
    }
    Ok(changed)
}

/// Check the import count against `limits`, import the terms, then sync the engine once.
pub fn import_vocabulary<S: VocabularyCommandStore>(
    store: &S,
    limits: &limits::Limits,
    terms: Vec<String>,
) -> Result<Vec<VocabularyEntry>, String> {
    limits::check_import_count(terms.len(), limits)?;
    let entries = store.import_terms(terms)?;
    sync_sidecar_runtime_snapshot_best_effort("import_vocabulary");
    Ok(entries)
}

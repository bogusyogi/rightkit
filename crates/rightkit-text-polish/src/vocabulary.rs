//! Vocabulary store — pure data + operations (no disk, no globals).
//!
//! The disk-backed singleton (load/save, `dirs` path, `Mutex<OnceLock>`) lives
//! in the host application and delegates the actual mutations here so
//! they can be unit-tested deterministically (time is passed in, not read).

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VocabularyEntry {
    pub id: u64,
    pub term: String,
    pub created_at_ms: i64,
    #[serde(default)]
    pub sounds_like: Vec<String>,
}

/// A learn-from-edit vocabulary suggestion. Surfaced in the renderer with
/// `seen` repeat counts to throttle one-off typo edits; multi-token phrase
/// corrections surface immediately because they are a strong signal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SuggestedEntry {
    pub term: String,
    pub heard_as: String,
    #[serde(default)]
    pub seen: u32,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VocabularyStore {
    pub entries: Vec<VocabularyEntry>,
    pub next_id: u64,
    #[serde(default)]
    pub suggestions: Vec<SuggestedEntry>,
}

pub fn normalize(term: &str) -> String {
    term.trim().to_string()
}

pub fn validate(term: &str) -> Result<(), String> {
    let t = term.trim();
    if t.is_empty() {
        return Err("term is empty".to_string());
    }
    if t.len() > 200 {
        return Err(format!("term too long ({} > 200)", t.len()));
    }
    if t.contains('\n') || t.contains('\r') {
        return Err("term cannot contain newlines".to_string());
    }
    Ok(())
}

impl VocabularyStore {
    /// Validate + normalize + dedup (case-insensitive) + assign id + append.
    /// `now_ms` is supplied by the caller so this stays time-pure.
    pub fn add(&mut self, term: &str, now_ms: i64) -> Result<VocabularyEntry, String> {
        validate(term)?;
        let normalized = normalize(term);
        let lowercased = normalized.to_lowercase();
        if self
            .entries
            .iter()
            .any(|e| e.term.to_lowercase() == lowercased)
        {
            return Err(format!("'{}' already in vocabulary", normalized));
        }
        let id = self.next_id.max(1);
        self.next_id = id + 1;
        let entry = VocabularyEntry {
            id,
            term: normalized,
            created_at_ms: now_ms,
            sounds_like: Vec::new(),
        };
        self.entries.push(entry.clone());
        Ok(entry)
    }

    pub fn add_with_aliases(
        &mut self,
        term: &str,
        sounds_like: Vec<String>,
        now_ms: i64,
    ) -> Result<VocabularyEntry, String> {
        let mut entry = self.add(term, now_ms)?;
        let mut seen = HashSet::new();
        let aliases: Vec<String> = sounds_like
            .into_iter()
            .map(|alias| alias.trim().to_string())
            .filter(|alias| alias_is_safe(alias))
            .filter(|alias| seen.insert(alias.to_lowercase()))
            .collect();
        if let Some(stored) = self.entries.iter_mut().find(|stored| stored.id == entry.id) {
            stored.sounds_like = aliases.clone();
        }
        entry.sounds_like = aliases;
        Ok(entry)
    }

    /// Remove by id. Returns whether anything was removed.
    pub fn remove(&mut self, id: u64) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| e.id != id);
        self.entries.len() != before
    }

    /// Bulk import. Dedup is checked against the entries that existed BEFORE
    /// the batch (matching the original behavior — duplicates *within* the same
    /// batch are not de-duplicated against each other).
    pub fn import(&mut self, terms: Vec<String>, now_ms: i64) -> Vec<VocabularyEntry> {
        let mut added = Vec::new();
        let mut existing: HashSet<String> =
            self.entries.iter().map(|e| e.term.to_lowercase()).collect();
        for raw in terms {
            let t = normalize(&raw);
            if t.is_empty() {
                continue;
            }
            if validate(&t).is_err() {
                continue;
            }
            let key = t.to_lowercase();
            // Dedup against BOTH the prior store AND terms already accepted in
            // THIS batch — otherwise a term repeated within one import is added
            // twice (audit test gap: vocabulary in-batch dedup).
            if !existing.insert(key) {
                continue;
            }
            let id = self.next_id.max(1);
            self.next_id = id + 1;
            let entry = VocabularyEntry {
                id,
                term: t,
                created_at_ms: now_ms,
                sounds_like: Vec::new(),
            };
            self.entries.push(entry.clone());
            added.push(entry);
        }
        added
    }

    /// Merge a batch of learn-from-edit suggestions. Dedup on lowercase term;
    /// a repeat hint bumps `seen` rather than creating a duplicate row.
    pub fn suggest_all(
        &mut self,
        candidates: Vec<crate::vocab_learn::Suggestion>,
        now_ms: i64,
    ) -> Vec<SuggestedEntry> {
        let mut added = Vec::new();
        for cand in candidates {
            let term_key = cand.term.to_lowercase();
            if let Some(existing) = self
                .suggestions
                .iter_mut()
                .find(|s| s.term.to_lowercase() == term_key)
            {
                existing.seen = existing.seen.saturating_add(1);
                added.push(existing.clone());
                continue;
            }
            let entry = SuggestedEntry {
                term: cand.term,
                heard_as: cand.heard_as,
                seen: 1,
                created_at_ms: now_ms,
            };
            self.suggestions.push(entry.clone());
            added.push(entry);
        }
        added
    }

    /// Accept a suggestion. If the term already exists, merge the heard-as
    /// alias into the existing entry instead of erroring on a duplicate.
    /// Returns the resulting vocabulary entry (or `None` if no suggestion
    /// with that term existed).
    pub fn accept_suggestion(&mut self, term: &str, now_ms: i64) -> Option<VocabularyEntry> {
        let lower = term.to_lowercase();
        let idx = self
            .suggestions
            .iter()
            .position(|s| s.term.to_lowercase() == lower)?;
        let suggestion = self.suggestions.remove(idx);
        let heard_as = vec![suggestion.heard_as];
        // Try to merge into an existing entry; otherwise create a fresh one.
        if let Some(existing) = self
            .entries
            .iter_mut()
            .find(|e| e.term.to_lowercase() == lower)
        {
            let mut seen: HashSet<String> = existing
                .sounds_like
                .iter()
                .map(|a| a.to_lowercase())
                .collect();
            for alias in heard_as {
                let trimmed = alias.trim().to_string();
                if alias_is_safe(&trimmed) && seen.insert(trimmed.to_lowercase()) {
                    existing.sounds_like.push(trimmed);
                }
            }
            Some(existing.clone())
        } else {
            self.add_with_aliases(&suggestion.term, heard_as, now_ms)
                .ok()
        }
    }

    /// Drop a suggestion (user dismissed it).
    pub fn dismiss_suggestion(&mut self, term: &str) -> bool {
        let before = self.suggestions.len();
        self.suggestions
            .retain(|s| s.term.to_lowercase() != term.to_lowercase());
        self.suggestions.len() != before
    }

    /// Borrow the suggestions list for the renderer.
    pub fn suggestions(&self) -> &[SuggestedEntry] {
        &self.suggestions
    }
}

fn alias_is_safe(alias: &str) -> bool {
    if alias.is_empty() || alias.len() > 200 || alias.contains('\n') || alias.contains('\r') {
        return false;
    }
    if alias.split_whitespace().count() >= 2 {
        return true;
    }
    !crate::vocab_learn::COMMON_WORD_STOPLIST.contains(&alias.to_lowercase().as_str())
}

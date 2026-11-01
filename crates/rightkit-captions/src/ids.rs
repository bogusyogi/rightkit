//! Stable subtitle cue ids.
//!
//! A cue id is a pure function of the language, the source media and the cue's
//! SOURCE word span (first word's source start to last word's source end). It
//! never reads output time or cue position, so a cue keeps its id when an
//! unrelated edit moves it on the timeline. A span placed twice in one track
//! gets `-2`, `-3`, ... on its repeats, counted in output order.

use std::collections::HashMap;

use crate::segment::{normalize_words, Cue};
use crate::words::TimedWord;

/// Hash domain for cue ids. Bump it if the derivation changes.
// Kept from CutRight: the schema string is hashed into every stored cue id, so changing it would
// re-key existing projects.
pub const CUE_ID_SCHEMA: &str = "cutright.caption-cue.v1";

/// Id for a cue covering `span` (source timeline, ms) in `language` of `source`.
/// The language is case-folded so `EN` and `en` name one track.
pub fn cue_id(language: &str, source: &str, span: (u64, u64)) -> String {
    let language = language.trim().to_ascii_lowercase();
    let mut hasher = blake3::Hasher::new();
    hasher.update(CUE_ID_SCHEMA.as_bytes());
    for part in [language.as_str(), source] {
        hasher.update(&(part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    hasher.update(&span.0.to_le_bytes());
    hasher.update(&span.1.to_le_bytes());
    let hex = hasher.finalize().to_hex();
    format!("cue-{}", &hex.as_str()[..16])
}

/// Source span of each cue: first word's source start to last word's source end.
/// `words` must be the slice the cues were built from; `None` for a cue whose
/// words carry no source span.
pub fn cue_source_spans(words: &[TimedWord], cues: &[Cue]) -> Vec<Option<(u64, u64)>> {
    // `build_cues` indexes the normalized words, so normalize the same way here.
    let normalized = normalize_words(words);
    cues.iter()
        .map(|c| {
            let first = normalized.get(c.first_word)?.source_ms?;
            let last = normalized.get(c.last_word)?.source_ms?;
            Some((first.0, last.1))
        })
        .collect()
}

/// Ids for `cues`, in cue order. Repeats are not yet disambiguated; see
/// [`disambiguate_ids`].
pub fn cue_ids(
    words: &[TimedWord],
    cues: &[Cue],
    language: &str,
    source: &str,
) -> Vec<Option<String>> {
    cue_source_spans(words, cues)
        .into_iter()
        .map(|span| span.map(|s| cue_id(language, source, s)))
        .collect()
}

/// Makes ids unique within one track: the k-th repeat of an id gets `-k`
/// (k >= 2). Pass ids in output-time order.
pub fn disambiguate_ids(ids: &mut [String]) {
    let mut seen: HashMap<String, usize> = HashMap::new();
    for id in ids.iter_mut() {
        let count = seen.entry(id.clone()).or_insert(0);
        *count += 1;
        if *count > 1 {
            *id = format!("{id}-{count}");
        }
    }
}

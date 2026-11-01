//! L11: surface FTS5 `snippet()` match spans on search results so the UI
//! lane (C6) can highlight matched substrings in the thread list without
//! re-running its own substring search. Backend field only — no UI here
//! (DISPATCH.md L11 item 4).
//!
//! `snippet()` is used (not `highlight()`) because it also truncates around
//! the match, which is what a search-result row needs; `highlight()` returns
//! the WHOLE column with markers, which is wasteful for a subject/body-sized
//! field. Match spans are wrapped in `\u{1}`/`\u{2}` (Unicode SOH/STX) rather
//! than HTML tags — these bytes never occur in real mail text, so the UI can
//! split on them without an HTML parser and choose its own highlight markup.

use super::query::SearchTerms;
use super::store::{SearchRow, SearchStore};

/// snippet() token budget around the match — same order of magnitude as the
/// `excerpt_around` max_chars used by `enrich.rs` for the plain-text excerpt.
const SNIPPET_TOKENS: u32 = 24;

/// Populate `matching_message_snippet` on every row that already has a
/// `matching_message_id` from `enrich_search_matches` (i.e. every row from an
/// FTS-anchored query). Runs as a second batched pass keyed by message id —
/// deliberately separate from `enrich.rs`'s excerpt query since `snippet()`
/// requires selecting directly from the `fts_messages` virtual table (not a
/// plain column), which the existing batched excerpt query does not do.
pub fn attach_snippets<S: SearchStore>(
    store: &S,
    out: &mut [S::Row],
    q: &SearchTerms,
) -> Result<(), String> {
    let Some(fts) = q.fts() else {
        return Ok(());
    };
    let message_ids: Vec<String> = out
        .iter()
        .filter_map(|r| r.matching_message_id().map(str::to_string))
        .collect();
    if message_ids.is_empty() {
        return Ok(());
    }

    // fts_messages is rowid-keyed to messages.rowid (SCHEMA_V9), so join back
    // through messages to resolve each message_id to its FTS rowid, then call
    // snippet() on that rowid. subject(col 2)/from_text(col 3)/body(col 4)
    // per the fts_messages column order in db_schema.rs; snippet() picks
    // whichever column arg (-1 = best match across all columns) contains the
    // highest-ranked match.
    let placeholders = message_ids
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    let snippet_expr = rightkit_search::snippet::fts_snippet_expr("fts_messages", SNIPPET_TOKENS);
    let sql = format!(
        "SELECT m.id,
                {snippet_expr}
         FROM messages m
         JOIN fts_messages ON fts_messages.rowid = m.rowid
         WHERE m.id IN ({placeholders})
           AND fts_messages MATCH ?"
    );
    let mut params: Vec<String> = message_ids.clone();
    params.push(fts);

    let snippets: std::collections::HashMap<String, String> = store
        .query_text_rows(&sql, &params)
        .map_err(|e| format!("query snippet: {e}"))?
        .into_iter()
        .filter_map(|r| {
            let [id, snippet]: [String; 2] = r.try_into().ok()?;
            Some((id, snippet))
        })
        .collect();

    for row in out.iter_mut() {
        let Some(snippet) = row
            .matching_message_id()
            .and_then(|mid| snippets.get(mid))
            .cloned()
        else {
            continue;
        };
        row.set_matching_snippet(snippet);
    }

    Ok(())
}

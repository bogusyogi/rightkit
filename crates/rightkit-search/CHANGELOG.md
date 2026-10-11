# Changelog

## Unreleased

**Behaviour change: folding is now Unicode default case folding (fold version 2).** Nothing here was compiled or run locally; verification is the CI run.

- **`fold` changed:** it was `str::to_lowercase`; it is now per-character full case folding: `ß`/`ẞ` -> `ss`, `Σ`/`σ`/`ς` -> `σ` (final sigma), `ſ` -> `s`, `µ` -> `μ`, Greek variant letters (`ϐ ϑ ϕ ϖ ϰ ϱ ϵ`), Latin ligatures U+FB00..U+FB06, and the expansions `ŉ ǰ ΐ ΰ և ẖ ẗ ẘ ẙ ẚ`. Coverage is stated in the `fold` module docs; Greek ypogegrammeni capitals (U+1F80..U+1FFC), Armenian ligatures and Cherokee still fold as `to_lowercase` does. No new dependency (`caseless` and `unicode-normalization` are not dependencies of this crate).
- **New:** `FoldOptions { turkic }` (`FoldOptions::turkic()`), `fold_with`, `folded_len`, `TextMatcher::with_options`. Turkish/Azeri folding (`I` -> `ı`, `İ` -> `i`) is opt-in; the default is locale-neutral. Index and query must use the same options.
- **New:** `FOLD_VERSION` (2; 1 was `to_lowercase`), `check_fold_version(stored) -> Result<(), RebuildNeeded>`. This crate persists no index (the FTS5 predicates leave folding to SQLite; the matcher and filename index fold in memory), so there is nothing to migrate here. A consumer that stores folded text must store `FOLD_VERSION` beside it and rebuild on `RebuildNeeded`.
- **Same folding everywhere:** `TextMatcher` (match and snippet offsets, now exact because folding is context-free), `FilenameIndex` (basename, query and extension; were `to_lowercase`), `snippet::excerpt_around` and `snippet::contains_any_term` (were `to_lowercase`; `excerpt_around` now maps the folded hit back to a source character even when folding changes the character count).
- **Behaviour for existing consumers:** more things match (`ß`/`ss`, `ς`/`σ`, ligatures); `Σ` at a word end no longer folds to `ς`, so anything stored with the old `fold` must be refolded. There is still no diacritic stripping and no normalisation, as before.
- **Tests:** `tests/fold.rs`.

## 0.2.1

**Additive release.** Every change below is a new item. No existing type, function, variant or field changed, so 0.2.0 callers compile unchanged and the `pulse` pin (`=0.2.0`) is unaffected. Nothing in this entry was compiled or run locally; verification is the CI run.

- **New module `fold`:** `fold(&str) -> String`, `str::to_lowercase` with no normalisation. Std-only, by design. Limits: decomposed accents (`e` + U+0301) do not match precomposed ones (`é`); `İ` folds to two characters; a word-final `Σ` folds to `ς`.
- **New module `matcher`:** `TextMatcher` evaluates free text over explicit haystacks (case-folded substring, AND across haystacks, contiguous phrases, OR groups, negation, `%` and `_` literal). `matches_with(eager, lazy)` calls `lazy` only when `eager` cannot decide. `snippet(text, max_chars)` returns a `Snippet` of `Span`s around the first hit. Types `MatchError` (`MixedOrGroup`) and `Snippet` (`#[non_exhaustive]`).
- **New `Query::literal(&str)`:** the whole trimmed input as one phrase; operators, quotes, `OR` and `-` are not interpreted.
- **New `Query::filters_only(&self)`:** operator terms and operator-only OR groups, with free text removed. Build the `TextMatcher` from the full query first; `TextMatcher::new` refuses a mixed OR group, and `filters_only` drops it.
- **New module `page`:** keyset paging. `Keyset` (`asc`, `desc`), `Cursor` (`new`, `values`, `encode`, `decode`), `CursorError` (`Malformed`, `Arity`, `EmptyKeyset`; `#[non_exhaustive]`), `PagedSql`, and `Compiled::page(select_from, keyset, after, limit)`. The last keyset column must be unique. An empty keyset returns `CursorError::EmptyKeyset`.
- **New module `scan`:** `ScanBudget::new(max_rows, chunk)`, `scan_page(after, limit, budget, fetch, key, matches) -> ScanPage<T>` (`#[non_exhaustive]`, with `hits`, `next`, `scanned`), and `read_text_prefix(path, max_bytes)`, which drops an incomplete trailing UTF-8 sequence. A needle that straddles the caller's byte cap is missed.
- **Root re-exports:** `fold`, `MatchError`, `Snippet`, `TextMatcher`, `Cursor`, `CursorError`, `Keyset`, `PagedSql`, `read_text_prefix`, `scan_page`, `ScanBudget`, `ScanPage`. The existing exports are unchanged.
- **Tests:** `tests/matcher.rs`, `tests/paging.rs`, `tests/scan.rs` and `tests/history_e2e.rs` (SQLite table plus transcript files, scan strategy).
- **No new dependencies.** `rusqlite` and `tempfile` were already dev-dependencies.

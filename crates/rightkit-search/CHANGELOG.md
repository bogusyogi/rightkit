# Changelog

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

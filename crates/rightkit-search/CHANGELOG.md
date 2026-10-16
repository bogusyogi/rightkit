# Changelog

## 0.4.0

**Why:** MailRight (bogusyogi/mailright#8) found `ChildScope` (0.3.0) could not express its thread predicates (Orthic-Labs/rightkit#166). **0.x breaking, in the narrow sense below; existing `ChildScope::new` / `Schema::scope` callers compile and behave unchanged.** Not compiled or run locally; CI is the compiler.

- **Breaking (source):** `ChildScope` gained public fields (`negated_text_like`, `op_sources`), so a struct-literal `ChildScope { from, link }` no longer compiles; use `ChildScope::new`. `Binding`, `ValueKind` and `CompileError` gained variants and are not `#[non_exhaustive]`, so an exhaustive `match` over them in a caller needs new arms. With none of the new items used, the compiled SQL and params are byte-identical to 0.3.0.
- **New `Binding::SqlMulti { sql, kinds }`:** a fragment with one `?` per entry of `kinds`; the operator value is converted by each kind and bound in order (e.g. a recipient `EXISTS` matching an address prefix or an `@domain` suffix). `Binding::Sql` (exactly one `?`) is unchanged. A `?` count different from `kinds.len()` is `CompileError::BindCount`; a `?` inside a SQL string literal is not supported.
- **New `ValueKind::Prefix` (`value%`) and `ValueKind::Suffix` (`%value`):** `LIKE` patterns with `%`, `_` and `\` in the value escaped (use `ESCAPE '\'` in the SQL), like `Contains`.
- **New `ChildScope::negated_text_like(&[columns])`:** in a scope, a negated free-text term drops the parent when any listed column `LIKE`s the term (contains, escaped) or the FTS table matches it: one `NOT EXISTS` per term with one bind per column plus one for FTS (subject LIKE, snippet LIKE, FTS MATCH = three). Default (no columns) is the 0.3.0 FTS-only behaviour.
- **New `ChildScope::op_source(op, from, link)` and `OpSource`:** an operator whose columns live in another child table (`-label:` over `thread_labels`) is evaluated over its own source: `-op:` becomes `NOT EXISTS (SELECT 1 FROM <from> WHERE <link> AND ...)`, and positive `op:` terms share one `EXISTS` over that source. Any number of sources; each operator maps to one. Using such an operator inside an OR group is `CompileError::OpSourceInOrGroup` (it cannot be expressed across sources).
- **All values stay bound parameters;** only the static fragments supplied by the schema are in the SQL text.
- **Tests:** `tests/scope_e2e.rs` (multi-bind, three-way negated text, negation and positive terms over a second child table, mixed negation sources, error cases, injection-shaped and wildcard values). Also fixed the pre-existing `unscoped_schema_is_unchanged` expectation, whose Rust string literal had a single backslash (`'\'`, which is just `'`) instead of `'\\'`.

## 0.3.0

**Behaviour change: folding is now Unicode default case folding (fold version 2).** Nothing here was compiled or run locally; verification is the CI run.

- **`fold` changed:** it was `str::to_lowercase`; it is now per-character full case folding: `ß`/`ẞ` -> `ss`, `Σ`/`σ`/`ς` -> `σ` (final sigma), `ſ` -> `s`, `µ` -> `μ`, Greek variant letters (`ϐ ϑ ϕ ϖ ϰ ϱ ϵ`), Latin ligatures U+FB00..U+FB06, and the expansions `ŉ ǰ ΐ ΰ և ẖ ẗ ẘ ẙ ẚ`. Coverage is stated in the `fold` module docs; Greek ypogegrammeni capitals (U+1F80..U+1FFC), Armenian ligatures and Cherokee still fold as `to_lowercase` does. No new dependency (`caseless` and `unicode-normalization` are not dependencies of this crate).
- **New:** `FoldOptions { turkic }` (`FoldOptions::turkic()`), `fold_with`, `folded_len`, `TextMatcher::with_options`. Turkish/Azeri folding (`I` -> `ı`, `İ` -> `i`) is opt-in; the default is locale-neutral. Index and query must use the same options.
- **New:** `FOLD_VERSION` (2; 1 was `to_lowercase`), `check_fold_version(stored) -> Result<(), RebuildNeeded>`. This crate persists no index (the FTS5 predicates leave folding to SQLite; the matcher and filename index fold in memory), so there is nothing to migrate here. A consumer that stores folded text must store `FOLD_VERSION` beside it and rebuild on `RebuildNeeded`.
- **Same folding everywhere:** `TextMatcher` (match and snippet offsets, now exact because folding is context-free), `FilenameIndex` (basename, query and extension; were `to_lowercase`), `snippet::excerpt_around` and `snippet::contains_any_term` (were `to_lowercase`; `excerpt_around` now maps the folded hit back to a source character even when folding changes the character count).
- **Behaviour for existing consumers:** more things match (`ß`/`ss`, `ς`/`σ`, ligatures); `Σ` at a word end no longer folds to `ς`, so anything stored with the old `fold` must be refolded. There is still no diacritic stripping and no normalisation, as before.
- **Tests:** `tests/fold.rs`.

**Additive: parent/child match scope (Orthic-Labs/rightkit#135).** No existing item changed; without a scope the output is byte-identical. This section is additive on its own; the release already needs a minor bump (0.3.0) for the fold change above. Not compiled or run locally; CI is the compiler.

- **New `ChildScope { from, link }`** (`ChildScope::new`) and **`Schema::scope(ChildScope)`**. Bindings and the FTS `rowid_expr` then describe the child row (e.g. a message) and the `WHERE` body filters the parent (e.g. a thread). All positive terms compile to one `EXISTS (SELECT 1 FROM <from> WHERE <link> AND ...)`, so they co-occur in one child row. Each negated operator term and each negated free-text term compiles to its own `NOT EXISTS (SELECT 1 FROM <from> WHERE <link> AND <un-negated term>)`, so `-from:x` drops the whole parent when any child matches. OR groups stay inside the positive `EXISTS`. A query with only negated terms emits only `NOT EXISTS` clauses.
- **Tests:** `tests/scope_e2e.rs` (SQLite threads over messages with FTS5).

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

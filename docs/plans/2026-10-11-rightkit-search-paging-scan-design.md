# rightkit-search: paging, substring matching and bounded scans (design ruling)

Date: 11 October 2026. Design ruling by Sage from ScrapeRight's gap list (`scraperight/docs/handoff-2026-10-11/rightkit-gaps-jobs-search.md`). Not yet implemented. Lead's decision on the open point: folding stays std-only for 0.2.1 (no normalisation dependency); the limit is documented.

**Evidence limits.** `/Volumes/D/claude/mailright/docs/PERFORMANCE-ARCHITECTURE-2026-10-11.md` does not exist in this checkout, so the MailRight facts below come from your brief and are unverified. Neither MailRight nor ScrapeRight depends on `rightkit-search` today; the only consumer is `pulse/core/Cargo.toml:24`, pinned `=0.2.0`, using `filename` only. The crate has no CHANGELOG.

## 1. Rulings

| # | Item | Ruling |
|---|---|---|
| 1 | Keyset paging | ACCEPT, `page.rs` |
| 2 | Whole-query substring, Unicode folding | ACCEPT, `fold.rs`, `matcher.rs`, `Query::literal` |
| 3 | AND, phrase, negation, escaping | Parsing ALREADY PRESENT: call `Schema::parse` or `Query::parse` (`terms.rs:35`, `:113-117`; `lexer.rs:54-68`; `ast.rs:216`, `:261-266`). Evaluation without FTS is missing (`sql.rs:150-160`); ACCEPT via `TextMatcher`, where `%` and `_` are literal because nothing uses `LIKE`. |
| 4 | Bounded body scan, budget, cursor | ACCEPT, `scan.rs` |
| 5 | Highlight snippets | `Span` and `split_highlights` ALREADY PRESENT (`snippet.rs:16-44`) for FTS. ACCEPT `TextMatcher::snippet` for scanned text. The stored 160-character prefix (`history.rs:798`) stays in the app. |
| 6 | Body source set | STAYS IN THE APP. Which of `.txt`, `.md`, `.srt` is searched is product policy. The crate takes explicit haystacks and discovers no files; ScrapeRight declares a constant and tests it. |
| 7 | Upsert by id | STAYS IN THE APP. It is storage SQL on the app's own table, already correct (`SR/crates/store/src/lib.rs:90-91`, `:152`). `rightkit-db` has no row API and should not gain one for this. |

```rust
// terms.rs
impl Query { pub fn literal(input: &str) -> Query;   // whole trimmed input as one phrase
             pub fn filters_only(&self) -> Query; }  // ops and op-only OR groups
// fold.rs
pub fn fold(s: &str) -> String;                      // str::to_lowercase
// matcher.rs
pub struct TextMatcher { /* private */ }
#[non_exhaustive] pub enum MatchError { MixedOrGroup }
#[non_exhaustive] pub struct Snippet { pub spans: Vec<Span>, pub cut_start: bool, pub cut_end: bool }
impl TextMatcher {
  pub fn new(q: &Query) -> Result<Self, MatchError>;
  pub fn is_empty(&self) -> bool;
  pub fn matches_with(&self, eager: &[&str], lazy: impl FnOnce() -> Vec<String>) -> bool;
  pub fn snippet(&self, text: &str, max_chars: usize) -> Option<Snippet>; }
// page.rs
pub struct Keyset { /* private */ }
impl Keyset { pub fn new() -> Self; pub fn desc(self, expr: &'static str) -> Self;
              pub fn asc(self, expr: &'static str) -> Self; }
#[derive(Clone, Debug, PartialEq)] pub struct Cursor(Vec<Param>);
impl Cursor { pub fn new(v: Vec<Param>) -> Self; pub fn values(&self) -> &[Param];
              pub fn encode(&self) -> String; pub fn decode(s: &str) -> Result<Self, CursorError>; }
#[non_exhaustive] pub enum CursorError { Malformed, Arity { expected: usize, got: usize } }
#[non_exhaustive] pub struct PagedSql { pub sql: String, pub params: Vec<Param> }
impl Compiled { pub fn page(&self, select_from: &str, keyset: &Keyset,
                after: Option<&Cursor>, limit: usize) -> Result<PagedSql, CursorError>; }
// scan.rs
pub struct ScanBudget { /* private */ }
impl ScanBudget { pub fn new(max_rows: usize, chunk: usize) -> Self; }
#[non_exhaustive] pub struct ScanPage<T> { pub hits: Vec<T>, pub next: Option<Cursor>, pub scanned: usize }
pub fn scan_page<T, E>(after: Option<Cursor>, limit: usize, budget: ScanBudget,
  fetch: impl FnMut(Option<&Cursor>, usize) -> Result<Vec<T>, E>,
  key: impl Fn(&T) -> Cursor, matches: impl FnMut(&T) -> bool) -> Result<ScanPage<T>, E>;
pub fn read_text_prefix(path: &Path, max_bytes: u64) -> std::io::Result<String>;
```

Semantics the lanes must follow:
- **`Compiled::page`** emits `WHERE (<where_sql>) AND <expanded OR-chain> ORDER BY … LIMIT ?`, binding `limit + 1`. Parameter order is where, cursor, limit. `select_from` carries no placeholders. The last keyset column must be unique.
- **`Cursor::encode`** is `k1` followed by `i<digits>;` or `t<byte_len>:<text>` per value.
- **`scan_page`** sets `next` to the key of the last row examined (hit or not) when the page fills or the budget ends, and to `None` only when `fetch` returns fewer rows than asked.
- **`matches_with`** calls `lazy` only when the eager haystacks cannot decide. Every positive term must appear in some haystack; no negated term may appear in any.
- **`read_text_prefix`** drops an incomplete trailing UTF-8 sequence and decodes the rest lossily.

## 2. One query model

- **Shared:** syntax (lexer, AST, `Query`), operator filters compiled by `Schema`, `Keyset` and `Cursor`, `Span`.
- **Per backend:** only how free text is evaluated.
  - **Index (MailRight):** `Schema::fts` then `compile` then `Compiled::page`. The existing uncorrelated `IN (SELECT rowid … MATCH ?)` (`sql.rs:241-248`) keeps the hit set to one evaluation per statement.
  - **Scan (ScrapeRight):** `schema.compile(&q.filters_only())` then `page` as `fetch`, with `TextMatcher` as `matches` inside `scan_page`.
- **No SQL substring path.** A term in the title and another in the body must AND across sources, which SQL cannot see.

## 3. Lanes

- **A (text):** `src/terms.rs`, `src/fold.rs`, `src/matcher.rs`, `tests/matcher.rs`.
- **B (paging and scan):** `src/page.rs`, `src/scan.rs`, `tests/paging.rs`, `tests/scan.rs`.
- **C (integration):** `src/lib.rs`, `Cargo.toml`, `README.md`, new `CHANGELOG.md`, `tests/history_e2e.rs`.

A and B run in parallel. C can be written in parallel against these signatures but lands last; nothing compiles until C adds the modules to `lib.rs`.

## 4. Tests

**Lane A**
- `folds_unicode_case`: "Café" matches "CAFÉ"; "東京" matches; "ÉCOLE" matches "école".
- `mid_word_fragment_matches`: "anscr" matches "Transcript".
- `terms_and_across_haystacks`: `a b` needs both, and they may sit in different haystacks.
- `phrase_is_contiguous`, `negation_checks_every_haystack`.
- `percent_and_underscore_are_literal`.
- `lazy_not_called_when_eager_decides`.
- `mixed_or_group_is_typed_error`.
- `literal_ignores_operators_and_quotes`.
- `snippet_spans_reassemble_to_a_substring_and_matched_spans_fold_to_a_term`, including a case where folding changes length ("İ").

**Lane B**
- `page_two_starts_strictly_after_page_one_with_equal_sort_keys`.
- `rows_inserted_between_pages`: insert a newer row, an equal-timestamp higher-id row, and a row in page 2's range, then delete the cursor row. Page 2 must have no duplicate, no skipped pre-existing row, exclude the newer rows and include the in-range row.
- `cursor_roundtrip_and_arity_error`, `fts_filter_pages_newest_first`.
- `budget_bounds_rows_fetched`, with a counting fetch.
- `budget_exhaustion_returns_resumable_cursor`: resuming yields the same union as an unbudgeted scan.
- `exhausted_source_returns_none`.
- `multibyte_cut_at_cap_still_matches_earlier_text`.

**Lane C**
- `history_e2e`: SQLite table plus transcript files; pages concatenate to the full hit set; a body hit yields a snippet.

## 5. Compatibility

- Purely additive, so publish as 0.2.1; the pulse pin is unaffected.
- **Would break struct literals:** new fields on `Compiled`, `Leaf`, `OpTerm`, `Fts`, `Span`, or `filename::{Entry, Request, Page}`.
- **Would break exhaustive matches:** new variants on `Binding`, `ValueKind`, `Param`, `CompileError`, `ParseError`, `Node`, `Token`, or `filename::{Error, Kind}`.
- **How to avoid it:** add none of those. New behaviour goes in new methods and new types, which use private fields or `#[non_exhaustive]`. Adding `#[non_exhaustive]` to an existing type is itself breaking, so defer that to 0.3.

## 6. Risks and least-sure decisions

- **Text-typed `created_at`:** ScrapeRight's ordering breaks when digit counts differ (gap file line 44). The app must key on `CAST(created_at AS INTEGER)` or migrate the column.
- **Body cap boundary:** a needle that straddles the 4 MiB cap is still missed. This is a declared limit.
- **Full page on the last row:** `scan_page` returns a cursor, and the next call returns an empty final page.
- **Least sure, std-only folding:** there is no normalisation, so decomposed "é" will not match precomposed, and "İ" will not match "i". The alternative is a `unicode-normalization` dependency with NFKC.
- **Least sure, no SQL folded-substring strategy:** metadata-only searches load rows instead of filtering in SQLite. That is fine at history scale and wrong if a large-table consumer appears.

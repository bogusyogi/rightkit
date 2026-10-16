# rightkit-search

Product-neutral search query language for Right Suite apps. It owns the tokenizer, the parser (with caller-defined operators), the `Query` data model, an FTS5 expression builder, a SQL predicate compiler (`Schema`), snippets and excerpts, and an optional in-memory filename index.

**Use it** when an app needs one search syntax across its records, compiled to SQLite FTS5 and SQL predicates, with highlighted snippets.

**Do not use it** as a search engine. It compiles queries and builds predicates; the caller runs them against its own database or index.

## Features

| Feature | Default | Pulls in | Effect |
|---|---|---|---|
| `sqlite` | on | `rusqlite` | The `rusqlite::ToSql` impl for `Param` (`src/sql.rs:17-25`); the SQL predicate compiler itself (`src/sql.rs`, `src/lib.rs:13`) is not gated. |
| `filename-index` | off | none | In-memory filename index (`src/filename.rs`). |

Disable `sqlite` (`default-features = false`) to use the parser and data model without `rusqlite`.

## Platform support

No `cfg(target_os)` gates in `src/`. `rusqlite` is a dependency with default features off, so the library build does not bundle SQLite; the `bundled` feature is enabled only for dev-dependencies, i.e. the tests (`Cargo.toml:20`, `:23`). Platform support follows `rusqlite` and the consumer's SQLite selection.

Published version: see `INDEX.md` at the repository root. Release notes are in `CHANGELOG.md`.

## Example

Illustrative; not compiled as a doctest. For compiled usage against a real SQLite database, see `tests/sqlite_e2e.rs`; for the filename index, see `tests/filename_index.rs`.

```rust,ignore
// Parse a user query into a Query, then compile it with a Schema
// that maps the app's searchable fields to columns or an FTS5 table.
```

## Parent/child scope (threads over messages)

`Schema::scope(ChildScope::new("messages m", "m.thread_id = t.id"))` makes bindings describe the child row while the `WHERE` body filters the parent: positive terms co-occur in one child (`EXISTS`), each negated term is its own `NOT EXISTS`. Refinements (0.4.0): `.negated_text_like(&["m.subject", "m.snippet"])` makes negated free text also `LIKE`-match those columns besides FTS; `.op_source("label", "thread_labels l", "l.thread_id = t.id")` evaluates an operator over another child table; `Binding::SqlMulti { sql, kinds }` binds one operator value to several `?` (with `ValueKind::Prefix`/`Suffix`). See `tests/scope_e2e.rs`.

## Paging, substring matching and bounded scans

Three additions for paging and free-text search, all in 0.2.1. Each is purely additive. Shared pieces are the query syntax (`Query`), operator filters (`Schema`), `Keyset`/`Cursor`, and `Span`. What differs by backend is how free text is evaluated.

**Index strategy (free text in FTS5).** Use this when the table has an FTS5 index. Free text becomes an uncorrelated `IN (SELECT rowid ... MATCH ?)` predicate, so the hit set is evaluated once per statement.

```rust,ignore
let schema = Schema::new()
    .fts("messages_fts", "messages.rowid")
    .bind("from", Binding::Eq("sender"));
let q = schema.parse(input)?;
let compiled = schema.compile(&q)?;
let keyset = Keyset::new().desc("received_at").desc("id"); // the last column is unique
let page = compiled.page(
    "SELECT id, received_at, subject FROM messages",
    &keyset,
    cursor.as_ref(), // None for the first page
    50,
)?;
// Run page.sql with page.params. Keep the first 50 rows. A 51st row means more pages:
// its cursor is built from the last kept row, and Cursor::encode gives the transport form.
```

**Scan strategy (free text in Rust).** Use this when the text lives outside SQL, for example transcript bodies on disk. SQL applies only the operator filters, and `TextMatcher` checks the free text. Build the matcher from the full query first: it refuses a mixed OR group, and `filters_only` silently drops that group.

```rust,ignore
let matcher = TextMatcher::new(&q)?;               // refuses mixed OR groups first
let filters = schema.compile(&q.filters_only())?;  // operator side only
let page = scan_page(
    cursor,                          // Option<Cursor> from the previous page
    50,
    ScanBudget::new(2_000, 200),     // at most 2,000 rows examined per call
    |after, n| {
        // Run filters.page("SELECT id, created_at, title, path FROM history", &keyset, after, n)
        // and return up to n rows.
        fetch_rows(after, n)
    },
    |row| Cursor::new(vec![Param::Int(row.created_at), Param::Int(row.id)]),
    |row| matcher.matches_with(&[&row.title], || vec![read_body(&row.path)]),
)?;
// page.next is Some while more rows may exist. Resume by passing it back as `cursor`.
// A snippet for a body hit: matcher.snippet(&body, 160).
```

**Folding limit (std-only).** Matching uses `fold` (`src/fold.rs`, `FOLD_VERSION` 2): per-character Unicode default case folding built on `char::to_lowercase`, with no Unicode normalisation. A decomposed accent (`e` + U+0301) does not match the precomposed `é`. `İ` folds to `i` + U+0307, so `İSTANBUL` and `istanbul` do not match. `Σ`, `σ` and `ς` all fold to `σ`, in any position. Text must use the same normalisation form on both sides. The crate has no normalisation dependency (`Cargo.toml`). Version 1 (`str::to_lowercase`, 0.2.1) folds differently, so text folded under it needs a rebuild (`check_fold_version`).

**Body-cap boundary.** `read_text_prefix(path, max_bytes)` reads at most `max_bytes`. The cap is the caller's choice. A needle that straddles the cap is missed, because the bytes after it are never read. The crate does not detect this.

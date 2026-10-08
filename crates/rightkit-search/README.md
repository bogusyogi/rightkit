# rightkit-search

Product-neutral search query language for Right Suite apps. It owns the tokenizer, the parser (with caller-defined operators), the `Query` data model, an FTS5 expression builder, a SQL predicate compiler (`Schema`), snippets and excerpts, and an optional in-memory filename index.

**Use it** when an app needs one search syntax across its records, compiled to SQLite FTS5 and SQL predicates, with highlighted snippets.

**Do not use it** as a search engine. It compiles queries and builds predicates; the caller runs them against its own database or index.

## Features

| Feature | Default | Pulls in | Effect |
|---|---|---|---|
| `sqlite` | on | `rusqlite` | SQL predicate compiler (`src/sql.rs`) and the SQLite-backed tests. |
| `filename-index` | off | none | In-memory filename index (`src/filename.rs`). |

Disable `sqlite` (`default-features = false`) to use the parser and data model without `rusqlite`.

## Platform support

No `cfg(target_os)` gates in `src/`. `rusqlite` is built with its bundled SQLite in the usual way, so platform support follows `rusqlite`.

Published version: see `INDEX.md` at the repository root. This crate has no `CHANGELOG.md`.

## Example

Illustrative; not compiled as a doctest. For compiled usage against a real SQLite database, see `tests/sqlite_e2e.rs`; for the filename index, see `tests/filename_index.rs`.

```rust,ignore
// Parse a user query into a Query, then compile it with a Schema
// that maps the app's searchable fields to columns or an FTS5 table.
```

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

Published version: see `INDEX.md` at the repository root. This crate has no `CHANGELOG.md`.

## Example

Illustrative; not compiled as a doctest. For compiled usage against a real SQLite database, see `tests/sqlite_e2e.rs`; for the filename index, see `tests/filename_index.rs`.

```rust,ignore
// Parse a user query into a Query, then compile it with a Schema
// that maps the app's searchable fields to columns or an FTS5 table.
```

# rightkit-search

Search query language shared by Right Suite apps: tokenizer, parser (caller-defined operators), `Query`
data model, FTS5 expression builder, SQL predicate compiler (`Schema`), snippets/excerpts, and an
optional in-memory filename index (`filename-index` feature). See `tests/` for end-to-end usage
against a real SQLite database.

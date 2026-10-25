# rightkit-search registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-search` to the exact version in its
`Cargo.toml` with the default `sqlite` feature and `filename-index`: every feature the crate has,
which is the union of what the apps select (most turn `sqlite` off, two keep it, one adds
`filename-index`). It also depends on `rusqlite` with its `bundled` feature, so the compiled SQL runs
on a real SQLite built from source rather than a system library. It is built from the sealed archive
through the offline directory registry. Nothing in it is platform-gated, so Windows, macOS and Linux
run the same assertions. Every public module is imported by path, so a removed item fails to compile.

It proves:

- Parsing: `tokenize`, `Syntax` (a lowercased vocabulary), `ast::parse` and `Query::parse` for
  operators, free text, phrases, negation, `OR` chains, `{}` groups and `()` groups; unknown
  operators, empty groups and unbalanced grouping are typed errors; a URL is free text;
  `parse_lenient`, `literal`, `filters_only`, `fts` and `fts_negated`.
- Values: `parse_date_ms`, `parse_relative_ms` with an explicit now, `parse_size_bytes`.
- SQL: `Schema::compile_at` for every `Binding` (`Like`, `Eq`, `Cmp` with each `ValueKind`, `Sql`,
  `SqlMulti`, `SqlByShape`, `Flags`), negation, OR groups, free text through an FTS table, a
  parent/child `ChildScope` with a separate operator source and the negated-text refinements, and
  every `CompileError`.
- Highlighting and folding: the FTS5 snippet expression, `split_highlights`, `excerpt_around`,
  Unicode default case folding (sharp s, sigma, ligatures, the Turkic option), `check_fold_version`.
- Matching: `TextMatcher` over eager and lazy haystacks (AND, phrase, OR group, negation, literal `%`
  and `_`, the `MixedOrGroup` refusal) and its highlighted snippets with the cut flags.
- Paging and scanning: `Keyset` SQL and parameters for `desc`, `asc`, `range_bound` and
  `cursor_expr`, every `CursorError`, the `Cursor` text form and its decode failures, `scan_page`
  budgets, limits, resume cursors, short sources and error propagation, and `read_text_prefix`.
- The filename index: insert, search by folded name, kind, extension and size, deterministic path
  order, pagination and `has_more`, every validation error, de-duplication and `remove_tree`.
- SQLite (feature `sqlite`): `Param` binds as text and integer; compiled predicates run on an
  in-memory database with an FTS5 external-content table and return exactly the expected rows for free
  text, prefixes, phrases, negation, escaped `%` and `_`, every `Binding`, OR groups, mixed groups and a
  parent/child scope; the FTS5 snippet expression runs and splits into highlight spans; keyset paging
  walks every row once in order across encoded and decoded cursors, with and without the range bound.

It does not prove `values::now_ms` (it reads the system clock) or `Schema::compile` (it calls
`now_ms`; `compile_at` is proven instead), SQLite behaviour beyond the statements the crate emits, or
the build without the `sqlite` feature (it differs only by the missing `ToSql` implementation).

State: an in-memory SQLite database and one small file under a per-process directory in the OS temp
directory, removed at the end. No network, no process spawn, no wall clock, no real user data. Run
only by the CI consumer lane.

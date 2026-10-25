# rightkit-platform-rules registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-platform-rules` to the exact version in
its `Cargo.toml` with default features (the crate has none; its `serde` and `serde_json`
dependencies come from the same registry). It is built from the sealed archive through the offline
directory registry. Nothing in it is platform-gated, so Windows, macOS and Linux run the same
assertions.

Every check uses an explicit check date (`check_at`, `hashtag_max_at`, `link_placements_at`,
`check_id_at`), so the result does not depend on the day it runs. It proves:

- The bundled catalog loads and keeps every revision. `Catalog::get` falls back from the account
  type to `all` and from the format to `any`, keeps the two X video limits apart, returns `None` for
  an absent rule, and answers with revision 2 where a rule has history (Instagram caption length).
- `check_at` accepts a value at the limit and refuses a value above a maximum, below a minimum, in
  the wrong unit, of the wrong type, with no rule, against an unsourced rule, against a stale
  revision binding, outside an enum, and against a boolean, each with the matching `Violation`.
- The persisted revision id `platform:hash`: 32 lowercase hex digits, stable across loads, scoped to
  its platform, round-tripping through `RuleRevision::parse`, `FromStr` and `Display`, a malformed id
  is `BadRevisionId`, and `is_current_id` turns false once the platform's rules change.
- `merge` and `merge_json`: a change is a new revision, the original catalog is unchanged, history is
  kept, an edit, a skipped revision and a renamed rule are refused, a source-only change keeps the
  hash, and a new key starts at revision 1.
- The promotion rules and shape errors on load: grade, source URL, stated value, conflict note, no
  value on an unsourced record, bad JSON, schema, dates, `min` above `max`, empty fields, duplicate
  revisions, a broken `supersedes` chain, and an unknown link placement.
- `Rule::limit` for every limit kind, `status_at` and `blocks_use_at` around a sunset date,
  exclusive bounds, `hashtag_max_at`, `link_placements_at`, `LinkPlacement` names, upload routes
  (`get` against `get_in`, `Planned::on_route`, `Rule::with_route`), `to_json` and `from_json`
  equality, and the message of every error type.

It does not prove the clock-dated variants `check`, `check_id`, `hashtag_max` and `link_placements`,
because they read today's UTC date and their result changes with the day, or the accuracy of any
platform's real limit (the data comes from research and is only checked for self-consistency here).

State: in-memory strings only. No network, no files, no process spawn, no clock, no user data. Run
only by the CI consumer lane.

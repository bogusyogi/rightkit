# Changelog

## Unreleased

- Hashtag maximum (Orthic-Labs/rightkit#460): `Catalog::hashtag_max` / `hashtag_max_at` read the `caption.hashtags` rule (unit `hashtags`) as a `u32`. Seeded `instagram.caption.hashtags` (30) and `instagram.caption.mentions` (20).
- Link placement (#460): `LinkPlacement` (`caption`, `first_comment`, `link_sticker`, `bio`, `none`), `LINK_PLACEMENTS`, `Catalog::link_placements` / `link_placements_at`, `Planned::link_placement`. A `link.placement` enum rule that names any other value is refused on load.
- Route dimension (#460): optional `Rule::route` (JSON `route`, absent means every route; part of the history key). `Catalog::get_in`, `Planned::on_route`. `Catalog::get` is unchanged and sees only unrestricted rules. A rule without a route keeps its 0.1.0 revision hash.
- Persisted revision id (#460): `RuleRevision::parse` and `FromStr`, `Catalog::revision_id`, `is_current_id`, `check_id`, `check_id_at`, and `Violation::BadRevisionId`.
- Seed rows promoted from PostRight p4-platform-rules-authority section 5.4 (fetched 2026-10-17): `instagram.caption.length` revision 2 (2200 characters), `instagram.reel.duration` revision 2 (3 to 900 seconds), `facebook.reel.duration` (3 to 90 seconds), `youtube.duration` (`default`, 15 minutes; `verified_google_account` unsourced), `youtube.title.length`, `youtube.description.length` (bytes), `mastodon.text.length` (`default_server`). No link-placement row: section 5.4 states no sourced value. The `youtube.duration` default row starts at revision 1 because this catalog holds no earlier revision of it.
- Not compiled or run when written; verification is the CI run. `Planned` gained a private `route` field, so build it with the constructors and `on_route`. `Rule` gains an optional `route` field, private: read it with `Rule::route()` and set it with `Rule::with_route()`. A private field also rules out struct-literal construction of `Rule` outside this crate; no literal of either type exists in the workspace.

## 0.1.0 - unreleased

- New crate (Orthic-Labs/rightkit#299, PostRight gap RG-1; second consumer: CutRight export manifest, to be confirmed in the issue). Bundled JSON catalog of per-platform rules (`assets/platform-rules.json`, seeded from PostRight's P0/P1 research), `Catalog` (`bundled`, `from_json`, `merge`, `get`, `revision`, `is_current`, `check`, `check_at`), `Rule`, `Limit`, `RuleRevision`, `Planned`, `Violation`, `CatalogError`.
- The promotion rule (`sourced` needs an official fetched source, a URL, a stated value and no conflict) and the no-edit rule (a change is the next revision) are enforced when a catalog loads or merges.
- Not compiled or run when written; verification is the CI run.

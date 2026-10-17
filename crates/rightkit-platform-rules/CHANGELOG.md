# Changelog

## 0.1.0 - unreleased

- New crate (Orthic-Labs/rightkit#299, PostRight gap RG-1; second consumer: CutRight export manifest, to be confirmed in the issue). Bundled JSON catalog of per-platform rules (`assets/platform-rules.json`, seeded from PostRight's P0/P1 research), `Catalog` (`bundled`, `from_json`, `merge`, `get`, `revision`, `is_current`, `check`, `check_at`), `Rule`, `Limit`, `RuleRevision`, `Planned`, `Violation`, `CatalogError`.
- The promotion rule (`sourced` needs an official fetched source, a URL, a stated value and no conflict) and the no-edit rule (a change is the next revision) are enforced when a catalog loads or merges.
- Not compiled or run when written; verification is the CI run.

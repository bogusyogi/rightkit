# Changelog

## 0.1.0 (unreleased)

- First version (bogusyogi/rightkit#875): `right-issues` writes a self-contained, offline `issues.html` at a repository root from that repository's GitHub issues. A "Needs <who>" section lists decisions and live checks first; issues are grouped by an explicit mapping, an `area:`/`component:`/`feature:`/`group:` label, or a title rule from `.rightkit/issue-groups.json`, else "Other". Status chips come from the labels `needs-decision`, `needs-live-run`, `built-needs-check`, `blocked` and `tracking`; every closed issue stays in the page (the default view shows open plus the last 14 days, `--closed-days`; "Done" and "All" show the history). Search, status filters, per-group counts, light and dark. Generalised from Pulse's reference tracker.
- Input hardening: unknown or valueless options fail, `gh` failures and invalid JSON (a leading byte order mark is accepted) give one-line errors, an invalid group-rule pattern names the rule, links outside http(s) never reach an href.
- Completeness: the whole inventory is fetched through paginated REST (pull requests dropped, count checked against the search total); failed or incomplete retrieval, or more issues than an explicit `--max`, exits non-zero and leaves an existing `issues.html` untouched. `--limit` is removed. Local arguments are validated before any `gh` call, and `main` takes an injectable fetcher so tests need no `gh`.
- Deterministic grouping: label prefix priority `group:` > `component:` > `feature:` > `area:`, ties by sorted label name; group names are trimmed and de-duplicated case-insensitively, first declared spelling wins.
- Registered in the ownership catalog (`issues-tracker`, declaration-only) and INDEX.md as staged, never published.

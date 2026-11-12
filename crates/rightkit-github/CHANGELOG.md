# Changelog

## 0.2.0

- Breaking for dependents: dependency requirements `rightkit-review` 0.4 -> 0.5. 0.1.1 was published before these requirements were raised; a semver-breaking dependency is a breaking change, so they ship as 0.2.0 instead of changing 0.1.1 in place (#1572). No source change in this crate since 0.1.1.

## Unreleased

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.1 (unreleased)

- Added the `observer` feature: `delivery_observer` (one bounded read-only GitHub pull-request observation normalised into a delivery assessment) and `engineering` (bounded imported-evidence reads, work-plan, review-loop and delivery assessments, diagnostic exit codes), moved from Legion's `commands/github_observer.rs` and `commands/engineering.rs`. Additive; off by default.

## 0.1.0 (unreleased)

- First RightKit release, moved from the CodeRight daemon (`github_app`, `github_signin`).

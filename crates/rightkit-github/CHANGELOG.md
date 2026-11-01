# Changelog

## 0.1.1 (unreleased)

- Added the `observer` feature: `delivery_observer` (one bounded read-only GitHub pull-request observation normalised into a delivery assessment) and `engineering` (bounded imported-evidence reads, work-plan, review-loop and delivery assessments, diagnostic exit codes), moved from Legion's `commands/github_observer.rs` and `commands/engineering.rs`. Additive; off by default.

## 0.1.0 (unreleased)

- First RightKit release, moved from the CodeRight daemon (`github_app`, `github_signin`).

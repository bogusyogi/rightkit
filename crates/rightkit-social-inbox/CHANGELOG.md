# Changelog

## 0.3.1

- Fixed: `webhook::event_secs` reads a UTC instant by the calendar's rule (`rightkit_time::zones::parse_instant`) again, so a minute-precision instant (`2026-10-20T10:00Z`, `2026-10-20T10:00+0000`) is accepted as PostRight's shipped code accepted it. The lift had narrowed it to the 20-character form.

## 0.3.0

- Breaking for dependents: dependency requirements `rightkit-social-analytics` 0.2 -> 0.3, `rightkit-social-publish` 0.1 -> 0.2. 0.2.0 was published before these requirements were raised; a semver-breaking dependency is a breaking change, so they ship as 0.3.0 instead of changing 0.2.0 in place (#1572). No source change in this crate since 0.2.0.

## 0.2.0 (unpublished)

Taken on the recommended option of the shape review (`docs/audits/2026-11-11-shape-matrix/reports/cut-post-redraw.md`), pending the owner's confirmation.

- Breaking: requires `rightkit-social-analytics` 0.2.0; `automation::AutomationStore` extends its `links::LinksStore`. No behaviour change.

## 0.1.2 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.1 (unreleased)

- Added `automation`: the `dm.*` commands (rules, rate, opt-out, process, sweep, outcomes, drafts) over a host-implemented `AutomationStore` (a `LinksStore` of `rightkit-social-analytics`) and a `Transport` for the one platform call. `keywords::words` is public. New dependencies `rightkit-social-analytics`, `rightkit-http`.
- Added `protocol` (inbox and DM-automation payload shapes; the channel `Scope` comes from `rightkit-social-publish`) and `inbox`: `inbox.receive`, `list`, `get`, `set_state`, `stats` over a host-implemented `InboxStore`. Additive. New feature `test-mode`.

## 0.1.0 (unreleased)

- First RightKit release, moved from PostRight (postright-engine inbox.rs verification and parsing, automation.rs keyword matching and event time).

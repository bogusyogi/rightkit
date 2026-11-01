# Changelog

## 0.1.1 (unreleased)

- Added `automation`: the `dm.*` commands (rules, rate, opt-out, process, sweep, outcomes, drafts) over a host-implemented `AutomationStore` (a `LinksStore` of `rightkit-social-analytics`) and a `Transport` for the one platform call. `keywords::words` is public. New dependencies `rightkit-social-analytics`, `rightkit-http`.
- Added `protocol` (inbox and DM-automation payload shapes; the channel `Scope` comes from `rightkit-social-publish`) and `inbox`: `inbox.receive`, `list`, `get`, `set_state`, `stats` over a host-implemented `InboxStore`. Additive. New feature `test-mode`.

## 0.1.0 (unreleased)

- First RightKit release, moved from PostRight (postright-engine inbox.rs verification and parsing, automation.rs keyword matching and event time).

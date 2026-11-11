# Changelog

## 0.1.0 (renamed from `rightkit-adapt` 0.1.2)

- Renamed from `rightkit-adapt` (library `rightkit_agent_learning`): this is CodeRight's post-session learning, and the `rightkit-adapt` name now holds Pulse's governed behavioural learning (`rightkit-adapt` 0.2.0, formerly `rightkit-adapt-taste`). Replace `rightkit_adapt::` with `rightkit_agent_learning::`.
- `harness_radar` (the harness watch-list feed and its `HarnessRadar*` types) moved to `rightkit-agent-server`, its only user, as a private module. Removed from this crate's API.
- Taken on the recommended option of the 2026-11-11 shape review, pending the owner's confirmation.

## rightkit-adapt 0.1.2

- Manifest only: dependency requirements and workspace metadata changed since 0.1.1 was published; no source change.

## rightkit-adapt 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## rightkit-adapt 0.1.0 (unreleased)

- First RightKit release, moved from `coderight-learning` (engine).

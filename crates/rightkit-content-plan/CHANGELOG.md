# Changelog

## 0.2.0

- Breaking for dependents: dependency requirements `rightkit-social-analytics` 0.2 -> 0.3, `rightkit-social-publish` 0.1 -> 0.2. 0.1.3 was published before these requirements were raised; a semver-breaking dependency is a breaking change, so they ship as 0.2.0 instead of changing 0.1.3 in place (#1572). No source change in this crate since 0.1.3.

## 0.1.3 (unpublished)

Taken on the recommended option of the shape review (`docs/audits/2026-11-11-shape-matrix/reports/cut-post-redraw.md`), pending the owner's confirmation.

- Requires `rightkit-social-analytics` 0.2.0 (its video-editing retention commands moved to `rightkit-autoedit`); only `links::public_https` is used, and no type of it is in this crate's API. No behaviour change.

## 0.1.2 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.1 (unreleased)

- Added `hook_test`: `hook_test.create`, `bind`, `read`, `get`, `list` over a host-implemented `HookTestStore`. Additive.
- Added `variants`: the typed-brief and variant commands over a host-implemented `VariantStore` (the platform-rule catalog is a host parameter); variant and hook-test payload shapes in `protocol`. New dependencies `rightkit-platform-rules`, `rightkit-fs`.
- Added `calendar`: the brief and calendar commands over a host-implemented `CalendarStore`; calendar payload shapes in `protocol`. Additive.
- Added `protocol` (draft payload shapes) and `draft_store`: the manual draft commands over a host-implemented `DraftStore`. Additive. New dependencies: `rightkit-social-publish` (channel `Scope`, instants, shared checks), serde, serde_json.

## 0.1.0 (unreleased)

- First RightKit release, moved from PostRight (postright-engine drafts.rs approval and link-host rules).

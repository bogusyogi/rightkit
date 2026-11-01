# Changelog

## 0.1.1 (unreleased)

- Added `hook_test`: `hook_test.create`, `bind`, `read`, `get`, `list` over a host-implemented `HookTestStore`. Additive.
- Added `variants`: the typed-brief and variant commands over a host-implemented `VariantStore` (the platform-rule catalog is a host parameter); variant and hook-test payload shapes in `protocol`. New dependencies `rightkit-platform-rules`, `rightkit-fs`.
- Added `calendar`: the brief and calendar commands over a host-implemented `CalendarStore`; calendar payload shapes in `protocol`. Additive.
- Added `protocol` (draft payload shapes) and `draft_store`: the manual draft commands over a host-implemented `DraftStore`. Additive. New dependencies: `rightkit-social-publish` (channel `Scope`, instants, shared checks), serde, serde_json.

## 0.1.0 (unreleased)

- First RightKit release, moved from PostRight (postright-engine drafts.rs approval and link-host rules).

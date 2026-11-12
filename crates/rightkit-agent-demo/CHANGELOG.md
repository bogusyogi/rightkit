# Changelog

## 0.2.0

- Breaking for dependents: dependency requirements `rightkit-automation` 0.6 -> 0.7, `rightkit-daemon` 0.5 -> 0.8, `rightkit-logs` 0.3 -> 0.4, `rightkit-tauri-shell` 0.7 -> 0.8. 0.1.0 was published before these requirements were raised; a semver-breaking dependency is a breaking change, so they ship as 0.2.0 instead of changing 0.1.0 in place (#1572). No source change in this crate since 0.1.0.

## Unreleased

- `AppRoot`, `AppRootError`, `Suite` and `mode::AppRoots` come from `rightkit-roots` (new dependency) instead of the deprecated `rightkit_daemon` paths.
- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from CutRight's `apps/cutright/src-tauri/src/demo` and `apps/demo-fixture`. The host product identity (name, slug, own app ids, version) is passed to `plugin` as a `DemoIdentity`; the host app is always on the deny list.

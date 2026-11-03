# Changelog

## Unreleased

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from CutRight's `apps/cutright/src-tauri/src/demo` and `apps/demo-fixture`. The host product identity (name, slug, own app ids, version) is passed to `plugin` as a `DemoIdentity`; the host app is always on the deny list.

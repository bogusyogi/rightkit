# Changelog

## 0.3.0

- `DEFAULT_APP_FONT` is `"system"` (was `"hanken"`), and a settings patch accepts `"system"` for `app_font`. The settings screen already offers and defaults to the system font; the Rust default disagreed, so a fresh blob named a font the screen did not show as selected, and choosing the system font was refused by the patch check (#1572).

- Breaking for dependents: dependency requirements `rightkit-logs` 0.3 -> 0.4, `rightkit-speech-client` 0.4 -> 0.5. 0.2.4 was published before these requirements were raised; a semver-breaking dependency is a breaking change, so they ship as 0.3.0 instead of changing 0.2.4 in place (#1572). No source change in this crate since 0.2.4.

## 0.2.4

- Publishes the source changes made since 0.2.3 was published under the same version number (src/engine/provider_handoff.rs).

## 0.2.3 (unpublished)

- Removed (breaking): `controller` (`DictationController`, `FakeDeliveryBackend`, `for_test`), `engine::FakeEngine`, and `app_paths::test_binary_data_root` / `app_data_root_is_explicit_override`. The controller could only be driven by `FakeEngine` and had no caller; the two stubs always returned `None` / `false`. Adrian, 2026-11-03: "My decision was no unit tests. Only e2e."; "dev mode, is mine ... real data. snapshot and restore." `rightkit-transcript-library` stays declared in Cargo.toml (now unused here; drop it with the next lock refresh).
- Removed (breaking, rightkit#1437): the `test-mode` feature, `app_paths::TEST_APP_DATA_DIR_NAME`, and the `RIGHTKIT_APP_DATA_DIR` override and the test-binary data root behind it. `app_data_root()` is always `production_data_root()` (the real per-user folder); `app_data_dir_name()` always panics when the host never configured a name. `test_binary_data_root()` and `app_data_root_is_explicit_override()` are deleted (see above). `dev_var!` / `dev_var_os!` are deleted: every caller was removed first (Adrian, 2026-11-03: "dev mode, is mine ... real data.").
- Why: Adrian, 2026-11-03: "no debug assertions"; "dev mode, is mine. im the fucking owner, my data. no dummy data, no fucking made up shit. real data. snapshot and restore." A build that enables `rightkit-dictation-core/test-mode` no longer resolves: drop the feature.

## 0.2.2 (unpublished)

- `resolve_production_data_root` no longer calls `Options::qa_overrides`, which rightkit-fs 0.4.11 removed with the test-mode residue.
- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.2.1

- Dependency: `rightkit-credentials` is a caret `0.4.3` requirement (was the exact `=0.4.3`), so the suite can move to credentials 0.4.4 (no `debug_assertions` switches). No source change.

## 0.2.0 (unreleased)

- `settings` (versioned settings document), `app_paths` (data root for a host-configured folder name, `RIGHTKIT_APP_DATA_DIR` test override), payload and diagnostic redaction, and the test-only `dev_var!` / `dev_var_os!` macros, moved from HeardRight `heardright_core`.
- `test-mode` feature.

## 0.1.0 (unreleased)

- First RightKit release, moved from HeardRight.

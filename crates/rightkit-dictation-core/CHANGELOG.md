# Changelog

## 0.3.4

- Add typed warm-up and seal frames, start-attempt correlation and validation for early capture. Expose audio conditioning through an opt-in tools binary.

## 0.3.3 (unpublished)

- Add typed last-paste casing commands in the extensible transcript reset diagnostics. Existing `DiagnosticsPayload` callers and older wire readers retain the reset and its failure diagnostic. The command names the prior delivery and requested case. It carries neither editor authority nor text.

## 0.3.2 (unpublished)

- Add `pill`: shared `PillEvent`, render-state mapping, visibility, hit-testing & event-driven hover scheduling. Serde derives, tags, variants & fields match HeardRight commit `28acb588^`; labels remain host inputs.
- Include ten source-derived JSON fixtures & wire compatibility checks. Fixtures were handwritten from serde rules; no serialization run is claimed.

## 0.3.1 (2026-11-16)

HeardRight's `heardright_core::app_paths` is compared with this crate; nothing changes for a host that makes no call below.

- New: `app_paths::set_app_data_root(PathBuf) -> bool` hands the process its app-data root once, before anything reads it (a host's `main` passes `production_data_root()`, a dev tool its temp folder). The first root stays: the same root again returns `true`, a different one `false`. `app_paths::app_data_root_is_injected()` says whether one was handed in. Without a call `app_data_root()` is `production_data_root()`, as before. Every crate that resolves the root through `app_data_root()` (telemetry, settings, the engine runtime) follows it.
- Compared and kept different: HeardRight's setter replaces the root on each call (tests in one process set it in turn) and, with no call, answers a per-process temp folder; here the first call wins and no call means the real per-user folder. A host that wants either keeps passing the root itself.

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

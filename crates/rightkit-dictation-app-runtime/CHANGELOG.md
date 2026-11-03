# Changelog

## 0.1.2 (unpublished)

- Removed (rightkit#1437): the `shared_store_smoke` example and its `[[example]]` entry. It ran the shared store against a scratch suite root through `rightkit-daemon`'s `test-mode` and `TEST_CAPABILITY`, which no longer exist (rightkit-daemon 0.5.3): a dummy root is not a test any more. Removed too: the `test-mode` feature and everything it gated: `engine_supervisor::{pass_live_eval_env_for_test, l3_env_keys_for_test, strip_l3_env_for_test, resolve_engine_override_for_test, build_sidecar_command_for_test}`; the `RIGHTKIT_ENGINE_BIN`, `RIGHTKIT_ENGINE_APP_PATH`, `RIGHTKIT_NEXT_ENGINE_ROOT`, `RIGHTKIT_AI_EVAL_*`, `RIGHTKIT_L3_LOG_PAYLOADS`, `RIGHTKIT_OWNER_DIAGNOSTICS*` and `HR_*` lever forwards; the `RIGHTKIT_SUITE_ROOT` / `RIGHT_SUITE_DIR` / `ORTHIC_SHARED_DIR` / `RIGHTKIT_APP_DATA_DIR` forwards to the sidecar; and the source-checkout sidecar lookup (`CARGO_MANIFEST_DIR`). `resolve_engine_bin` looks only next to the executable and in the macOS bundle `Resources`. Release behaviour is unchanged. A build that enables `rightkit-dictation-app-runtime/test-mode` no longer resolves: drop the feature.
- Also: Adrian, 2026-11-03: "no debug assertions".
- Why: Adrian, 2026-11-03: "dev mode, is mine. im the fucking owner, my data. no dummy data, no fucking made up shit. real data. snapshot and restore."

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `heardright-app-runtime` (tauri-app-next).

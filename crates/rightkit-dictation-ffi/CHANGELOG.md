# Changelog

## 0.2.0

- Breaking for dependents: dependency requirements `rightkit-asr` 0.4 -> 0.5, `rightkit-dictation-core` 0.2 -> 0.3, `rightkit-onnx` 0.2 -> 0.3. 0.1.0 was published before these requirements were raised; a semver-breaking dependency is a breaking change, so they ship as 0.2.0 instead of changing 0.1.0 in place (#1572). No source change in this crate since 0.1.0.

## Unreleased

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from HeardRight. C symbols renamed to `rightkit_dictation_*`.
- `kws` module: C surface for streaming keyword spotting, moved from HeardRight `heardright_core::kws_ffi`. Env switches for its asset-gated test are `RIGHTKIT_TEST_KWS_DIR`, `RIGHTKIT_TEST_ORT_DYLIB`, `RIGHTKIT_TEST_KWS_KEYWORDS`, `RIGHTKIT_TEST_KWS_WAV`, `RIGHTKIT_TEST_KWS_EXPECT`.

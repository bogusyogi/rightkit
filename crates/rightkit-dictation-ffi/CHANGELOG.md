# Changelog

## 0.2.1

- ABI delta for the HeardRight iOS adoption (no existing symbol or layout changed):
  - New `rightkit_dictation_configure_brand_homophones_json`: installs a process-wide brand homophone table that `rightkit_dictation_polish` applies (HeardRight's L0 polish repairs its product name; 0.2.0 had no way to). Without a call, polish is byte-identical to 0.2.0.
  - New cargo feature `kws` (default on): gates `kws` module & direct optional `rightkit-onnx` dependency. `default-features = false` omits `rightkit_dictation_kws_*` exports; detokenization still requires `rightkit-asr` with transitive `rightkit-onnx/session` & `ort` dependencies.
  - New source-reviewed header `include/rightkit_dictation.h` (proposed `cbindgen.toml`; reproduction unverified), shipped in the package: KWS declarations sit under `RIGHTKIT_DICTATION_KWS`, pointers carry `_Nullable`, ownership and return codes are documented. Shells use this header instead of keeping their own copy.

- Configuration validates complete owned input before retaining one table; malformed or losing concurrent attempts release temporary allocations.

## 0.2.0

- Breaking for dependents: dependency requirements `rightkit-asr` 0.4 -> 0.5, `rightkit-dictation-core` 0.2 -> 0.3, `rightkit-onnx` 0.2 -> 0.3. 0.1.0 was published before these requirements were raised; a semver-breaking dependency is a breaking change, so they ship as 0.2.0 instead of changing 0.1.0 in place (#1572). No source change in this crate since 0.1.0.

## Unreleased

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from HeardRight. C symbols renamed to `rightkit_dictation_*`.
- `kws` module: C surface for streaming keyword spotting, moved from HeardRight `heardright_core::kws_ffi`. Env switches for its asset-gated test are `RIGHTKIT_TEST_KWS_DIR`, `RIGHTKIT_TEST_ORT_DYLIB`, `RIGHTKIT_TEST_KWS_KEYWORDS`, `RIGHTKIT_TEST_KWS_WAV`, `RIGHTKIT_TEST_KWS_EXPECT`.

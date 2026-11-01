# Changelog

## 0.1.0 (unreleased)

- First RightKit release, moved from HeardRight. C symbols renamed to `rightkit_dictation_*`.
- `kws` module: C surface for streaming keyword spotting, moved from HeardRight `heardright_core::kws_ffi`. Env switches for its asset-gated test are `RIGHTKIT_TEST_KWS_DIR`, `RIGHTKIT_TEST_ORT_DYLIB`, `RIGHTKIT_TEST_KWS_KEYWORDS`, `RIGHTKIT_TEST_KWS_WAV`, `RIGHTKIT_TEST_KWS_EXPECT`.

# Changelog

All notable changes to `rightkit-tts`. Versions follow Cargo semver; while the
version is `0.x`, a minor bump may carry breaking changes.

## 0.1.0 (staged, unpublished)

After the first-release feature list below, before publication:

- Requires `rightkit-transcript` 0.2.0 (the 0.1.x line is no longer in the workspace).
- `thiserror` is now compiled only with `system` (its only user, `SystemTtsError`); `client`-only
  builds drop it.
- Model manifest hashing (`models::sha256_file`) uses `rightkit_fs::digest`; the direct `sha2`
  dependency is gone.
- New fail-closed `models::try_data_root`, `try_model_dir`, `try_voices_dir` (error type
  `DataRootError`): no temp-directory fallback and a relative base is rejected. The client and
  worker use them, so an unresolvable data root now reports `models_missing` / `voice_missing`
  instead of reading from the OS temp directory. `data_root`, `model_dir`, `voices_dir` keep their
  signatures and legacy fallback.

First crates.io release. Features:

- `system`: OS voice backend (`say` and `afconvert` on macOS, SAPI on Windows)
  rendering to an isolated WAV with a word-timed manifest.
- `client`: QTS worker client (`QtsClient`), sentence chunking, and model
  manifest verification.
- `engine`, `metal`, `vulkan`: the Qwen3-TTS worker (`rightkit-tts-worker`
  binary) and its accelerator features.

Earlier history is in the git log of this repository; this file starts at the
publication point.

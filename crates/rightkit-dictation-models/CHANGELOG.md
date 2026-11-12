# Changelog

## 0.2.0

- Breaking for dependents: dependency requirements `rightkit-dictation-core` 0.2 -> 0.3, `rightkit-dictation-telemetry` 0.1 -> 0.2, `rightkit-models` 0.5 -> 0.7. 0.1.3 was published before these requirements were raised; a semver-breaking dependency is a breaking change, so they ship as 0.2.0 instead of changing 0.1.3 in place (#1572). No source change in this crate since 0.1.3.

## 0.1.3 (unpublished)

- Fixed: the compiled-in model worker host is `heardright-models.adrdsouza.workers.dev`, the host every download URL in this crate uses. It read `rightkit-models.adrdsouza.workers.dev` after the move from HeardRight, so the production worker was treated as a development override, the one case allowed to resolve to a local or private address (`model_download_grant.rs`). Found in the HeardRight shape review (rightkit#1572).

## 0.1.2 (unpublished)

- Removed (breaking, rightkit#1437): the `test-mode` feature; the `test_support` module; `shared::reset_activation_for_tests` and `shared::activate_for_tests`; the `verify_receipt` module and the `RIGHTKIT_MODEL_VERIFY=receipt` stat-receipt policy (every launch verifies in full, as every release build already did); the `RIGHTKIT_MODEL_BASE_URL` manifest override in `model_fetch::load_manifest`; and the `KwsCandidateKind::DevSource` variant with its `CARGO_MANIFEST_DIR` source-checkout candidate. Release behaviour is unchanged. A build that enables `rightkit-dictation-models/test-mode` no longer resolves: drop the feature.
- Why: Adrian, 2026-11-03: "no debug assertions"; "dev mode, is mine. im the fucking owner, my data. no dummy data, no fucking made up shit. real data. snapshot and restore."

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `heardright-model-store` (tauri-app-next).
- `model_paths`: the bundled keyword-spotting model resolution drops the Sherpa naming: `SHERPA_KWS_SUBDIR` is `KWS_SUBDIR`, `SherpaKwsCandidate(Kind)` is `KwsCandidate(Kind)`, and `sherpa_kws_candidates`, `select_sherpa_kws_dir`, `sherpa_kws_dir`, `sherpa_kws_ready`, `sherpa_kws_missing` are `kws_candidates`, `select_kws_dir`, `kws_dir`, `kws_ready`, `kws_missing`. Resolution order and the required model files are unchanged; the telemetry message `sherpa_kws_dir_resolved` is now `kws_dir_resolved`. Keyword spotting is the Rust `rightkit_asr::kws`. Staged in the unreleased 0.1.0 section (this crate has not been published).

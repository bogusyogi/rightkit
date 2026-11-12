# Changelog

## 0.2.0

- Breaking for dependents: dependency requirements `rightkit-automation` 0.6 -> 0.7, `rightkit-crash` 0.3 -> 0.4, `rightkit-daemon` 0.5 -> 0.8, `rightkit-dictation-command-host` 0.1 -> 0.2, `rightkit-dictation-core` 0.2 -> 0.3, `rightkit-dictation-inference` 0.1 -> 0.2, `rightkit-dictation-models` 0.1 -> 0.2, `rightkit-dictation-text-pipeline` 0.1 -> 0.2, `rightkit-job-queue` 0.5 -> 0.6, `rightkit-logs` 0.3 -> 0.4, `rightkit-onnx` 0.2 -> 0.3, `rightkit-speech-client` 0.4 -> 0.5. 0.1.0 was published before these requirements were raised and before the change below; a semver-breaking dependency is a breaking change, so they ship as 0.2.0 instead of changing 0.1.0 in place (#1572).
- Speech-service peers are checked with `rightkit_daemon::verify_peer` (an Orthic `PeerPolicy`: macOS team `6KLGD3LLKF`, Windows signer `Damned Ventures LLC`) instead of this crate's own copy of the macOS and Windows signature checks; `speech_service/peer_trust.rs` keeps only the identities (388 to 44 lines). The Windows pipe-name tag is `rightkit_daemon::current_user_sid()` instead of its own token query. The `security-framework`, `core-foundation` and `windows-sys` dependencies are gone with that code. Same identities and the same fail-closed refusal; the refusal log line's reason is now `verify_peer`'s error text.

## Unreleased

- Removed the `RIGHTKIT_CALIBRATION_COLLECT_DIAGNOSTICS` environment switch. `calibrate <pcm>` takes an explicit `--collect-diagnostics` argument instead; the shell passes it when it wants diagnostics.
- Removed the test-mode environment levers (`perf_levers`, `RIGHTKIT_*` reads via `dev_var!`), the live L3 reconfigure module (`ReconfigureL3` now answers with the not-enabled error), the live-evaluation hook in finalize, the VAD auto-calibrate and developer ORT-path overrides, and the argv models-base hint; each now runs its shipped behaviour only (Adrian, 2026-11-03: "My decision was no unit tests. Only e2e."; "dev mode, is mine ... real data. snapshot and restore.").
- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `heardright-runtime` (tauri-app-next).
- Sherpa removed: keyword spotting is the Rust `rightkit_asr::kws` through `rightkit-dictation-inference`'s `kws::Kws`. The `rust-kws` cargo feature is removed; the `sherpa_kws` re-export is `kws`; `engine_paths::EnginePaths::sherpa_kws_dir` is `kws_dir`; the test-mode directory override is `RIGHTKIT_KWS_DIR` (was `RIGHTKIT_SHERPA_KWS_DIR`); the hey-wake event model label is `kws-hey-zephyr` (was `sherpa-onnx-hey-zephyr`) and the control classifier result is `control_complete_kws` (was `control_complete_sherpa`).
- Staged in the unreleased 0.1.0 section: this crate has not been published, so the removal is not a version bump.

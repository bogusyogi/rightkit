# Changelog

## Unreleased

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `heardright-engine` (tauri-app-next).
- Sherpa removed: the `rust-kws` cargo feature is removed and the `sherpa_kws` re-export is `kws` (`kws::Kws`). The `sherpa_kws_probe_replay` example is deleted; the KWS probe examples lose their `--backend` option and the `backend` column. Test-mode directory override is `RIGHTKIT_KWS_DIR`.
- Staged in the unreleased 0.1.0 section: this crate has not been published, so the removal is not a version bump.

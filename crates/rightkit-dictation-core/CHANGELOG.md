# Changelog

## 0.2.1

- Dependency: `rightkit-credentials` is a caret `0.4.3` requirement (was the exact `=0.4.3`), so the suite can move to credentials 0.4.4 (no `debug_assertions` switches). No source change.

## 0.2.0 (unreleased)

- `settings` (versioned settings document), `app_paths` (data root for a host-configured folder name, `RIGHTKIT_APP_DATA_DIR` test override), payload and diagnostic redaction, and the test-only `dev_var!` / `dev_var_os!` macros, moved from HeardRight `heardright_core`.
- `test-mode` feature.

## 0.1.0 (unreleased)

- First RightKit release, moved from HeardRight.

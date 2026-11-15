# Changelog

## 0.2.1

HeardRight's command host is compared with this crate and every difference in its output is taken; nothing changes for a host that makes no call below.

- `host::set_paste_owner_name(&'static str)` (once per process; returns `false` when already set) names the paste's owner in the undo refusal "Undo skipped: no recent `<name>` paste to undo" (default `dictation`). `host::paste_owner_name()` returns it.
- Compared and kept: HeardRight's `test_mode()` is always false in its shipped build; its bundle identifier, overlay window titles and own-process prefix are `host::HostIdentity` values the host already sets.

## 0.2.0

- Breaking for dependents: dependency requirements `rightkit-automation` 0.6 -> 0.7, `rightkit-dictation-core` 0.2 -> 0.3, `rightkit-dictation-text-pipeline` 0.1 -> 0.2, `rightkit-logs` 0.3 -> 0.4. 0.1.0 was published before these requirements were raised; a semver-breaking dependency is a breaking change, so they ship as 0.2.0 instead of changing 0.1.0 in place (#1572). No source change in this crate since 0.1.0.

## Unreleased

- Removed `test_mode()` and every test branch in delivery (clipboard, keystrokes, targets, insert); delivery always takes the real OS path (Adrian, 2026-11-03: "My decision was no unit tests. Only e2e."; "dev mode, is mine ... real data. snapshot and restore.").
- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `heardright-command-host` (tauri-app-next).

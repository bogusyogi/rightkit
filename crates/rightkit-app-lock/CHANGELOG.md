# Changelog

## Unreleased

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from MailRight (src-tauri app_lock modules).
- Added `pin_window` (Windows): a modal masked PIN entry window moved from `pulse-windows` (Pulse) `unlock_auth.rs`.
- Added Windows `hello_key`: Hello owner-key enrolment, binding and signing over an `OwnerEngine` trait, moved from the Pulse hub (`hello.rs`).

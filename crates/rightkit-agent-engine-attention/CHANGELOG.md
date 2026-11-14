# Changelog

## 0.1.3

- Taken from the CodeRight app's newer copy: attention files are written with `rightkit_fs::atomic_write` (a crash can no longer leave a half-written file; the notification-state update used a plain write before), and `lock_attention_state()` serialises the read-modify-write of the notifications and snooze files across concurrent requests. New dependency: `rightkit-fs`. Behaviour change for existing callers: none beyond the safer writes.

## 0.1.2
- Synced with CodeRight: a damaged state or notification file is set aside as `<name>.damaged-<unix seconds>` before the default is used, so a default is never saved over the only copy; a trusted-source refusal returns the authority's own status instead of a blanket 401.

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `coderight-attention` (engine).

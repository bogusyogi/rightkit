# Changelog

## Unreleased

- Added `inbox` (RK-3; additive, nothing existing changes): the agent Inbox item (finding, rule, evidence ids, proposed actions with the approval each needs, state, dedupe key, snooze-until, expiry); dedupe on check id plus sorted fact ids, where an open key never gets a second item and a dismissed, handled or expired key stays closed; a snooze that returns the item when it passes, and expiry; quiet hours in local civil time (via `rightkit-time`, with a caller-supplied zone offset function) that hold an alert and release it when the window ends; pure routing rules for "Alerts follow you" (computer in use, phone otherwise, phone after an untouched wait, urgent to both, quiet hours hold) with the two times as settings; an `InboxStore` trait with file-backed and in-memory stores. New dependency: `rightkit-time`. Written, not yet compiled. No unit tests, per the 2026-11-03 rule.

## 0.1.3

- Taken from the CodeRight app's newer copy: attention files are written with `rightkit_fs::atomic_write` (a crash can no longer leave a half-written file; the notification-state update used a plain write before), and `lock_attention_state()` serialises the read-modify-write of the notifications and snooze files across concurrent requests. New dependency: `rightkit-fs`. Behaviour change for existing callers: none beyond the safer writes.

## 0.1.2
- Synced with CodeRight: a damaged state or notification file is set aside as `<name>.damaged-<unix seconds>` before the default is used, so a default is never saved over the only copy; a trusted-source refusal returns the authority's own status instead of a blanket 401.

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `coderight-attention` (engine).

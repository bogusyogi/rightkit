# Changelog

## 0.2.0

- Breaking, so a minor release: the axum routes are no longer in the default build, and published `rightkit-agent-server` 0.1.1 requires `^0.1.0` without `server`, so a patch release would have broken its fresh builds. `rightkit-time` requirement raised to 0.1.5 (`zones::offset_at`, used by the `/attention` route and `zone_offset_secs`). `rightkit-agent-server` and `rightkit-assist` now require 0.2.0.
- `rightkit-fs` requirement relaxed from `0.5.0` to `>=0.4.15, <0.6`: this crate only calls `atomic_write`, which both have, so a host on 0.4.15 (Pulse's engine) does not gain a second `rightkit-fs`. `rightkit-agent-server` still needs 0.5.0. Written, not compiled.
- The axum routes (`attention_routes`, `AttentionRouterState`) now sit behind the new non-default feature `server`; `axum` is an optional dependency. A host that wants only `inbox`, routing and the stores no longer pulls axum. Breaking for a caller of the routes that does not enable `server` (in-tree: `rightkit-agent-server` now does; `rightkit-assist` uses only `inbox`). The route code moved to `src/server.rs`, unchanged. Written, not compiled.

- Added `inbox` (RK-3; additive, nothing existing changes): the agent Inbox item (finding, rule, evidence ids, proposed actions with the approval each needs, state, dedupe key, snooze-until, expiry); dedupe on check id plus sorted fact ids, where an open key never gets a second item and a dismissed, handled or expired key stays closed; a snooze that returns the item when it passes, and expiry; quiet hours in local civil time (via `rightkit-time`, with a caller-supplied zone offset function) that hold an alert and release it when the window ends; pure routing rules for "Alerts follow you" (computer in use, phone otherwise, phone after an untouched wait, urgent to both, quiet hours hold) with the two times as settings; an `InboxStore` trait with file-backed and in-memory stores. New dependency: `rightkit-time`. Written, not yet compiled. No unit tests, per the 2026-11-03 rule.
- `inbox` follow-up (RK-3; still unreleased and uncompiled): an item whose expiry falls inside a quiet-hours hold is delivered once after the hold and then expires, instead of expiring undelivered (`tick` now takes the routing settings and a quiet-hours release function, and reports `expiry_deferred`); quiet-hours windows are read in their own zone (`quiet_release_at`, `quiet_hours_release_at` and `Inbox::poll` now take `offset_for(zone, unix)` in place of `offset_secs_at(unix)`; none had a consumer); new optional `InboxItem` fields `confidence` (low, medium, high) and `missing_information`; new `quiet_windows_contain`.
- Added `quiet_hours_active_in_zones(workspace_root, now_seconds, offset_for)`, which honours each window's timezone through a caller-supplied offset function. `quiet_hours_active` keeps its signature and is deprecated: it reads every window as UTC. The list route still has no zone source and reads windows as UTC.
- The `/attention` list route now reads each window in its own zone through `rightkit_time::zones::offset_at` (DST-correct for UTC, Asia/Kolkata, Europe/London, America/New_York; any other zone, or a DST zone outside 2026 and 2027, falls back to UTC). New public `zone_offset_secs(zone, unix)` for use as `offset_for`, and `quiet_hours_release_for_workspace(workspace_root, now)` (when the stored quiet hours end, `None` when not in force). Needs the unreleased `rightkit-time` `zones::offset_at`. Written, not compiled.
- `PresenceState` gains `Locked` (additive; a locked screen). Routing already counts only `Available` as a computer in use, so `Locked` is not in use. Written, not compiled.

## 0.1.3

- Taken from the CodeRight app's newer copy: attention files are written with `rightkit_fs::atomic_write` (a crash can no longer leave a half-written file; the notification-state update used a plain write before), and `lock_attention_state()` serialises the read-modify-write of the notifications and snooze files across concurrent requests. New dependency: `rightkit-fs`. Behaviour change for existing callers: none beyond the safer writes.

## 0.1.2
- Synced with CodeRight: a damaged state or notification file is set aside as `<name>.damaged-<unix seconds>` before the default is used, so a default is never saved over the only copy; a trusted-source refusal returns the authority's own status instead of a blanket 401.

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `coderight-attention` (engine).

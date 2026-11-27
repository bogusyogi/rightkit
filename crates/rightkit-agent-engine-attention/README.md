# rightkit-agent-engine-attention

Agent attention inbox: unread/needs-input items, snooze, density, quiet hours and deep links as axum routes

Moved into RightKit from `coderight-attention` (engine) on 2026-11-01.

## Snooze

`POST /attention/items/{id}/snooze` takes `snoozeUntil` as an RFC 3339 timestamp (`2026-11-05T10:00:00Z`, `...+02:00`, optional fraction), `unix:<secs>` or bare epoch seconds, and answers 400 for anything else. The list and banner routes stop hiding an item once that time has passed. `snooze.json` is updated under `lock_attention_state()`, so concurrent snoozes do not lose each other.

## Inbox (`inbox` module)

The agent Inbox: what the agent found, the rule and evidence behind it, what it proposes, and where it stands. Types are `Inbox*`; none of it touches Pulse's `pulse-inbox` directory.

- **Item.** `InboxItem`: finding, rule, check id and fact ids, evidence ids, how sure the agent is (`confidence`: low, medium or high; optional) and what information is missing (`missing_information`, a list; may be empty), proposed actions (each with the approval it needs and whether it is already done), state (`Open`, `Snoozed`, `Dismissed`, `Handled`, `Expired`), dedupe key, snooze-until, expiry, and the alerts already sent.
- **Dedupe.** `dedupe_key(check_id, fact_ids)` sorts the fact ids. `raise` never adds a second open item for a key, and a key whose item was dismissed, handled or expired is not raised again.
- **Time.** All instants are Unix seconds passed in by the caller. `tick` expires items and wakes snoozed ones whose time has passed; a woken item alerts again. **Expiry never swallows a held alert:** an open item that was never delivered, and whose expiry instant fell inside a quiet-hours hold, is not expired. It is reported in `TickReport::expiry_deferred`, routing delivers it once after the hold ends, and it expires on the first tick after that delivery is recorded (an item with no reachable device stays open until one exists or the owner acts). A snoozed item is not held by quiet hours, so it expires normally. Whether an expiry fell in a hold is worked out from the quiet-hours setting as it stands when `tick` runs.
- **Quiet hours.** `quiet_release_at(now, windows, offset_for)` returns when quiet hours end, in local civil time, or `None` when they are not in force. Each window is read in its own zone: `offset_for(zone, unix)` returns that zone's UTC offset in seconds (east positive) at that instant, with `zone` the window's `timezone` or `None` when it names none (the caller's default zone). The crate has no zone database, so DST is the caller's zone database's job. Routing holds the alert until then; it is never dropped. `quiet_hours_active_in_zones` is the matching yes/no for the stored setting; the older `quiet_hours_active` reads every window as UTC and is deprecated.
- **Routing.** `route_item` is a pure function of the item, each device's last real input and presence, and `RoutingSettings` (2 minutes of recent input and 3 minutes to the phone by default). It says which devices get the alert now and when the phone is due.
- **Storage.** `InboxStore::update` is one atomic read-modify-write. `FileInboxStore` writes `<state-dir>/attention/inbox.json` with the crate's lock and atomic write; `MemoryInboxStore` suits hosts that persist elsewhere. `Inbox<S>` wraps a store: `raise`, `dismiss`, `mark_handled`, `snooze`, `record_delivery`, `poll`.

The host sends the alert, then calls `record_delivery`; an alert whose send failed is offered again on the next `poll`.

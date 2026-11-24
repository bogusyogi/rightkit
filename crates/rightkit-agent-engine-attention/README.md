# rightkit-agent-engine-attention

Agent attention inbox: unread/needs-input items, snooze, density, quiet hours and deep links as axum routes

Moved into RightKit from `coderight-attention` (engine) on 2026-11-01.

## Inbox (`inbox` module)

The agent Inbox: what the agent found, the rule and evidence behind it, what it proposes, and where it stands. Types are `Inbox*`; none of it touches Pulse's `pulse-inbox` directory.

- **Item.** `InboxItem`: finding, rule, check id and fact ids, evidence ids, proposed actions (each with the approval it needs and whether it is already done), state (`Open`, `Snoozed`, `Dismissed`, `Handled`, `Expired`), dedupe key, snooze-until, expiry, and the alerts already sent.
- **Dedupe.** `dedupe_key(check_id, fact_ids)` sorts the fact ids. `raise` never adds a second open item for a key, and a key whose item was dismissed, handled or expired is not raised again.
- **Time.** All instants are Unix seconds passed in by the caller. `tick` expires items and wakes snoozed ones whose time has passed; a woken item alerts again.
- **Quiet hours.** `quiet_release_at(now, windows, offset_secs_at)` returns when quiet hours end, in local civil time, or `None` when they are not in force. The caller supplies the zone offset function, so DST is the caller's zone database's job. Routing holds the alert until then; it is never dropped.
- **Routing.** `route_item` is a pure function of the item, each device's last real input and presence, and `RoutingSettings` (2 minutes of recent input and 3 minutes to the phone by default). It says which devices get the alert now and when the phone is due.
- **Storage.** `InboxStore::update` is one atomic read-modify-write. `FileInboxStore` writes `<state-dir>/attention/inbox.json` with the crate's lock and atomic write; `MemoryInboxStore` suits hosts that persist elsewhere. `Inbox<S>` wraps a store: `raise`, `dismiss`, `mark_handled`, `snooze`, `record_delivery`, `poll`.

The host sends the alert, then calls `record_delivery`; an alert whose send failed is offered again on the next `poll`.

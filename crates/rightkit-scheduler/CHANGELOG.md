# Changelog

## 0.3.1

- Schedule rows (ME-4: schedules sync as a family; additive). Module `rows`: `ScheduleRow` (id, label, `ScheduleSpec`, `MisfirePolicy`, the run times shown, their digest, `created_ms`, `confirmed_ms`, `updated_ms`; serde, camelCase) is the neutral item, and `init_rows_table(conn)`, `list_rows(conn)`, `get_row(conn, id)`, `upsert_row(conn, &row) -> Result<bool>` (whether anything changed; an existing row keeps the older `created_ms`) and `delete_row(conn, id) -> Result<bool>` (also clears the runner's cursor and claims for the id) work on the `schedules` table a host already keeps, with the same columns and words (`skip`, `coalesce`, `catch_up`). `init_rows_table` adds `updated_ms`, the version a synced copy is ordered by, to a table made without it and fills it from `confirmed_ms` or `created_ms`; a host must write through `upsert_row` (or set `updated_ms` itself) for the version to move. A row carries the owner's confirmation, so a confirmed schedule is confirmed on his other devices once it syncs: safe only behind end-to-end encryption between his own devices. A stored spec or misfire word that cannot be read is an error, not skipped. Written, not compiled. No unit tests, per the 2026-11-03 rule.
- Feature `iana-full` (off by default; additive): every IANA zone from a bundled time zone database. The default still knows `UTC`, fixed `UTC+HH:MM` offsets and the eleven named zones in `timezone.rs`; with the feature a name outside them is looked up in `jiff`'s bundled database (`normalize_time_zone` returns its spelling, `time_zone_offset_minutes_at` its offset), in one file, `timezone_full.rs`. Needs `jiff` on RightKit's dependency allowlist, an open owner decision (it is in the workspace lock already but allowlisted only for one app). `rightkit-time`'s own zone table is four zones and has no database; it is not changed. Written, not compiled.

- Additive (RK-10, blueprint AG-7): `runner`, the schedule runner as a library on SQLite (`rightkit-db`, new dependency). `claim_due` / `claim_schedule` claim each schedule minute exactly once, keyed on schedule id + UTC minute (primary key, taken with the schedule's cursor in one `BEGIN IMMEDIATE` transaction). Per-schedule misfire policy through the existing `MisfirePolicy` (`Skip`; `Coalesce` = run once for all missed; `CatchUp` = run each). A cron time in a clock gap runs once after the gap; a repeated (overlap) time runs once. Offsets come from `rightkit_time::zones::offset_at`, so named zones are limited to its tables (README). A `confirmed` flag is required: an unconfirmed schedule is never claimed. `init_claim_tables` creates the two tables. Nothing existing changed in behaviour (`cron_matches_in_time_zone` now calls the new crate-private `cron_matches_local_minute`).

## 0.3.0

- Depends on `rightkit-agent-engine-protocol` 0.4.0 (additive events and fields, see its changelog). Breaking for dependents: the crate exposes protocol types in its public API, so those types are now the 0.4 ones.
- Synced with CodeRight's schedule model and time code (`bins/coderight/src/scheduler/{model,time}.rs`): `ScheduleSpec::validate` refuses an interval under `MIN_INTERVAL_SECONDS` (60; the runner ticks once a minute, R51) and an interval without a start time; `next_after` returns `None` for an interval without a stored start instead of inventing one from the query time (bug fix: the invented start moved with every query, so the interval never became due). Breaking for hosts that created sub-minute or start-less intervals.
- Cron matching follows standard cron for the day fields (bug fix): when both day-of-month and day-of-week are restricted either one matching is enough, otherwise both must match. Day-of-week accepts `7` for Sunday and `SUN`-`SAT`, month accepts `JAN`-`DEC`.
- No host wiring was needed: CodeRight's copy re-exports the profile types from its `api_schedules` module and takes time zone code from its `schedule_cron_tool`; this crate already holds both (`profile`, `timezone`), so CodeRight swaps those imports for `rightkit_scheduler::{BotProfileV1, ScheduleLifecycleState, normalize_time_zone, time_zone_offset_minutes_at}`.

## 0.2.0

- Breaking for dependents: a dependency requirement was raised to a semver-breaking version.

## 0.1.2 (unpublished)

- Requires `rightkit-agent-engine-protocol` 0.2.0, whose approval records carry the approvals core's `Binding` (mandatory digest) instead of an optional `payload_digest`. Decision 4 of the 2026-11-11 shape review (docs/audits/2026-11-11-shape-matrix/README.md, "Agent engine") taken on the recommended option, pending the owner's confirmation; design note: docs/audits/2026-11-11-shape-matrix/reports/agent-engine-contract-design.md.

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from the CodeRight daemon scheduler model and bot profile types.

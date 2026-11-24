# rightkit-scheduler

Agent schedule model: bot profiles and lifecycle types, schedule specs (cron, once, interval, monitor) with legacy parsing, due-minute computation in IANA time zones, and time zone normalisation.

Moved into RightKit from the CodeRight daemon (`scheduler/model.rs`, `scheduler/time.rs`, `api_schedules.rs` types) on 2026-11-01. The daemon runner that claims and fires schedules ships in `rightkit-agent-server`.

## Runner (`runner`)

A runner library on SQLite (through `rightkit-db`). It decides and claims; the host fires the work for each returned `Claim`.

- `init_claim_tables(&conn)` creates `schedule_claims` (primary key `(schedule_id, minute)`) and `schedule_cursor`.
- `claim_due(&mut conn, &schedules, now_ms)` claims every due minute, one transaction per schedule; a schedule that errors is reported in `TickOutcome::errors` and rolled back.
- A schedule must have `confirmed: true`; nothing is claimed for an unconfirmed schedule, and its cursor does not move. The first confirmed tick only sets the cursor, so nothing from before confirmation is replayed.
- Misfire (`MisfirePolicy`): `Skip` drops minutes older than `ON_TIME_GRACE_MINUTES`; `Coalesce` runs once for all missed minutes (on the latest); `CatchUp` runs each. One tick looks back at most `MAX_CATCHUP_MINUTES`.
- Clock changes: a cron time inside a spring-forward gap runs once, at the first minute after the gap; a time that occurs twice in a fall-back overlap runs once, at its first occurrence.

LIMIT: the runner takes zone offsets from `rightkit_time::zones::offset_at`, whose tables cover only `UTC`, `Asia/Kolkata`, `Europe/London` and `America/New_York` (the two DST zones for 2026 and 2027 only), until the jiff decision. Fixed `UTC+HH:MM` zones always work. Any other named zone, or a DST zone outside 2026 and 2027, is an error for that schedule, never a guess. The older `time_zone_offset_minutes_at` (twelve zones, computed rules) is still used by `cron_matches_in_time_zone` and `next_at`, not by the runner.


# Changelog

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

# rightkit-scheduler

Agent schedule model: bot profiles and lifecycle types, schedule specs (cron, once, interval, monitor) with legacy parsing, due-minute computation in IANA time zones, and time zone normalisation.

Moved into RightKit from the CodeRight daemon (`scheduler/model.rs`, `scheduler/time.rs`, `api_schedules.rs` types) on 2026-11-01. The daemon runner that claims and fires schedules ships in `rightkit-agent-server`.

# Changelog

## 0.1.0 (unpublished)

- New crate: Pulse's durable task journal (`engine/src/journal/mod.rs`, `worker.rs`, `schema.rs`, `link_replay.rs`), moved out of Pulse. A `JobStore` at a caller-chosen path with the task rows, the `pulse_task_events` outbox, `pulse_journal_meta` and `pulse_link_requests` (table names unchanged, so an existing journal opens as it is; schema version 2), owner epoch fencing, restart recovery, the cancel and drain rules, the retention sweeps, and the four lane workers (`JournalService`).
- The product is behind `JournalHost`: logging, the event bus, the kind table (`KindFacts`), per-kind submit checks, the lost-run receipt, the per-origin queue limit, the device-bound task hooks and the finished-task hook. The task kinds, their executors and the device-link transport stay in the application.
- Device-link replay uses the crate's own `LinkEntry`, `LinkOutcome`, `LinkRecord` and `LinkSubmit`; the application maps them to its transport's types.
- Written, not compiled.

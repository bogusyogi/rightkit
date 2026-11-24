# rightkit-agent-task-journal

The durable agent task journal. One `rightkit_job_queue::JobStore` (SQLite, full durability) holds one row per task: the job id is the task id, the submitter's idempotency key is the store's unique `request_id`, and the submitted envelope, kind and params are the row's request. The same database holds the lifecycle event outbox, the schema, owner epoch, admission and publish-cursor metadata, and the device-link request replay memory, all written in the same transactions as the task rows.

- Acceptance: the commit of the `jobs` row with its `task.accepted` event; a repeated idempotency key or utterance resolves to the original task.
- Fencing: an owner epoch per process (bumped at every open) and the job store's per-task `attempt_gen` (bumped at every lease, takeover, retry, recovery and pre-execution cancel); a write from a superseded holder is rejected as `stale_attempt`.
- Restart: a task found `running` from an older epoch becomes `uncertain`, never silently re-run; the host's kind table decides what is retried, settled `failed` (`interrupted`) or left for the owner.
- Workers: four lanes (`Lane`) over one store and one gate, started and stopped by `JournalService`.
- Events: published to the host after commit, at-least-once, each payload carrying its `seq`.

The product is the caller's: implement `JournalHost` (logging, event bus, kind table, submit checks, lost-run receipt, finished-task hook) and pass an `Executor`. The task contract types are `rightkit-agent-task-contract`'s.

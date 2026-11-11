# rightkit-agent-engine

The agent engine as one crate: a facade over the turn loop and the task contract.

* `core`: `rightkit-agent-engine-core`, the turn loop (tool dispatch, permission gate and approval flow,
  compaction, goals, guardian).
* `protocol`: `rightkit-agent-engine-protocol`, the events, envelopes, tool calls and approval records.
* `storage`: `rightkit-agent-engine-storage`, the SQLite session journal.
* `contract`: `rightkit-agent-task-contract`, the task envelope, statuses, authority and approval binding. Its
  modules (`task`, `approval`, `device`, `effect`, `intent`, `principal`, `voice`) and records are also at the
  crate root.

The turn loop is CodeRight's; the task contract is Pulse's. A tool call that waits for approval carries the
approvals core's `Binding` (digest of the exact arguments, destination, expiry, revision), and an external
effect whose result was lost maps to the contract's `TaskStatus::Uncertain`.

An application that needs only the records (no loop, no storage) depends on `rightkit-agent-task-contract`.

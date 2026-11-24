# Changelog

## 0.1.0 (unreleased)

- First version (RK-2, the noticing loop; blueprint 2026-11-24). Pure logic and storage traits: no network, no model calls, no clock reads.
  - `signal`: one signal per event, unique on (source, account, message id), states `new -> sorted -> triaged | no_work | triage_failed`.
  - `sort`: the owner's sender lists and mail headers settle travel, customer, bill and bulk; bulk becomes `no_work` with no model call; a sender whose authentication failed is not matched against the lists.
  - `markup`, `booking`: a bounded HTML reader and structured booking extraction (schema.org hotel and flight reservations from JSON-LD and microdata): dates, places, reference.
  - `triage`: the request for a tools-off model (allowlisted headers and up to 8 KB of plain text inside an escaped untrusted wrapper), the output JSON schema, a strict validator, one retry then `triage_failed`. Output can only be typed facts.
  - `facts`: typed facts (booking, party, commitment, money) with provenance (`owner`, `known_sender`, `untrusted`) and the source message id; a date is a local date plus a zone.
  - `checks`: run after each fact insert; first check `trip.dates`, producing `NewInboxItem` findings for the attention Inbox. Finding text is built from dates only.
  - `instruction`: standing instructions (trigger, condition, action, revision), inert until owner-approved and voided by a revision; occurrences keyed by a digest of instruction, revision and trigger, queued once or skipped, never overlapped.
  - `state`: `AssistState` behind the `AssistStore` trait, with `MemoryAssistStore`.
  - `pipeline`: `prepare`, `apply_answer` and `replay` for one signal.
  - The triage output shape is written once, as const field tables in `triage`: `output_schema()` renders them as JSON Schema and `validate()` walks the same tables. No schema-derivation crate is a dependency, so there is no second hand-kept copy.
- Depends on `rightkit-agent-engine-attention` (the unreleased `inbox` module), `rightkit-fs` (`digest` feature), `rightkit-time`, `serde`, `serde_json`. No new external dependency.
- Written, not compiled. No unit tests, per the 2026-11-03 rule.

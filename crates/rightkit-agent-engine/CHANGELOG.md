# Changelog

## 0.2.0 (unpublished)

- The crate is now the facade of the one agent engine: it re-exports `rightkit-agent-engine-core` as `core`,
  `rightkit-agent-engine-protocol` 0.2.0 as `protocol`, `rightkit-agent-engine-storage` as `storage` and the new
  `rightkit-agent-task-contract` 0.1.0 as `contract`. Decision 4 of the 2026-11-11 shape review
  (docs/audits/2026-11-11-shape-matrix/README.md, "Agent engine") taken on the recommended option, pending the
  owner's confirmation; design note: docs/audits/2026-11-11-shape-matrix/reports/agent-engine-contract-design.md.
- The task contract this crate held moved to `rightkit-agent-task-contract`; its modules and records are
  re-exported at the root, so `rightkit_agent_engine::task::TaskStatus` and `rightkit_agent_engine::EffectClass`
  still resolve. Breaking, as there: `ApprovalBinding` carries a core `Binding`; `VoiceHandoff::to_task_envelope`
  takes caller-supplied `VoiceLabels` (`VOICE_CHAT` and `VOICE_POLICY_REF` are gone); `task_cli` is behind the
  `cli` feature and `TaskHost` gains `voice_labels()` and `submitter_chat()`.
- Depending on this crate now builds the turn loop and storage; an application that needs only the records
  depends on `rightkit-agent-task-contract`.

## 0.1.1 (unpublished)

- `ApprovalBinding::core_binding`: the binding as the `rightkit-approvals` 0.2.0 core's `Binding` (arguments digest, destination, expiry, revision), the record every approval store and rule uses. Decision 3 of the 2026-11-11 shape review (docs/audits/2026-11-11-shape-matrix/README.md, "Approvals") taken on the recommended option, pending the owner's confirmation; design note: docs/audits/2026-11-11-shape-matrix/reports/approvals-core-design.md. Additive; the record itself is unchanged until the agent-engine merge (decision 4).

## Unreleased

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `pulse-core` (Pulse), the `agent` module.
- Added `task_cli`: the task command line (submit, ask, act, status, result, cancel, events) over a `TaskHost` trait, moved from `pulse-core` `task_cmd.rs`.

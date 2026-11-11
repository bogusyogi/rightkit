# Changelog

## 0.1.1 (unpublished)

- `ApprovalBinding::core_binding`: the binding as the `rightkit-approvals` 0.2.0 core's `Binding` (arguments digest, destination, expiry, revision), the record every approval store and rule uses. Decision 3 of the 2026-11-11 shape review (docs/audits/2026-11-11-shape-matrix/README.md, "Approvals") taken on the recommended option, pending the owner's confirmation; design note: docs/audits/2026-11-11-shape-matrix/reports/approvals-core-design.md. Additive; the record itself is unchanged until the agent-engine merge (decision 4).

## Unreleased

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `pulse-core` (Pulse), the `agent` module.
- Added `task_cli`: the task command line (submit, ask, act, status, result, cancel, events) over a `TaskHost` trait, moved from `pulse-core` `task_cmd.rs`.

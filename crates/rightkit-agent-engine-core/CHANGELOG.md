# Changelog

## 0.1.1 (unpublished)

- A tool call that waits for approval is bound by the approvals core's rule: its `PendingApprovalContext` carries
  a mandatory `rightkit_approvals::Binding` (the effect payload digest of the exact final payload, and the
  destination: the call's network host, else the tool) built with `rightkit_agent_engine_protocol::effect_binding`,
  instead of an optional `payload_digest`. Requires `rightkit-agent-engine-protocol` 0.2.0. Decision 4 of the
  2026-11-11 shape review (docs/audits/2026-11-11-shape-matrix/README.md, "Agent engine") taken on the
  recommended option, pending the owner's confirmation; design note:
  docs/audits/2026-11-11-shape-matrix/reports/agent-engine-contract-design.md.

## Unreleased

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `coderight-engine` (CodeRight).
- Skills come from the host: skill triggers, hints and effect enforcement take the skills a host-installed `rightkit_agent_host_traits::SkillCatalog` reports as triggered; with no catalog installed no skill triggers.
- Synced with CodeRight: approvals wait with no timer and are denied at once when no human client is attached (`Conduit::subscribe_human`, `unattended` / `detached` / `cancelled` reasons); the graduated stall ladder is replaced by a `repeated_call` Ask on the fourth identical call; `ToolPolicyEvent` records for hook blocks and sandbox refusals; session-scoped history snapshots before write tools; `PreModelRequest` / `PostModelRequest` / `PostCompact` / `Interrupt` / `PermissionRequest` hooks; `tool_search` no longer taints content provenance; test-file protection spares new files; auto-approval cache removed; bounded gate and verifier text; `SessionEngine::set_os_sandbox_enforced`; rewind keeps the removed tail for redo.

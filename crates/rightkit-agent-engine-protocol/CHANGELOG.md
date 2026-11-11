# Changelog

## 0.2.0 (unpublished)

- One approval record: `PendingApprovalContext` carries the approvals core's `rightkit_approvals::Binding`
  (`binding`, mandatory: the digest of the exact final payload, the destination, the expiry, the revision) instead
  of the optional `payload_digest` ("optional for back-compat"). `PendingApprovalContext::payload_digest()` gives
  the `sha256:<hex>` key session grants use; `effect_binding(digest, destination)` builds the binding.
  `PermissionGateRequest` carries the same `binding`. Breaking: both records' wire form has a mandatory `binding`
  object. Decision 4 of the 2026-11-11 shape review (docs/audits/2026-11-11-shape-matrix/README.md, "Agent
  engine") taken on the recommended option, pending the owner's confirmation; design note:
  docs/audits/2026-11-11-shape-matrix/reports/agent-engine-contract-design.md.
- The protocol uses the task contract for authority: `task_contract` re-exports `rightkit-agent-task-contract`
  0.1.0, and `ExternalEffectSettlement::task_status` maps a settlement to the contract's `TaskStatus`
  (`InterruptedUnknown` is `Uncertain`; `NotStarted` is none). `ApprovalCoreBinding` re-exports the core
  `Binding`.

## 0.1.2
- `ProtocolEvent::ToolApprovalResolved` gains `reason: Option<String>` (`unattended`, `detached`, `cancelled` or `user` on a denial; omitted from the wire when `None`).
- New `ProtocolEvent::ToolPolicyEvent` (`toolPolicyEvent`): a stored policy fact about one tool call that is not an approval card (`grant_installed`, `hook_block` or `sandbox_refusal`), with `policy_source` and a redacted `cause`. It carries the common event fields and correlates by `tool_call_id`.

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `coderight-protocol` (engine).

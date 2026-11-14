# Changelog

## Unreleased

- Ported from the CodeRight app's newer copy (drop-in alignment):
  - `AssistantMessagePhase` (`interim` | `final`) and the `phase` field of `ProtocolEvent::AssistantMessageCompleted` (wire `phase`: `"interim"`/`"final"`; a stored row without it reads as `final`).
  - `MessageOrigin { provider, model }` and the optional `origin` of `TranscriptMessage::Assistant` (omitted from the wire when `None`; absent reads as `None`).
  - `neutralize_boundary_tags(content, tag)` (public, the tag is a parameter) is now applied to the body inside the untrusted-content fence, so content cannot close or reopen the fence.
  - Behaviour change: `ReviewDecision::is_approved` is false for a `NetworkPolicyAmendment` whose action is `Deny` (was true for any amendment).
  - Source breaking for code that builds these values by struct literal: `TranscriptMessage::Assistant` needs `origin`, `AssistantMessageCompleted` needs `phase`. Matches with `..` and the wire are unaffected.
  - `secret_scan` keeps this crate's matching (same behaviour as the app's, written so no literal token prefix sits in the source).
- Host-configurable fence tag: `set_untrusted_context_tag(tag)` (once per process, before content is wrapped), `untrusted_context_tag()`, `DEFAULT_UNTRUSTED_CONTEXT_TAG` (`rightkit-untrusted-context`, the default, unchanged output), and `TrustEnvelope::boundary_wrapped_content_with_tag`. The writer and the neutraliser both use the configured tag.

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

# Changelog

## 0.4.0

Ported from CodeRight's newer copy so it can use this crate instead of its own. Every change is additive on the wire: a value written by an earlier version still reads, and the default output of a host that sets none of the options below is unchanged.

- Approvals, both shapes read: `PendingApprovalContext` gains the optional `payload_digest` and `effects` (`ApprovalEffects`), `arguments_json`, `requested_at`, `reason`, `policy_source`, `proposed_change`, plus the types `ApprovalFact`, `ApprovalFactValue`, `ApprovalEffects`. Hosts of the binding shape (RightKit) write `binding` and leave the new fields unset (not written); hosts of the flat shape (CodeRight) write `payload_digest` and the new fields and leave `binding` empty. `PendingApprovalContext::payload_digest()` returns the binding's digest, else the stored `payload_digest`. `PendingApprovalContext` now derives `Default`, so build it with `..Default::default()`. Source breaking for code that builds it by struct literal.
- Wire shape option: `set_wire_shape(WireShape::Flat)` (once at start-up; default `WireShape::Standard` = unchanged output). Under `Flat` an empty `binding` is left out of `PermissionGateRequest` and `PendingApprovalContext`, and roster cost fields (`provider`, `costUsd`, `totalCostUsd`, `costKnown`) are always written (`null` means unknown). `wire_shape()` reads it. Reading never depends on it.
- New events with CodeRight's serde names and shapes: `ProtocolEvent::ToolArgumentsPartial` (`toolArgumentsPartial`, live-only: a live projection, not stored), `ProtocolEvent::SubAgentOutput` (`subAgentOutput`), and `RateLimitObserved` (the optional `rateLimit` of `TurnUsage`). New optional fields, left out when unset: `effects` on `ToolApprovalRequested`, `cost_billed` on `UsageUpdated` (left out when false), `ttft_ms`, `total_ms`, `rate_limit`, `cache_ttl_ms` on `TurnUsage`, `tool_call_id` on `SubAgentSpawned` and `SubAgentCompleted`. Source breaking for exhaustive matches over `ProtocolEvent` and for struct-variant literals; matches with `..` and the wire are unaffected.
- Roster: `AgentRosterRow` gains `provider` and `cost_usd` (optional, read as `None` when absent), new `AgentRosterMain` and `roster_total_cost`, and `AgentRosterUpdated` gains `main`, `total_cost_usd`, `cost_known` (see the wire shape option for how they are written).
- `ArtifactVerdict::confidence: Option<f64>` (read as `None` when absent, not written when `None`). Source breaking for struct literals.
- Secret scanning: values after weaker key-like names (`KEY`, `AUTH`, `PASS`, `PWD`, `SIGNATURE`, `COOKIE`) are redacted as `ASSIGNED` only when they also look random (24+ characters, two character classes, entropy 3.5 bits or more); plain settings such as `key: name` stay. Behaviour change for text that contained such a random value after a weak name.
- New module `tool_step`: plain-words facts about a stored tool call (`tool_step_label`, `tool_step_kind`, `step_command`, `step_exit_code`, `rfc3339_to_unix_ms`, `step_duration_ms`, ...).
- Fence header style option: `set_untrusted_context_header_style(HeaderStyle::Short)` (once per process, before content is wrapped; default `HeaderStyle::Full` = the output of 0.3.0, unchanged). `Short` writes `<{tag} source="..." trust="..." instructions_are_data="true">`, the body and `</{tag}>` with no `content_hash`, authority attributes or preamble, byte for byte what CodeRight emits with its tag. `untrusted_context_header_style()` reads it.
- `ToolArgumentsPartial` and `SubAgentOutput` are handled by the durability and the `seq`/`session_id`/`created_at` accessors.

## 0.3.0

- `PermissionGateRequest::binding` and `PendingApprovalContext::binding` read as an empty binding when absent (`#[serde(default)]`), so records written by an app's own protocol copy (CodeRight, which sends no binding) still read. Writing is unchanged.

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

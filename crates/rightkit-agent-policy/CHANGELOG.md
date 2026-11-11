# Changelog

## 0.1.3 (unpublished)

- `PayloadDigest::of` is the `rightkit-approvals` 0.2.0 core's `bind_of` over the tool name and the payload, byte for byte the digest it computed itself (each part length-prefixed, SHA-256, lowercase hex); the private copy is deleted. Decision 3 of the 2026-11-11 shape review (docs/audits/2026-11-11-shape-matrix/README.md, "Approvals") taken on the recommended option, pending the owner's confirmation; design note: docs/audits/2026-11-11-shape-matrix/reports/approvals-core-design.md.
- Requires `rightkit-agent-engine-protocol` 0.2.0, whose approval records carry the approvals core's `Binding` (mandatory digest) instead of an optional `payload_digest`. Decision 4 of the 2026-11-11 shape review (docs/audits/2026-11-11-shape-matrix/README.md, "Agent engine") taken on the recommended option, pending the owner's confirmation; design note: docs/audits/2026-11-11-shape-matrix/reports/agent-engine-contract-design.md.

## 0.1.2

- Publishes the source changes made since 0.1.1 was published under the same version number (src/capabilities.rs, src/denial_tracking.rs, src/engine.rs, src/lib.rs, src/provenance.rs, src/shell_command.rs, src/trust.rs).

## 0.1.1 (unpublished)

- `parse_capability`: `list_mcp_sources` is a read-class tool like `list_mcp_tools` (rightkit#1245), so the new tool is not treated as an unknown, gated class.
- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- Synced with CodeRight: literal session command grants (`Rule::session_command_grant`), `EffectClass` / `ApprovalClass` / `effect_class_for_tool`, `TrustPolicy::bypass_refusal`, `evaluate_shell_command_in` / `classify_shell_command` (interpreters and test runners are routine under an enforcing OS sandbox), memory writes from any non-session origin ask, an ABAC Allow no longer relaxes the untrusted-workspace clamp; the auto-approval classifier and cache are removed and `transcribe` is no longer a media capability alias.
- Feature `legion`: effect adapter and authorization receipts binding agent policy decisions to sandbox effects, moved from CodeRight `coderight-legion-policy`.
- First RightKit release, moved from `coderight-policy` (engine).

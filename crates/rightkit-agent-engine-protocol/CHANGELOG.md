# Changelog

## Unreleased

- `ProtocolEvent::ToolApprovalResolved` gains `reason: Option<String>` (`unattended`, `detached`, `cancelled` or `user` on a denial; omitted from the wire when `None`).
- New `ProtocolEvent::ToolPolicyEvent` (`toolPolicyEvent`): a stored policy fact about one tool call that is not an approval card (`grant_installed`, `hook_block` or `sandbox_refusal`), with `policy_source` and a redacted `cause`. It carries the common event fields and correlates by `tool_call_id`.

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `coderight-protocol` (engine).

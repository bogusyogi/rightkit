# Changelog

## 0.1.0 (unreleased)

- First RightKit release, moved from `coderight-engine` (CodeRight).
- Skills come from the host: skill triggers, hints and effect enforcement take the skills a host-installed `rightkit_agent_host_traits::SkillCatalog` reports as triggered; with no catalog installed no skill triggers.
- Synced with CodeRight: approvals wait with no timer and are denied at once when no human client is attached (`Conduit::subscribe_human`, `unattended` / `detached` / `cancelled` reasons); the graduated stall ladder is replaced by a `repeated_call` Ask on the fourth identical call; `ToolPolicyEvent` records for hook blocks and sandbox refusals; session-scoped history snapshots before write tools; `PreModelRequest` / `PostModelRequest` / `PostCompact` / `Interrupt` / `PermissionRequest` hooks; `tool_search` no longer taints content provenance; test-file protection spares new files; auto-approval cache removed; bounded gate and verifier text; `SessionEngine::set_os_sandbox_enforced`; rewind keeps the removed tail for redo.

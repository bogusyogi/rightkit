# Changelog

## 0.1.3 (2026-11-16)

- Breaking for a host outside this repository that took `default-features = false` and relied on the implied OS keyring: the `rightkit-mcp` dependency no longer names `agent_mcp-os-keyring`, so a build without `host` no longer links the OS credential store (`rightkit-credentials`). New feature `os-keyring` (`rightkit-agent-coding-tools/os-keyring`, `rightkit-mcp/agent_mcp-os-keyring`, `rightkit-hooks/agent_hooks-os-keyring`); `host` implies it, so a default-features user is unchanged.
- Requires rightkit-agent-coding-tools 0.1.8 and rightkit-hooks 0.2.11.

## 0.1.2 (2026-11-16)

- Requires rightkit-mcp 0.10.0. Names `agent_mcp-os-keyring` explicitly, so the build is unchanged. No behaviour change.
- The package no longer ships `src/steering.rs` (the live steering queue is `api_sessions/steering.rs` in rightkit-agent-server) and `src/evidence_replay.rs`, neither of which `src/lib.rs` declared nor anything called. No public item, feature or behaviour changes; the compiled library is unchanged.

## 0.1.1 (unpublished)

- Requires `rightkit-mcp` 0.9.0 (agent MCP server options, bearer config, prompts and the CodeRight sync; see its changelog).
- Builds against `rightkit-agent-engine-protocol` 0.4.0: the events and approval records it emits leave the new optional fields unset (`effects`, `cost_billed`, `ttft_ms`, `total_ms`, `rate_limit`, `cache_ttl_ms`, `PendingApprovalContext` flat-shape fields), and `seq_of` handles `ToolArgumentsPartial` and `SubAgentOutput`. No behaviour change.

- A tool call that waits for approval is bound by the approvals core's rule: its `PendingApprovalContext` carries
  a mandatory `rightkit_approvals::Binding` (the effect payload digest of the exact final payload, and the
  destination: the call's network host, else the tool) built with `rightkit_agent_engine_protocol::effect_binding`,
  instead of an optional `payload_digest`. Requires `rightkit-agent-engine-protocol` 0.2.0. Decision 4 of the
  2026-11-11 shape review (docs/audits/2026-11-11-shape-matrix/README.md, "Agent engine") taken on the
  recommended option, pending the owner's confirmation; design note:
  docs/audits/2026-11-11-shape-matrix/reports/agent-engine-contract-design.md.

## Unreleased

- Builds against the protocol's new `origin` and `phase` fields: the assistant messages and completion events the engine emits carry `origin: None` and `phase: final`. No behaviour change.

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `coderight-engine` (CodeRight).
- Skills come from the host: skill triggers, hints and effect enforcement take the skills a host-installed `rightkit_agent_host_traits::SkillCatalog` reports as triggered; with no catalog installed no skill triggers.
- Synced with CodeRight: approvals wait with no timer and are denied at once when no human client is attached (`Conduit::subscribe_human`, `unattended` / `detached` / `cancelled` reasons); the graduated stall ladder is replaced by a `repeated_call` Ask on the fourth identical call; `ToolPolicyEvent` records for hook blocks and sandbox refusals; session-scoped history snapshots before write tools; `PreModelRequest` / `PostModelRequest` / `PostCompact` / `Interrupt` / `PermissionRequest` hooks; `tool_search` no longer taints content provenance; test-file protection spares new files; auto-approval cache removed; bounded gate and verifier text; `SessionEngine::set_os_sandbox_enforced`; rewind keeps the removed tail for redo.

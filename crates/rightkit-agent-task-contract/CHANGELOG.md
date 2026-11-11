# Changelog

## 0.1.0 (unpublished)

- New crate: Pulse's task contract, moved out of `rightkit-agent-engine` 0.1.1 when that name became the facade of
  the one agent engine. Decision 4 of the 2026-11-11 shape review (docs/audits/2026-11-11-shape-matrix/README.md,
  "Agent engine") taken on the recommended option, pending the owner's confirmation; design note:
  docs/audits/2026-11-11-shape-matrix/reports/agent-engine-contract-design.md.
- Paths: `rightkit_agent_engine::X` (0.1.1) is `rightkit_agent_task_contract::X` with the same module names; the
  facade `rightkit-agent-engine` 0.2.0 still re-exports every one of them.
- `ApprovalBinding` carries the approvals core's `Binding` (`binding`) instead of four loose fields
  (`args_hash`, `destination`, `expires_at_ms`, `revision`). The wire format is unchanged (the four keys stay
  flat); `args_hash()` gives the `sha256:<hex>` form and `core_binding()` returns the core record. Reading a
  record now refuses an `args_hash` that is not `sha256:` plus 64 lowercase hex, the form the contract always
  required.
- Product strings out: `VOICE_CHAT` (`heardright:control`) and `VOICE_POLICY_REF` are gone;
  `VoiceHandoff::to_task_envelope` takes the caller's `VoiceLabels { chat, policy_ref }` and refuses empty ones.
  Doc comments no longer name an assistant or a chat product.
- Task command line behind the `cli` feature (off by default), so the contract's default build has no stdin,
  stdout or polling. `TaskHost` gains `voice_labels()` and `submitter_chat()`: the chat-session environment
  variables the command read (`CLAUDE_CODE_SESSION_ID`, `CODEX_THREAD_ID`, `MUSE_SESSION_ID`) are now the
  application's to read. The help text names no agent product.
- One home for `Principal`, `DeviceRef` and `EffectClass`: `rightkit-device-link-types` 0.1.2 re-exports these
  modules instead of keeping its line-for-line copy.

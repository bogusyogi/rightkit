# Agent engine: one engine, one task contract

Decision 4 of the shape review (`../README.md`, "Agent engine"), taken on the recommended option, pending the
owner's confirmation. Source review: `family-agent-platform.md`, section rightkit-agent-engine ("Best shape").
Builds on the approvals core (`approvals-core-design.md`). File:line references are to the code as it was on the
branch before this change (commit `d95b190`), unless marked "now".

## What existed

* `crates/rightkit-agent-engine` 0.1.1: Pulse's task contract (from `pulse-core` `agent`), serde records and pure
  functions: `TaskEnvelope` (`task.rs:32`), ten statuses (`task.rs:104`) and the transition table
  (`task.rs:148`), `ApprovalBinding` (`approval.rs:18-40`), principals, intents, effects, devices, the voice
  hand-off and a 672-line task command line (`task_cli.rs`). Nothing in RightKit depended on it.
* `crates/rightkit-agent-engine-*` (14 crates): CodeRight's turn-loop engine. No code or dependency shared with the
  base crate.

## What moves where (now)

| From | To |
|---|---|
| `rightkit-agent-engine/src/{approval,device,effect,intent,principal,task,voice,task_cli,lib}.rs` | new `rightkit-agent-task-contract` 0.1.0, same module names (`git mv`) |
| `task_cli` (stdin, stdout, poll loop) | same crate, behind feature `cli` (off by default) |
| `rightkit-agent-engine` | facade 0.2.0: `core` = `-core`, `protocol` = `-protocol`, `storage` = `-storage`, `contract` = the task contract; the contract's modules and records also stay at the root |
| `rightkit-device-link-types/src/{device,effect,principal}.rs` | deleted; `pub use rightkit_agent_task_contract::{device, effect, principal}` |
| `ApprovalBinding.{args_hash, destination, expires_at_ms, revision}` | one field `binding: rightkit_approvals::Binding`; wire unchanged (`serde(into, try_from)` keeps the four flat keys) |
| `PendingApprovalContext.payload_digest: Option<String>` (`-protocol/src/approvals.rs:116`) | `binding: rightkit_approvals::Binding`, mandatory; `payload_digest()` gives the `sha256:<hex>` grant key |
| `PermissionGateRequest` (`approvals.rs:43`) | gains the same mandatory `binding` (no user in the tree) |
| `ExternalEffectSettlement` (`-protocol/src/effect_settlement.rs:10`) | `task_status()`: `InterruptedUnknown` is the contract's `TaskStatus::Uncertain`; `Running`, `Succeeded`, `Failed` map by name; `NotStarted` is none |

The engine protocol uses the contract for authority: it depends on `rightkit-agent-task-contract`, re-exports it
as `task_contract`, and its settlement maps onto the contract's statuses. A tool call that waits for approval is
bound by Pulse's rule through the core binding: `-core` builds it at the approval request
(`engine_tool_dispatch_approval.rs:311` before) from the effect payload digest of the exact final payload and the
destination (the call's network host, else the tool), with `effect_binding`. The server's "approve for session"
(`rightkit-agent-server/src/api_sessions/approvals.rs:154` before) no longer has a no-digest branch.

## Duplicate types and which definition wins

| Type | Definitions | Winner | Done |
|---|---|---|---|
| `Principal`, `PrincipalKind` | `rightkit-agent-engine/src/principal.rs:12,25`; `rightkit-device-link-types/src/principal.rs:13,26` | the contract's (Pulse's reference, same code; differ only in comments) | copy deleted, re-exported |
| `DeviceRef`, `DevicePlatform` | `rightkit-agent-engine/src/device.rs:13,25`; `rightkit-device-link-types/src/device.rs:13,25` | the contract's | copy deleted, re-exported |
| `DeviceRef`, `Platform` (decode-only) | `rightkit-mesh-ffi/src/circle.rs:61`, `src/pairing.rs:173,194` | the contract's | deferred: mesh-ffi is its own workspace and lock (iOS patch), not built here |
| `EffectClass` (reversible, destruction, spend, publication) | `rightkit-agent-engine/src/effect.rs:11`; `rightkit-device-link-types/src/effect.rs:11` | the contract's | copy deleted, re-exported |
| Approval record, engine side | Pulse `ApprovalBinding` (`approval.rs:18`, `args_hash` mandatory); CodeRight `PendingApprovalContext` (`approvals.rs:105`, `payload_digest` optional) | the approvals core's `Binding` inside both | both carry `Binding` |
| Task model | Pulse `TaskEnvelope`/`TaskStatus` (authority, idempotency, owner epoch); CodeRight `TaskNode`/`TaskState` (`-protocol/src/task.rs:6,47`, a planning DAG) | not a duplicate in function: both stay | rename CodeRight's to `PlanTask*` deferred |
| Unknown outcome | `TaskStatus::Uncertain` (`task.rs:104`); `ExternalEffectSettlement::InterruptedUnknown` (`effect_settlement.rs:15`) | both stay, mapped | `task_status()` |

Not duplicates of the contract's `EffectClass` (different vocabularies, left alone): `rightkit-agent-policy`
`capabilities.rs:81` (read/write/external/spend/destroy, CodeRight's tool caps). Found in passing, outside this
decision: `rightkit-agent-policy/src/legion/policy_adapter.rs:13` and `rightkit-policy-rules/src/model/effect.rs:8`
define the same twelve-variant `EffectClass`, and `rightkit-evidence-contracts/src/policy.rs:38` a third spelling of
it. Five other `TaskStatus` enums (`rightkit-agents/src/tasks.rs:39`, `rightkit-daemon-remote/src/remote/state.rs:35`,
`rightkit-evidence/src/lib.rs:139`, `rightkit-evidence-contracts/src/task.rs:19`,
`rightkit-terminal-ui/src/transcript/timeline.rs:227`) are UI or job states, not task authority. CodeRight's
`rightkit-agent-policy` `ApprovalBinding` (`approval_binding.rs:34`) is a payload verifier with the same name as
Pulse's record; it already digests through the core's `bind_of`.

## Product strings out of library code

* `voice.rs:34` `VOICE_CHAT = "heardright:control"` and `voice.rs:36` `VOICE_POLICY_REF`: gone;
  `to_task_envelope` takes the caller's `VoiceLabels { chat, policy_ref }`.
* `task_cli.rs:266-268` read `CLAUDE_CODE_SESSION_ID`, `CODEX_THREAD_ID`, `MUSE_SESSION_ID`: now
  `TaskHost::submitter_chat()`, the application's. `TaskHost::voice_labels()` supplies the hand-off labels. The
  help text (`task_cli.rs:113`) and doc comments (`task_cli.rs:89`, `intent.rs:52` "Astra", `intent.rs:99` and
  `principal.rs:31` "ChatGPT") name no product.

## Versions

New `rightkit-agent-task-contract` 0.1.0 (PK218). `rightkit-agent-engine` 0.2.0 (facade).
`rightkit-agent-engine-protocol` 0.2.0 (breaking: mandatory `binding`). `rightkit-agent-engine-core` 0.1.1,
`rightkit-agent-server` 0.1.2, `rightkit-device-link-types` 0.1.2. Requirement raised to protocol 0.2.0, patch
bump: `-storage` 0.1.2, `-tasks` 0.1.2, `-mobile-runtime` 0.1.1, `rightkit-bot-messaging` 0.1.2, `rightkit-mcp`
0.7.6, `rightkit-scheduler` 0.1.2, `rightkit-llm` 0.3.6, `rightkit-agents` 0.2.5, `rightkit-agent-tools` 0.3.5,
`rightkit-agent-orchestration` 0.1.2, `rightkit-hooks` 0.2.5; `rightkit-agent-policy` 0.1.3 and
`rightkit-agent-daemon` 0.1.0 (both unpublished, already new on the branch) record it in their open entry.

## Deferred (follow-up)

* Use-time check on the core rule: `-core` still rechecks an approval through `approved_effect_digests` (a digest
  string per call) rather than `Binding::check`; the binding has no expiry of its own (external approvals already
  lapse after 300 s in `engine_tool_dispatch_single.rs`) and revision 0. Filling expiry and the policy revision
  (`observe_host_revision`) and checking with `Binding::check` is the next step in the loop.
* Rename CodeRight's `TaskNode`/`TaskState` to `PlanTask*` (wide rename across `-protocol`, `-tasks`, `-storage`,
  agent-server).
* `rightkit-mesh-ffi`'s decode-only `DeviceRef` and `Platform` copies (own workspace and lock).
* `task_cli` into a command-line crate of its own, if a second application adopts it; for now it is a feature of
  the contract.
* The 11.7k lines of CodeRight's engine still in `rightkit-agent-server` (the agent-server split, not done here).
* The Arcane `EffectClass` held twice (`rightkit-policy-rules`, `rightkit-agent-policy` legion adapter) and a third
  spelling in `rightkit-evidence-contracts`.
* Pulse's Swift loop, budget ledger and device-lock approval gate (decision 5, `RightKitAgent`).

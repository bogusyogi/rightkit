# rightkit-agent-task-contract

The agent task contract: who asked, with what authority, on which device, and what happens when the outcome is
unknown. Task envelope and its ten statuses with legal transitions (`uncertain` after an unknown outcome,
`cancel-requested` not terminal), principals, intent provenance (a relay never mints authority), effect
classes, portable device references, the approval binding and the voice hand-off.

Serde types and pure functions only: no runtime, journal, IPC or effects. The approval binding carries the
approvals core's `rightkit_approvals::Binding` (digest of the exact arguments, destination, expiry, revision),
so an approval checked here is the same record every approval store checks.

Product names are the caller's: a voice hand-off is mapped with the `VoiceLabels` (chat label, policy
reference, agent name) the application supplies, and the task command line (feature `cli`) takes its program name,
product name, voice labels, submitting chat and, when it must keep its own text, the `TaskTexts` (help, command
spelling, two refusals) from the application's `TaskHost`.

`rightkit-agent-engine` re-exports this crate as `contract`, and its protocol uses it for authority
(`ExternalEffectSettlement::task_status`). `rightkit-device-link-types` re-exports its `device`, `effect` and
`principal` modules.

Moved into RightKit from `pulse-core` (`agent` module) on 2026-11-01 as `rightkit-agent-engine`; its own crate
since 0.1.0 (2026-11-11).

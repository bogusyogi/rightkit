# Changelog

## 0.3.4

- Add `app_launch::list_shortcuts`: the user's installed Shortcuts by name for a discovery list (macOS; always empty elsewhere). A probe that times out or exits unsuccessfully is an empty list.
- Additive only: no existing item changes.

## 0.3.3

- Spoken replies and interview answers (VO-3, VO-4). `HandoffIdentity::spoken_reply` submits the hand-off as `agent.request` with `reply: "speech"` (an answer written to be read aloud, answer-only whatever `agentActions` says); `submit_with` and `submit_installed(record, spoken_reply)` take it. `record_for` takes the pending interview question and sends it as `interview:<question id>` in `context_refs` (Pulse refuses any `context_refs` until its profile exists, so a hand-off that carries one is refused with `unsupported_context` today). `task_speech_text` gives the text to say aloud for a finished task: the answer `pulse task result` returns, whole for a reply written for speech and its first sentence for an acting run. Source-breaking for callers of `record_for`, `submit_installed` and a struct literal of `HandoffIdentity`; the only caller is `rightkit-dictation-runtime` 0.2.7.

## 0.3.2

- The hand-off now reads Pulse's real answers (VO-1, `docs/contracts/voice.md` in the Pulse repository). The accepted receipt is `receipt: "accepted"` with no `status`; `parse_receipt` read `status == "accepted"` and so never saw an accepted task. `Receipt::Accepted` gains `task_status`; `Receipt::NotBuilt`, which the command line never prints, is replaced by `Receipt::Unreachable` (`status: "unreachable"`: no engine answered, nothing ran, so the words are kept and a resend is the caller's) beside `Receipt::Unknown` (sent, no answer: `submit` resends once with the same key). A command line that cannot be started is `Unreachable`; a record that fails validation is `Refused`, not `Unknown`. `Receipt::pill_message` gives the pill's line for each of them. Source-breaking for a `match` on `Receipt`; the only caller is `rightkit-dictation-runtime` 0.2.6.
- `default_cli_path` looks for the per-user install first, `%LOCALAPPDATA%\Programs\Pulse\Helpers\pulse.exe` (the Local AppData known folder), then `Program Files\Pulse\Helpers\pulse.exe`; on the Mac it is `/Applications/Pulse.app/Contents/Helpers/pulse`. It returns the first that exists, where it returned the Program Files path whether or not Pulse was there. `submit_installed` sends to it and, with no Pulse installed, answers `Unreachable`.
- New: `HandoffIdentity::record_for` builds the record for one addressed recording that no command matched (generation 1, so the key `voice:<utterance id>:1` is the same on every resend; confidence 1.0, since the ASR gives none), `local_platform`, `pill_for_status` and `TaskPill` (the receipt-to-pill table), `task_pill` (`pulse task status`, and `pulse task result` for the first words of a finished answer) and `CLI_TIMEOUT`.
- Fixed the hand-off's wire form (RK-14, VO-1): `pulse_handoff::submit` sent a record the Pulse command line refused, a top-level `idempotency_key` that the contract's `VoiceHandoff` does not accept and the transcript, session id, wake id and confidence nested inside `utterance`. `HandoffRecord::to_voice_handoff` now builds the agent task contract's own `VoiceHandoff` and `submit` sends that, so the contract's types decide what is valid. The key is not sent: the command line derives the same `voice:<utterance id>:<generation>` (`HandoffRecord::idempotency_key` is unchanged). `HandoffRecord`, its fields and `validate` are unchanged. New dependency: `rightkit-agent-task-contract` 0.1.1 (already published). Still no caller: the hand-off branch in `rightkit-dictation-runtime` waits on VO-0 (whether the wake listener runs, decision 5) and is not written. Not compiled or run here; rustfmt checked.

## 0.3.1

- Bind live streaming and final append to a fresh verified caret on the same control. Preserve visible streamed text on cancel or divergent final output and report the copy fallback.
- Share platform destination security probes so hosts can forward the current native check directly.

## 0.3.0

- Add host-configured paste delivery, stop-time control binding & streaming reconciliation. Automatic writes require fresh security, control & physical-modifier evidence.
- Check permit expiry after receipt marking, immediately before input is sent; retain clipboard custody reporting on refusal.
- Legacy delivery calls without host policy refuse automatic input with `delivery_host_required` & leave clipboard untouched. Migrate automatic delivery to `paste_delivery::RealDeliveryBackend` or `paste_delivery::paste_text`.
- Last-paste transforms without host-bound replacement custody refuse before Undo, preserving original field content. Empty-input & unknown-transform results remain unchanged.
- Require `rightkit-automation` 0.7.2, `rightkit-transcript-library` 0.1.4 & `rightkit-dictation-core` 0.3.3.

## 0.2.1

HeardRight's command host is compared with this crate and every difference in its output is taken; nothing changes for a host that makes no call below.

- `host::set_paste_owner_name(&'static str)` (once per process; returns `false` when already set) names the paste's owner in the undo refusal "Undo skipped: no recent `<name>` paste to undo" (default `dictation`). `host::paste_owner_name()` returns it.
- Compared and kept: HeardRight's `test_mode()` is always false in its shipped build; its bundle identifier, overlay window titles and own-process prefix are `host::HostIdentity` values the host already sets.

## 0.2.0

- Breaking for dependents: dependency requirements `rightkit-automation` 0.6 -> 0.7, `rightkit-dictation-core` 0.2 -> 0.3, `rightkit-dictation-text-pipeline` 0.1 -> 0.2, `rightkit-logs` 0.3 -> 0.4. 0.1.0 was published before these requirements were raised; a semver-breaking dependency is a breaking change, so they ship as 0.2.0 instead of changing 0.1.0 in place (#1572). No source change in this crate since 0.1.0.

## Unreleased

- Removed `test_mode()` and every test branch in delivery (clipboard, keystrokes, targets, insert); delivery always takes the real OS path (Adrian, 2026-11-03: "My decision was no unit tests. Only e2e."; "dev mode, is mine ... real data. snapshot and restore.").
- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `heardright-command-host` (tauri-app-next).

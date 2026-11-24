# Changelog

## 0.2.6

- Addressed recordings reach Pulse (VO-1, decision 5 in the Pulse repository's `docs/contracts/voice.md`). A recording that began from the wake word (`FinalTranscript::addressed`, set from the worker's own wake hand-off, `RecordingSession::addressed`) and that no command, AI-transform tail or bare summarize matched is no longer dictated once the host calls the new `host::configure_pulse_handoff(HandoffIdentity)`: finalize phase 2 submits it with `rightkit_dictation_command_host::pulse_handoff::submit_installed`. Only an accepted receipt clears the words (`FinalizeOutcome::CommandDispatched`, action `pulse.task`, detail `<task id> <task status>`); a refused, unreachable or unanswered one keeps them as the new `FinalizeOutcome::HandoffKept { text, message }`, sent to the shell as a transcript event with `cancelled` and `command_failed` set (history, never delivery; the pill's line is in `command_failed`). Nothing is typed or replayed. A recording from the dictation key is never addressed. A host that makes no call keeps today's behaviour. `FinalizeOutcome` gains a variant and `FinalTranscript` a field: source-breaking for an exhaustive `match` and for a struct literal.
- Not done here: the pill following the task after acceptance (the host shows `command_detail` and polls `pulse_handoff::task_pill`), the follow-up window and compound grammar (VO-2), and a way to resend a kept hand-off on the owner's action.

## 0.2.5

- Seal exact capture at the validated release edge and retire the producer before reopening the selected microphone.

## 0.2.4

- Use the inference crate's wake-to-recording audio handoff and expose the production replay policy on macOS as well as Windows.
- Route last-paste casing through a typed reset command for the shell. The legacy engine repaste route now refuses & directs callers to the shell delivery host.

## 0.2.3

- `GetState` returns fixed state labels without error text; transcribing-session correlation retains its existing wire format. Transcript-returning APIs remain unchanged.

## 0.2.2 (2026-11-16)

HeardRight's engine runtime was compared with this crate again, file by file (cli, crash capture, entry, owner diagnostics, speech service, worker, recording finish, finalize, IPC); nothing changes for a host that makes no call below.

- New: `crash_capture::configure_crash_options(CrashOptions)` (once per process, before `install_panic_hook`; returns `false` when already installed). `CrashOptions::dump_helper` names the Windows dump helper file (default `rightkit-dump-helper.exe`). `CrashOptions::abort_in_report_hook` (default `false`) ends the process from inside `rightkit-crash`'s own hook (`CrashConfig::abort_after_report(true)`, rightkit-crash 0.4.0) instead of wrapping the installed hook: a panic an app catches on purpose inside a `CaughtPanicScope` then no longer aborts the process, which the wrapper did. `CrashOptions::DEFAULT` is the old behaviour. The product and version in the reports are still `configure_crash_identity`.
- New: `host::configure_service_log_names(ServiceLogNames)` and `host::service_log_names()` for the names in two speech-service warnings: `unidentified_peer_event` (default `service_peer_unidentified`), `unidentified_peer_reason_first` (default `false`: `reason` after the peer's `pid`, `image` and `kind`) and `untrusted_peer_field_is_reason` (default `false`: the `service_peer_untrusted` refusal is the field `error`; `true` names it `reason`). `ServiceLogNames::DEFAULT` is the old behaviour.
- Compared and kept: HeardRight's `entry.rs` calls `perf_levers::apply_process_qos`, `worker/unified_probe_lane.rs` carries `keep_warm_continuous` and `HR_ASR_KEEP_WARM_PAUSE_MODE`, `recording_pump.rs` an opening-probe and keep-warm pause switch, `recording_finish.rs` the macOS stop-snapshot reorder, `ipc/dispatch.rs` and `ipc/events.rs` the live L3 reconfigure and the drain length. Every one of them reads `perf_levers`, whose values are parsed from `None` (no environment is read in any build), so each takes its default path, which is the path this crate has; porting them would add code no host can reach. `finalize_transcript.rs`'s live-evaluation branch asks `live_eval_suppresses_delivery()`, which is `false` in a shipped build. `owner_diagnostics.rs`'s `resolve_root` is always called with no override. `worker_api.rs`'s wake diagnostics prefix, `entry.rs`'s calibration diagnostics variable, the microphone-permission owner, the product name and the engine name and version are the existing `host::configure_identity` fields.
- Still different: the `target` of two finalize-timing events (`tracing` needs a constant target; this crate says `rightkit_finalize_timing`, nothing in either tree filters on it), the extra `--collect-diagnostics` flag of the `calibrate` one-shot, `spawn_dummy_worker`, and the `kws` module name (`kws_cascade` on the other side, with the inference fork).

## 0.2.1

HeardRight's engine runtime is compared with this crate and every difference in its output is taken; nothing changes for a host that makes no call below.

- New module `host`, once-per-process settings (each returns `false` when already installed). `configure_identity(EngineIdentity)` sets the engine name (hello envelope, ready log line, fatal IPC message, diagnostics JSON), the build identity string (hello envelope, engine info, speech-service response), the product name in "repair the `<name>` installation", the owner in the microphone-permission hint, the wake diagnostics stderr prefix and an optional environment variable that turns on calibration diagnostics in the `calibrate` one-shot as `--collect-diagnostics` does. `EngineIdentity::DEFAULT` is the old behaviour. `identity()` returns the one in force.
- `host::configure_cli(CliIdentity)` turns on the installed command line taken from HeardRight (#309): `<program> transcribe <file> [--format txt|srt|vtt|json] [--out <path>]`, `help`, `--version`, a speech client that queues behind dictation and, with the app closed, starts this executable as a headless speech host. Exit codes 0, 1, 2, 3, 4, 5 and 130. Without the call the engine has no command line and every argument belongs to its other roles. New module `cli` (macOS and Windows, like `entry`).
- Compared and kept: HeardRight's `perf_levers` and `ipc/l3_live.rs` read no environment in a shipped build (every lever is its default; `ReconfigureL3` answers "not enabled"), which this crate already does. Its hand-written Windows and macOS peer verification is `rightkit_daemon::verify_peer` here.

## 0.2.0

- Breaking for dependents: dependency requirements `rightkit-automation` 0.6 -> 0.7, `rightkit-crash` 0.3 -> 0.4, `rightkit-daemon` 0.5 -> 0.8, `rightkit-dictation-command-host` 0.1 -> 0.2, `rightkit-dictation-core` 0.2 -> 0.3, `rightkit-dictation-inference` 0.1 -> 0.2, `rightkit-dictation-models` 0.1 -> 0.2, `rightkit-dictation-text-pipeline` 0.1 -> 0.2, `rightkit-job-queue` 0.5 -> 0.6, `rightkit-logs` 0.3 -> 0.4, `rightkit-onnx` 0.2 -> 0.3, `rightkit-speech-client` 0.4 -> 0.5. 0.1.0 was published before these requirements were raised and before the change below; a semver-breaking dependency is a breaking change, so they ship as 0.2.0 instead of changing 0.1.0 in place (#1572).
- Speech-service peers are checked with `rightkit_daemon::verify_peer` (an Orthic `PeerPolicy`: macOS team `6KLGD3LLKF`, Windows signer `Damned Ventures LLC`) instead of this crate's own copy of the macOS and Windows signature checks; `speech_service/peer_trust.rs` keeps only the identities (388 to 44 lines). The Windows pipe-name tag is `rightkit_daemon::current_user_sid()` instead of its own token query. The `security-framework`, `core-foundation` and `windows-sys` dependencies are gone with that code. Same identities and the same fail-closed refusal; the refusal log line's reason is now `verify_peer`'s error text.

## Unreleased

- Removed the `RIGHTKIT_CALIBRATION_COLLECT_DIAGNOSTICS` environment switch. `calibrate <pcm>` takes an explicit `--collect-diagnostics` argument instead; the shell passes it when it wants diagnostics.
- Removed the test-mode environment levers (`perf_levers`, `RIGHTKIT_*` reads via `dev_var!`), the live L3 reconfigure module (`ReconfigureL3` now answers with the not-enabled error), the live-evaluation hook in finalize, the VAD auto-calibrate and developer ORT-path overrides, and the argv models-base hint; each now runs its shipped behaviour only (Adrian, 2026-11-03: "My decision was no unit tests. Only e2e."; "dev mode, is mine ... real data. snapshot and restore.").
- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `heardright-runtime` (tauri-app-next).
- Sherpa removed: keyword spotting is the Rust `rightkit_asr::kws` through `rightkit-dictation-inference`'s `kws::Kws`. The `rust-kws` cargo feature is removed; the `sherpa_kws` re-export is `kws`; `engine_paths::EnginePaths::sherpa_kws_dir` is `kws_dir`; the test-mode directory override is `RIGHTKIT_KWS_DIR` (was `RIGHTKIT_SHERPA_KWS_DIR`); the hey-wake event model label is `kws-hey-zephyr` (was `sherpa-onnx-hey-zephyr`) and the control classifier result is `control_complete_kws` (was `control_complete_sherpa`).
- Staged in the unreleased 0.1.0 section: this crate has not been published, so the removal is not a version bump.

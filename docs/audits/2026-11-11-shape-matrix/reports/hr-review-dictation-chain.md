# HeardRight dictation chain vs RightKit: read-only comparison

Trees compared (no builds, no edits):

- LIVE = `survey/heardright-live/tauri-app-next` (Mac 0.1.213, 12b7d8ce)
- MAIN = `survey/heardright-main/tauri-app-next` (0.1.231, unreleased)
- RK = `/home/user/rightkit/crates`

Method. Files were paired by relative path and diffed with blank lines dropped. "norm" counts are changed lines after replacing every `heardright*` / `rightkit*` identifier with one token, so a pure rename counts as 0. Counts include in-file tests where a side still has them. Every pair listed in section 1 was diffed in full by script; the diffs I actually read are named in each row. Anything else is marked "not verified".

Paths below are relative to the tree root (`heardright-live/…`, `heardright-main/…`) or to `rightkit/crates/` (`rightkit-…`).

## Headline

1. RK's dictation crates are a fork of a recent MAIN snapshot, not of LIVE. Against MAIN they differ by 2-6% of lines (rename, host configuration, removal of test levers). Against LIVE they differ by 30-45%.
2. MAIN consumes only the bottom layer from RightKit: `rightkit-dictation-core`, `rightkit-text-polish`, `rightkit-voice-commands`, `rightkit-transcript-library` (all through `heardright_core` re-exports), plus `rightkit-speech-client` and `rightkit-transcript`. Ten `rightkit-dictation-*` crates (about 62,000 lines) have no consumer in MAIN: MAIN runs its own copy of every one of them.
3. For text polish, voice commands and the pure state machine, RK reproduces LIVE logic exactly (formatting-only diffs) apart from three deliberate API changes. For the text pipeline and the shell session runtime, RK reproduces MAIN, which has moved a long way from LIVE on purpose.
4. Real divergences that need work before HeardRight can drop its copies: the KWS wrapper (about 2,960 of 3,000 lines differ), the engine CLI (`transcribe`, #309) missing from RK, the app-data-root design, several product strings renamed in RK that are wire- or file-visible, and the fact that the RK engine binary gives the host no place to configure anything.

## 1. Crate-to-crate map

| HeardRight (MAIN) | RightKit holder | Closeness to MAIN | Changed lines (norm / RK lines matched) | Newer in content |
|---|---|---|---|---|
| `heardright_core::text_pipeline/*`, `vocab_learn`, `vocabulary` (deleted in MAIN, now a 72-line brand wrapper) | `rightkit-text-polish` | MAIN already uses RK. vs LIVE: diverged only by brand parameterisation and rustfmt | vs LIVE 278 / 3,159 | RK (single copy) |
| `heardright_core::command`, `command_recognition/*` (deleted in MAIN) | `rightkit-voice-commands` | MAIN already uses RK. vs LIVE renamed + tests removed | vs LIVE 60 / 984 | RK (single copy) |
| `heardright_core::delivery`, `history`, `net` (deleted in MAIN) | `rightkit-transcript-library` | MAIN already uses RK | vs LIVE 208 / 1,378 (not read line by line) | RK (single copy) |
| `heardright_core::engine/*`, `state`, `wake`, `mic_selection`, `audio_conditioning`, redaction (deleted in MAIN) | `rightkit-dictation-core` | MAIN already uses RK (pinned =0.2.3; tree is 0.2.4) | vs LIVE 633 / 2,319 | RK (single copy) |
| `heardright_core::settings/{blob,patch}.rs`, `app_paths.rs` (still in MAIN) | `rightkit-dictation-core::settings`, `::app_paths` | DUPLICATED, diverged in API | 98 / 891 | Neither: two designs (section 4) |
| `heardright_core::ffi.rs`, `kws_ffi.rs` | `rightkit-dictation-ffi` (`lib.rs`, `kws.rs`) | renamed C symbols, diverged | 68 and 56 (raw rename-normalised diff) | MAIN (keeps brand repair) |
| `heardright_core::external_url.rs` | `rightkit-voice-commands::external_url` | Done: MAIN keeps only the allowlist data | n/a | RK |
| `heardright_core::pill`, `hotkey_conflict`, `credential` | not in the dictation crates (LIVE `ptt_policy`/`chord_format` now live in `rightkit-hotkeys`) | not verified (other review) | | |
| `heardright-telemetry` | `rightkit-dictation-telemetry` | renamed + host config | 71 / 1,282 | Same content; RK adds `configure_*` |
| `heardright-app-runtime` | `rightkit-dictation-app-runtime` | renamed + host config | 82 / 2,282 | Same content |
| `heardright-model-store` | `rightkit-dictation-models` | renamed + small divergence | 104 / 2,758 | Mixed (RK has one rename bug; MAIN has test-only `verify_receipt.rs`) |
| `heardright-text-pipeline` | `rightkit-dictation-text-pipeline` | renamed + lever removal + 3 renamed wire strings | 323 / 4,163 | Same logic; RK cleaner, MAIN has the right strings |
| `heardright-command-host` | `rightkit-dictation-command-host` | renamed, `test_mode()` removed, `heardright_platform` calls now `rightkit_automation::desktop` | 340 / 5,484 | Same logic |
| `heardright-inference` | `rightkit-dictation-inference` | diverged | 1,744 / 12,910 plus `kws.rs` vs `kws_cascade.rs` 2,962 | Mixed; KWS undecided |
| `heardright-runtime` | `rightkit-dictation-runtime` | renamed + lever removal; RK lacks 3 files | 564 / 13,921 | MAIN (has CLI) |
| `heardright-engine` | `rightkit-dictation-engine` | renamed only | 4 / 209 (src only; examples not diffed) | Same; MAIN has `build.rs` |
| `heardright-parakeet-core` (879 lines left in MAIN) | `rightkit-asr::parakeet_core` | near-identical except `tdt_beam.rs` | bias 4, command_alignment 2, tdt_beam 61 | MAIN for `tdt_beam` (scratch buffer) |
| `heardright-onnx-asr` | `rightkit-asr::onnx_tdt` | diverged | environment 7, model 202, session 151 (counts only, not read) | not verified |
| `heardright_platform` | `rightkit-automation::desktop` | renamed, small diffs | windows.rs 18/1,408, macos_focus_continuity 20/1,193, focus/pin 40/563, focus/context 38/664 (counts only) | not verified (other review) |
| `src-tauri/src/runtime/*`, `runtime.rs`, `runtime_lock.rs`, `start_sequence.rs`, `dictation_error.rs`, `engine_event_consumer.rs` | `rightkit-dictation-tauri` | diverged by host-trait extraction | 737 / 3,899 (session.rs alone 440) | Same logic; RK moves platform delivery behind a trait |
| `src-tauri/src/delivery/*` (3,164), `history/*` (2,236), `windows_uia/*`, `streaming_insertion.rs`, `cloud_sync*`, `media_control/*`, `settings.rs`, `vocabulary.rs`, `dictation_lifecycle.rs`, `dictation_commands.rs`, `engine_events*.rs`, `pill*`, `commands/*` | none in the dictation crates. These are what `DictationHost` expects the app to supply | n/a | HeardRight only |
| LIVE `heardright_capture` | removed in MAIN (`rightkit-audio` capture) | not verified | | |

Files diffed and read for this table: text-polish `polish.rs`, `control.rs`, `normalize.rs`, `inline.rs`, `text_pipeline.rs`, `vocab_learn.rs`, `vocabulary.rs`; voice-commands all five files and `external_url.rs`; core `state.rs`, `app_paths.rs`, `settings/blob.rs`, `settings/patch.rs`; ffi `lib.rs`; text-pipeline `providers.rs`, `prompts.rs`, `lanes.rs`, `acceptance.rs`, `state.rs`, `policy.rs`, `ops.rs`, `vocabulary.rs`, `settings.rs`, `transport.rs`, `text_polish.rs`, `local.rs`; command-host `delivery/{insert,clipboard,keystrokes,targets}.rs`, `focus.rs`, `screenshot.rs`; inference `onnx_beam_decode.rs`, `trigger_audio_cut.rs`, `control_cleanup.rs`, `vad.rs`, `file_transcribe.rs`, `asr/runtime.rs`, `coreml_asr/{decode_window,engine,hypothesis_batch}.rs`, `coreml/multiarray.rs`, `whisper_win_ffi.rs` (head only), `kws.rs` (structure only); runtime `entry.rs`, `worker_api.rs`, `recording_finish.rs`, `worker_main.rs`, `recording_pump.rs`, `ipc/{dispatch,events}.rs`, `crash_capture.rs`, `owner_diagnostics.rs`, `speech_service/*`, `wake_listener.rs`, `engine_impl.rs`; models `shared.rs`, `model_paths.rs`, `ane_cache_prune.rs`, `model_download_grant.rs`; app-runtime all; tauri all 13 files plus `host.rs` and `lib.rs`. Not read: `unified_probe_lane.rs` (85 changed), `asr/models.rs` beyond imports, `whisper_coreml/*` beyond one hunk, `calibration.rs` beyond signature, all `examples/`.

`rightkit-speech-client`, `rightkit-transcript`, `rightkit-library`: no file in any of them matches a HeardRight source file. They are not copies. MAIN uses the first two; nothing in HeardRight uses `rightkit-library`.

## 2. Does MAIN use each RightKit crate?

Used (dependency line, then call site):

| RK crate | Cargo.toml | Call site |
|---|---|---|
| `rightkit-dictation-core` =0.2.3 | `heardright_core/Cargo.toml:29` | `heardright_core/src/lib.rs:28` re-exports `audio_conditioning, engine, mic_selection, redact_*, state, wake`. All of MAIN reaches it as `heardright_core::…` (about 340 references; 167 in src-tauri, 78 in heardright-runtime). Its `settings` and `app_paths` modules are NOT used: MAIN keeps `heardright_core/src/settings/{blob,patch}.rs` and `app_paths.rs` |
| `rightkit-text-polish` =0.1.0 | `heardright_core/Cargo.toml:37` | `heardright_core/src/text_pipeline.rs:5,14`; `lib.rs:32` |
| `rightkit-voice-commands` =0.1.0 | `heardright_core/Cargo.toml:39` | `heardright_core/src/lib.rs:34`; `external_url.rs:42` |
| `rightkit-transcript-library` =0.1.2 | `heardright_core/Cargo.toml:35` | `heardright_core/src/lib.rs:33` |
| `rightkit-speech-client` =0.4.2 | `heardright-runtime/Cargo.toml:14`, `heardright-engine/Cargo.toml:62` | `heardright-runtime/src/speech_service/{mod.rs:20,conn.rs:15,files.rs:6}`, `cli.rs:16` |
| `rightkit-transcript` =0.4.0 | `heardright-inference/Cargo.toml:31` | `heardright-inference/src/subtitle_export.rs:6` |

Not used by anything in MAIN (no Cargo.toml entry, no `rightkit_dictation_*` path in any .rs file other than `heardright_core/src/lib.rs:28`; RK's own `docs/ADOPTION.md:114-124` records zero consumers for each):

`rightkit-dictation-app-runtime`, `-command-host`, `-engine`, `-ffi`, `-inference`, `-models`, `-runtime`, `-tauri`, `-telemetry`, `-text-pipeline`, and `rightkit-library`.

Version note: the pinned crates in MAIN's `Cargo.lock` come from crates.io with checksums. The RK tree is ahead of the pins (`dictation-core` 0.2.4, `text-polish` 0.1.1, `voice-commands` 0.1.1, `transcript` 0.4.2); those bumps are test removal and one `provider_handoff.rs` republish per the changelogs. Whether 0.2.4 is published: not verified.

## 3. Against the LIVE build

### Text polish (L0)

LIVE: `heardright_core/src/text_pipeline/*`. Fixed pipeline in `deterministic_polish_inner` ending with lexicon then HeardRight homophone repair (`polish.rs:158`, table at `:177`, repair at `:258`).

RK reproduces it. Every hunk in `control.rs`, `normalize.rs`, `inline.rs`, `number_words.rs` (0 changed lines) is rustfmt or comment text. Differences:

| # | Difference | Where | Status |
|---|---|---|---|
| P1 | Brand repair is now a host-supplied table. Plain `deterministic_polish` does no brand repair; the host must call `deterministic_polish_with_brand` | `rightkit-text-polish/src/text_pipeline/polish.rs:299,305,695` | verified by reading |
| P2 | MAIN supplies the same seven phrases, guards and 18 context terms as LIVE | `heardright-main/heardright_core/src/text_pipeline.rs:22-62` vs LIVE `polish.rs:177-212` | verified by reading |
| P3 | `rightkit-dictation-ffi` polish calls the no-brand entry point, so a native shell on that crate loses "heard right" → "HeardRight" | `rightkit-dictation-ffi/src/lib.rs:71` (MAIN `heardright_core/src/ffi.rs:69` keeps it) | verified by reading |
| P4 | `rightkit-dictation-text-pipeline` does brand repair only if `configure_brand_homophones` was called; nothing in RK calls it | `rightkit-dictation-text-pipeline/src/text_polish.rs:15`, `text_polish/local.rs:76,87` | verified by reading |
| P5 | The proper-noun lexicon still carries HeardRight-specific words ("Wispr Flow", "Superwhisper", "Bandra") inside the shared crate | `rightkit-text-polish/src/text_pipeline/polish.rs` lexicon | verified; same as LIVE, so no behaviour change |

### Voice commands

LIVE: `heardright_core/src/command.rs`, `command_recognition/*`. RK reproduces catalog, matching and recognition (2, 2 and 9 changed lines, comments only) with two differences:

| # | Difference | Where | Status |
|---|---|---|---|
| V1 | On a platform that is neither macOS nor Windows, LIVE reported every command token unsupported; RK reports all but `__mac_force_quit` supported. No effect on Mac or Windows | RK `recognize.rs:136`; LIVE `recognize.rs:138` | verified by reading |
| V2 | `is_allowed_external_url` takes the allowlist as an argument. MAIN passes a longer list than LIVE had (legal pages, store account, `mailto:`) | RK `external_url.rs:14`; MAIN `heardright_core/src/external_url.rs:4-42` | verified by reading |

Command dispatch (keys, power, screenshot, app launch) is `heardright-command-host` → `rightkit-dictation-command-host`. vs LIVE it differs by 2,045 of 5,157 lines; vs MAIN by 340. I read the MAIN diff only; the LIVE diff: not verified.

### Text pipeline (L1-L3 cloud cleanup)

LIVE: `heardright-text-pipeline`. RK matches MAIN, and MAIN changed the product behaviour after LIVE:

| # | LIVE | RK (and MAIN) | Status |
|---|---|---|---|
| T1 | Groq key read from the process environment (`l3_cleanup/providers.rs:39`), with env overrides for model, base URL, roster (NVIDIA, OpenRouter) | Key only from the IPC hand-off store (`rightkit-…-text-pipeline/src/l3_cleanup/providers.rs:15`, `provider_keys.rs`); no roster, Groq ladder only | verified by reading |
| T2 | Cleanup enabled / cloud consent read from env `HEARDRIGHT_L3_CLEANUP`, `HEARDRIGHT_L3_CLOUD_CONSENT` (`lanes.rs:378,390,398`) | From the pushed runtime config, fail closed until pushed (`l3_cleanup/policy.rs:13,18`) | verified by reading |
| T3 | L1 dictation polish tried primary then fallback model (`lanes.rs:156`) | L1 is one attempt; fallback kept for prompt/summarize only (`lanes.rs:128,296`) | verified by reading |
| T4 | Any failure moved on to the next provider | An uncertain (possibly billed) failure is not replayed, except one retry on the Groq backup for prompt/summarize (`lanes.rs:307`) | verified by reading |
| T5 | `ureq` transport | `rightkit-llm` client (`state.rs` `CachedAgent`) | verified by reading |
| T6 | Min-audio / min-words gates overridable by env (`text_polish.rs:247,255`) | Fixed 3.0 s / 8 words (`text_polish.rs:272,276`); same defaults | verified by reading |
| T7 | Prompt version `l1_app_polish_prompt_15`, models `qwen/qwen3.8-27b` then `openai/gpt-oss-120b` | Same constants; prompt body line-wrapped by rustfmt only | verified for constants; byte equality of the prompt body is a suspicion-free claim only for the lines shown in the diff, the full body was not hashed |
| T8 | Acceptance guardrail | Same rules plus a diagnostics-only `rejection_rule` (`acceptance.rs` after line 87) | verified by reading |

RK-only differences from MAIN in this crate (these are the ones that matter for adoption):

| # | Difference | Where | Status |
|---|---|---|---|
| T9 | HTTP User-Agent `rightkit-dictation/0.1` instead of `heardright-engine/0.1` | RK `transport.rs:23`; MAIN `transport.rs:24` | verified by reading |
| T10 | Request header `X-RightKit-AI-Prompt-Version` instead of `X-HeardRight-AI-Prompt-Version` | RK `transport.rs:139`; MAIN `:140` | verified by reading |
| T11 | Diagnostic line prefix `RIGHTKIT_RUNTIME_DIAGNOSTIC_JSON=` instead of `HR_RUNTIME_DIAGNOSTIC_JSON=`. No Rust consumer found in either tree; script consumers not verified | RK `transport.rs:260`; MAIN `:261` | verified (strings), suspicion (impact) |
| T12 | Built-in product vocabulary ("Heard Right", "HeardRight", "Zephyr", …) is empty until `configure_product_terms` | RK `vocabulary.rs:54,63`; MAIN `vocabulary.rs:41` | verified by reading |
| T13 | Wire schema tags stay `SCHEMA:HEARDRIGHT_CLOUD_C1/C3` in RK | RK `wire.rs:153,155`, `acceptance.rs:149` | verified by reading (unchanged, good) |

### Dictation session state machine

Three layers.

Pure transitions (`state.rs`): LIVE `heardright_core/src/state.rs` and `rightkit-dictation-core/src/state.rs` are the same transition table. Only change: `TransitionError` fields are boxed (`state.rs:98`). Verified by reading. LIVE's `controller.rs` (`DictationController`, used only by the `Fake` runtime) and `ptt_policy.rs` are gone from both MAIN and the dictation crates (`ptt` now in `rightkit-hotkeys`, not verified).

Shell session (`src-tauri/src/runtime/session.rs` → `rightkit-dictation-tauri/src/runtime/session.rs`): RK follows MAIN. LIVE → MAIN changes that RK therefore also has:

| # | Change after LIVE | MAIN location | Status |
|---|---|---|---|
| S1 | History store pre-opened outside the runtime lock and adopted at first use | `session.rs:45` | verified by reading |
| S2 | Engine start carries snapshot frames and a supervisor generation; `RecordingStarted` carries `last_pasted` | `session.rs:161` | verified by reading |
| S3 | Upload-telemetry funnel calls removed; local utterance timeline marks instead | LIVE `session.rs:586`; MAIN uses `telemetry::utterance_timeline` | verified by reading |
| S4 | Streaming insertion: text already typed during recording is subtracted from the final; cancel erases it | `session.rs:680` | verified by reading |
| S5 | Windows: an unbound transcript goes to the stop destination only when the provider reported binding `Some(false)` and the stop target is not HeardRight's own process | `session.rs:697-713` | verified by reading |

RK vs MAIN in this layer: everything platform-specific (start/stop destination capture, focus pins, the Windows and macOS "original editor vs replacement editor vs recovery" delivery decision, about 350 lines at MAIN `session.rs:690-1035`) is replaced by one call, `host().deliver_final(FinalDelivery{…})` (`rightkit-dictation-tauri/src/runtime/session.rs:633`). The trait default pastes at the current foreground target (`host.rs:113`) and reports no start capture (`host.rs:129`). So RK does not reproduce HeardRight's delivery policy; it expects HeardRight to supply it. No HeardRight implementation of `DictationHost` exists in any tree. Verified by reading.

Other RK-vs-MAIN differences in this layer: `RuntimeApp::Fake` removed; `watchdog_tick` takes an emit closure instead of `tauri::AppHandle` (`runtime_lock.rs`); the engine-crash message and the microphone label no longer name the product (`runtime/app.rs`, `dictation_error.rs`); cloud-sync and scoped-delete calls are unconditional and routed to the host (defaults are no-ops). Verified by reading.

Engine worker (`heardright-runtime` → `rightkit-dictation-runtime`): vs LIVE 3,909 of 12,004 lines differ (not read against LIVE). vs MAIN the recording start/pump/finish/streaming files are identical apart from lever removal (section 4). LIVE spotted wake words through the Sherpa-ONNX C library (`heardright-inference/src/sherpa_kws.rs:10,44`); MAIN and RK both use the Rust spotter in `rightkit-asr`. Verified by reading.

## 4. Where the copies diverged: which side has what

MAIN has, RK lacks:

1. Engine command line `transcribe` / `help` / `--version` (#309): `heardright-runtime/src/cli.rs` (498 lines), dispatched at `entry.rs:31`. RK `entry.rs` has no CLI. This is a shipped feature, not a lever.
2. Injected app-data root (GATE-CONTRACT GC-A09): `heardright_core/src/app_paths.rs:33`, called at `heardright-runtime/src/entry.rs:21`. RK resolves the production folder always, from a configured name (`rightkit-dictation-core/src/app_paths.rs:26`), citing rightkit#1437. The two rules contradict each other; one must win.
3. Compile-time legal contract (`heardright_core/src/settings/blob.rs:55`, version returned as `&str` at `:71`). RK needs a runtime `configure_legal_contract` and returns `Option` (`rightkit-dictation-core/src/settings/blob.rs:68,90`).
4. Brand repair in the C polish entry point (P3).
5. Correct product strings: dump helper `heardright-dump-helper.exe` (`heardright-runtime/src/crash_capture.rs:27`; RK hard-codes `rightkit-dump-helper.exe` at `rightkit-dictation-runtime/src/crash_capture.rs:46`, although the telemetry crate made the same name configurable); engine version `heardright-engine/…` (`engine_impl.rs:41` both sides); T9-T11; model-grant host (item 9 below).
6. `tdt_beam` rank scratch buffer (thread-local reuse, perf audit ENG-14) in `heardright-parakeet-core/src/tdt_beam.rs`; `rightkit-asr/src/parakeet_core/tdt_beam.rs` allocates per call.
7. `wake_handoff.rs` (33 lines): the 40 ms handoff margin as one shared definition. RK inlines the same arithmetic in `wake_listener.rs`. Same behaviour.
8. KWS cascade features present only in `heardright-inference/src/kws_cascade.rs`: `KwsCascade`, `KwsCutSource`, `LatchedCut`, guarded confirm cut, refractory block, `KwsOnsetParams` (function and type names compared; bodies not read).
9. `heardright-engine/build.rs`: embeds the Windows package identity manifest and icon into the engine exe. The RK engine crate has no build script.
10. Experiment scaffolding that reads nothing at run time: `perf_levers.rs` (318), `ipc/l3_live.rs` (98), `l3_cleanup/overrides.rs` (102), `live_eval_release.rs` (38), `CutResolverSwitches`, `coreml/aligned_probe.rs` (336), `verify_receipt.rs` (120), `PredictorExperiment`, `WhisperWinTuning`. In MAIN every one of these returns its default (`overrides.rs` header: "Every override is absent"; `perf_levers.rs` header: "No switch is read at run time"). This is dead weight, not a fix.

RK has, MAIN lacks:

1. Host configuration points replacing hard-coded HeardRight values: app-runtime `host::configure` (`host.rs:26`), command-host `host::configure` (`host.rs:30`), telemetry `configure_dump_helper_name` / `configure_feedback_endpoint`, models `configure_bundle_identifier`, inference `configure_app_display_name`, runtime `configure_crash_identity`, text-pipeline `configure_brand_homophones` / `configure_product_terms`, core `configure_app_data_dir_name` / `configure_legal_contract`, tauri `install_host` (`host.rs:218`).
2. Item 10 above already removed.
3. Speech-service log for an unidentified peer (`rightkit-dictation-runtime/src/speech_service/transport.rs:61`, `Accepted::Unidentified`). Depends on a newer `rightkit-daemon` than MAIN's =0.5.1 (suspicion: version requirement not checked).
4. `legacy_compatibility_required(app_version)` takes the version instead of `include_str!` of the app's `package.json` (`rightkit-dictation-models/src/shared.rs:24`).
5. KWS features present only in `rightkit-dictation-inference/src/kws.rs`: trace capture, fricative onset search, shared beam / shared front end, decoder table report, stale-gap guard (names compared; bodies not read).
6. Leftovers that MAIN has already deleted: `spawn_dummy_worker` (`rightkit-dictation-runtime/src/worker/worker_api.rs:791`) and an uncalled `SpeechVad::calibrate` (`rightkit-dictation-inference/src/vad.rs:118`, no caller in any RK crate). The first contradicts the "no dummy" rule quoted in RK's own changelogs.

Bugs introduced by the move into RK:

- Model grant host renamed: `DEFAULT_WORKER_HOST = "rightkit-models.adrdsouza.workers.dev"` (`rightkit-dictation-models/src/model_download_grant.rs:10`) while the download URLs in the same crate still point at `heardright-models.adrdsouza.workers.dev` (`ultra_model.rs:78,83`, `whisper_model.rs:73,86`). Effect, read from lines 57-62: the real production host no longer equals the default, so it is treated as a development override and gets `parity_client_allowing_host`, the client that is allowed to resolve to local or private addresses. Verified by reading; runtime effect not exercised.
- Env name `RIGHTKIT_MODEL_WORKER_BASE` replaces `HR_MODEL_WORKER_BASE` (`resumable_download.rs:93-95`); the feature gate around it: not verified.

## 5. No-duplicate end state

Owner of every duplicated module: RightKit. HeardRight keeps: the two binaries' `main` functions, the Tauri command table and window/pill/tray/hotkey code, its product data (brand table, allowlist, legal contract, names, endpoints), and one `DictationHost` implementation over its existing `src-tauri/src/delivery`, `history`, `windows_uia`, `streaming_insertion`, `cloud_sync`, `media_control`, `settings`, `vocabulary` modules.

A constraint that fixes the order: the RK crates depend on each other by their RK names and hold process-wide state in statics (pushed settings and L3 policy in `rightkit-dictation-text-pipeline/src/settings.rs:13`, provider keys, vocabulary terms, engine paths in `rightkit-dictation-inference/src/engine_paths.rs:37,91`, the model activation in `rightkit-dictation-models/src/shared.rs:54`). If HeardRight switched an upper crate while a lower one was still local, the process would hold two copies of that state: the runtime would push config into one and inference would read the other. So the switch has to go bottom-up along RK's dependency graph, and a switched HeardRight crate should first become a one-line re-export of its RK twin so that the crates above it keep compiling unchanged.

RightKit changes needed first (nothing below can land without them):

| # | Change in RightKit | Why |
|---|---|---|
| A1 | `rightkit-dictation-runtime`: port `cli.rs` and its dispatch in `entry::run` (or expose a hook that runs before tracing/ORT setup). The CLI names `HeardRightLauncher`; the launcher must be host-supplied | MAIN feature #309 |
| A2 | `rightkit-dictation-runtime`: `crash_capture` takes the dump helper name from configuration (reuse the telemetry crate's setting); engine version prefix from configuration | wrong helper exe name, wrong version string |
| A3 | `rightkit-dictation-text-pipeline`: User-Agent, prompt-version header name and diagnostic prefix host-configurable, or restored to the HeardRight values | T9-T11 |
| A4 | `rightkit-dictation-models`: restore `DEFAULT_WORKER_HOST` to the real host (or derive it from the archive URL) | grant-host bug |
| A5 | `rightkit-dictation-ffi`: polish entry point takes or looks up the brand table; decide whether C symbols stay `heardright_core_*` for the existing Swift callers or the Swift side renames to `rightkit_dictation_*` | P3; symbol rename at `lib.rs:61-242` |
| A6 | `rightkit-dictation-core`: settle app-data root (injected root per GC-A09, or configured name per rightkit#1437) and expose whichever wins; keep `configure_legal_contract` | section 4, MAIN item 2 |
| A7 | `rightkit-dictation-inference`: reconcile `kws.rs` with `kws_cascade.rs` into one file. This is the largest single piece of real divergence and I could not determine from reading which side carries the currently qualified behaviour | section 4 |
| A8 | `rightkit-asr`: take MAIN's `tdt_beam` scratch buffer | perf fix |
| A9 | `rightkit-dictation-engine`: its `main.rs` calls `entry::run()` with no configuration step, so its binary can never be HeardRight's engine. Either drop the `[[bin]]` and keep the library facade, or give `run` a config argument | section 3 P4, T12 |
| A10 | Remove `spawn_dummy_worker` and the uncalled `SpeechVad::calibrate` | dead code MAIN already removed |
| A11 | Publish every `rightkit-dictation-*` crate (MAIN's workspace forbids a `[patch]`: `Cargo.toml:33`) | none of the ten is consumable today |

Order in HeardRight (each step leaves the workspace building):

| Step | HeardRight change | Deleted |
|---|---|---|
| 1 | `heardright_core`: use `rightkit_dictation_core::{settings, app_paths}`; call `configure_legal_contract(include_str!(legal-contract.json))` and the data-dir call in both `main`s. Adapt the two `Option` call sites | `heardright_core/src/settings/*`, `app_paths.rs` (about 910 lines) |
| 2 | `heardright_core::ffi`, `kws_ffi` → `rightkit-dictation-ffi` (after A5) | `ffi.rs`, `kws_ffi.rs` (786 lines) |
| 3 | `heardright-telemetry` → re-export of `rightkit-dictation-telemetry`; configure dump helper and feedback endpoint (`https://heardright.app/v1/feedback`, prefix `x-heardright-`) | crate body (1,353 lines) |
| 4 | `heardright-parakeet-core`, `heardright-onnx-asr` → `rightkit_asr::{parakeet_core, onnx_tdt}` (after A8; onnx diff not verified) | both crates (2,889 lines) |
| 5 | `heardright_platform` → `rightkit_automation::desktop` | crate (7,811 lines; not verified here) |
| 6 | `heardright-model-store` → `rightkit-dictation-models` (after A4); configure bundle id `app.heardright.next`; pass app version to `legacy_compatibility_required` | crate (3,120 lines) |
| 7 | `heardright-text-pipeline` → `rightkit-dictation-text-pipeline` (after A3); configure brand table and product terms in the engine `main` | crate (4,788 lines) |
| 8 | `heardright-command-host` → `rightkit-dictation-command-host`; configure `HostIdentity` (bundle id, prefix `heardright`, titles "HeardRight Pill/Recent/Hint", name) | crate (5,937 lines) |
| 9 | `heardright-inference` → `rightkit-dictation-inference` (after A7); configure display name | crate (20,323 lines) |
| 10 | `heardright-runtime` → `rightkit-dictation-runtime` (after A1, A2) | crate (15,805 lines) |
| 11 | `heardright-engine` shrinks to `main.rs` (configure calls, then `rightkit_dictation_runtime::entry::run()`), `build.rs`, entitlements. Examples move with the RK engine crate | `src/lib.rs` facade, examples |
| 12 | `heardright-app-runtime` → `rightkit-dictation-app-runtime`; configure `EngineHost { binary_stem: "heardright-engine", bundle_identifier: "app.heardright.next", app_resource_dir: "heardright" }` | crate (2,425 lines) |
| 13 | `src-tauri`: add `impl DictationHost` (move MAIN `runtime/session.rs:690-1035` and the start/stop capture bodies into it, over the existing `delivery` module), `install_host` at startup, then use `rightkit_dictation_tauri::{runtime, runtime_lock, start_sequence, dictation_error, engine_event_consumer}` | `src-tauri/src/runtime/*`, `runtime.rs`, `runtime_lock.rs`, `start_sequence.rs`, `dictation_error.rs`, `engine_event_consumer.rs` (about 4,600 lines) |
| 14 | Remove the re-export shims and `heardright_core`'s pass-through modules; rewrite `heardright_core::…` imports to the RK crates | the shim crates |

Steps 3-5 are independent of each other. Steps 6-10 must run in that order (RK dependency order: models needs telemetry; text-pipeline needs core; command-host needs text-pipeline; inference needs text-pipeline; runtime needs all four). Step 12 needs 3 and 6. Step 13 needs 3, 6, 8 and 12.

Cannot move as is: the shell delivery policy (step 13) has no RK home by design; it stays HeardRight code behind the trait, which is consistent with "wiring only" only if the owner accepts about 350 lines of destination policy as host code. If not, the policy needs a home in `rightkit-dictation-command-host` plus `rightkit-automation`, which is a new RK API.

## 6. Post-LIVE changes: deliberate or regression

Deliberate (documented in code or changelog, and consistent across MAIN and RK):

- Sherpa-ONNX C library replaced by the Rust spotter (`rightkit-asr` 0.4.0 changelog).
- Upload telemetry removed; telemetry crate 6,073 → 1,353 lines; local utterance timeline added (S3).
- Provider keys over IPC hand-off, policy pushed in config, no environment reads (T1, T2; rightkit#886).
- L1 single attempt and the no-replay rule with its dated exception (T3, T4; "Adrian, 2026-10-31" in `lanes.rs`).
- `ureq` → `rightkit-llm` (T5).
- Streaming insertion (S4), prewarmed history (S1), tightened Windows stop-destination rule (S5).
- Engine CLI #309.
- `heardright_capture` crate dropped for `rightkit-audio`; `controller`/Fake runtime dropped; unit tests and `test_mode()` dropped (owner statements dated 2026-11-03 in the RK changelogs).
- Longer external-link allowlist (V2).
- ONNX duration cap 2 → 4 with measurements cited (`onnx_beam_decode.rs`).

Look like regressions or unintended (all on the RK side; none found in MAIN by reading):

- Grant host rename (section 4). Verified.
- UA / header / diagnostic prefix rename (T9-T11). Verified strings; server-side dependence on them is a suspicion.
- Hard-coded `rightkit-dump-helper.exe` in the runtime while the telemetry crate is configurable. Verified.
- FFI polish without brand repair (P3). Verified.
- `spawn_dummy_worker` still present. Verified.

Needs an owner decision, not classifiable from the code:

- Release profile `panic = "abort"` (LIVE `Cargo.toml:45`) became `panic = "unwind"` (MAIN `Cargo.toml:52`). The engine's panic hook says it records and then aborts, so the engine should behave the same; the shell's behaviour on a thread panic: not verified.
- App-data root rule (GC-A09 vs rightkit#1437).
- Which KWS wrapper is current.
- V1 (non-desktop platforms now report commands supported).

## 7. Verdict per RightKit crate

| Crate | Verdict | Gaps |
|---|---|---|
| `rightkit-text-polish` | right shape for HeardRight | in use; none |
| `rightkit-voice-commands` | right shape for HeardRight | in use; V1 is Linux-only |
| `rightkit-transcript-library` | right shape for HeardRight | in use; LIVE diff not read line by line |
| `rightkit-transcript` | right shape for HeardRight | in use (subtitle export only) |
| `rightkit-speech-client` | right shape for HeardRight | in use; names `HeardRightLauncher` / `HostRole::HeardRight*` by design |
| `rightkit-dictation-core` | right shape with listed gaps | `settings` and `app_paths` duplicated in MAIN; A6 |
| `rightkit-dictation-telemetry` | right shape for HeardRight | needs the two configure calls |
| `rightkit-dictation-app-runtime` | right shape for HeardRight | needs `host::configure` |
| `rightkit-dictation-models` | right shape with listed gaps | A4; env name change |
| `rightkit-dictation-text-pipeline` | right shape with listed gaps | A3; brand and product terms must be configured |
| `rightkit-dictation-command-host` | right shape with listed gaps | can only land together with `heardright_platform` → `rightkit-automation` (step 5); LIVE diff not read |
| `rightkit-dictation-inference` | right shape with listed gaps | A7 (KWS), A8, A10 |
| `rightkit-dictation-runtime` | right shape with listed gaps | A1 (CLI), A2, A6, A10 |
| `rightkit-dictation-engine` | right shape with listed gaps | A9: library facade fine, binary unusable by a host; no Windows identity build step |
| `rightkit-dictation-tauri` | right shape with listed gaps | HeardRight must write `DictationHost`; default delivery is not HeardRight's policy |
| `rightkit-dictation-ffi` | right shape with listed gaps | A5 |
| `rightkit-library` | not part of this chain | no HeardRight code in it, no HeardRight consumer |

No crate is the wrong shape. The module boundaries are HeardRight's own; what is missing is the last 2-6% per crate plus the KWS reconciliation, and then the actual switch, which has not started above the core layer.

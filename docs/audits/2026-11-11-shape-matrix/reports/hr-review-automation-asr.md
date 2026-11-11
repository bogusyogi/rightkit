# HeardRight vs RightKit: `rightkit-automation`, `rightkit-asr`, `rightkit-vad`

Read-only comparison by reading source. Nothing was compiled or run, so every "verified" below means "verified by reading the code", not by execution.

Trees (paths below are relative to these roots):

- LIVE = `<scratch>/survey/heardright-live/tauri-app-next` (0.1.213, `12b7d8ce`)
- MAIN = `<scratch>/survey/heardright-main/tauri-app-next` (0.1.231)
- RK = `/home/user/rightkit/crates`

Version note: MAIN pins `rightkit-automation =0.6.7`, `rightkit-asr =0.4.0`, `rightkit-vad =0.1.2`, all resolved from the registry (`MAIN/Cargo.lock:4334,4378,4777`). The RightKit source tree I read is one or two patch versions ahead (automation 0.6.8, asr 0.4.2, vad 0.1.3). Per the changelogs the deltas are docs, removed tests and removed env switches, plus one behaviour change in asr 0.4.2 (Windows duration cap 2 to 4, see ASR section). I did not read the published 0.6.7 / 0.4.0 / 0.1.2 tarballs.

## Verdicts

| Package | Verdict | Reason |
|---|---|---|
| `rightkit-automation` | **Right shape with listed gaps** (primitives right, delivery orchestration absent, generic `deliver_text` wrong for dictation) | The `desktop` module is MAIN's `heardright_platform` almost line for line, and its proof, pin and clipboard logic is the live logic. But MAIN does not call it, the code that actually delivers a transcript (about 6k lines in `src-tauri`) has no home in this crate, and the crate's own `Automation::deliver_text` does things the live app deliberately refuses to do. |
| `rightkit-asr` | **Right shape with listed gaps** | The keyword spotter and the decode core (detok, policy, scoring, beam pruning) are in use on MAIN with the live constants. The Mac CoreML engine, the Windows ONNX execution and both Whisper engines are still HeardRight code; the RightKit equivalents exist but lack things live HeardRight uses (command alignment, window health, CLI fallback). |
| `rightkit-vad` | **Right shape for HeardRight** | MAIN already runs its production VAD on `rightkit_vad::Silero` + `FrameStream` with the live session settings and threshold. The only live behaviour not carried is an env-gated calibration that was off by default. |

---

## 1. `rightkit-automation`

### 1.1 What live HeardRight does

One delivery path per OS, in the shell process:

- Entry: `RealDeliveryBackend::deliver*` (`LIVE/src-tauri/src/delivery/backend.rs:14-127`), body `deliver_inner_with_progress` (`:129-302`), platform `insert_transcript` (macOS `:533-957`, Windows `:959-1220`).
- **Paste only, one dispatch.** "Single dispatch-only path ... ONE clipboard + Cmd+V sequence. No native AX write precedes it" (`backend.rs:594-598` macOS, `:980` Windows). The `keystroke` setting is retired: it returns `synthetic_insertion_unverifiable` and falls back to clipboard (`:587-593`).
- **Clipboard transaction.** Snapshot the prior pasteboard, stage the transcript as a *promised* clipboard item, mark posted, send the chord, wait for the provider callback (the "receipt"), then restore the prior content only if nobody else wrote meanwhile (`backend.rs:600-626`, `:733-742`, `:943-949`; primitives in `LIVE/heardright_platform/src/macos/clipboard.rs` and `windows.rs`). Windows refuses the whole transaction when the prior clipboard cannot be snapshotted losslessly, and then does not even write the fallback copy (`backend.rs:985-993`, `:272-280`).
- **Secure fields.** Positive secure-field evidence is the only veto, checked before staging and again just before dispatch; unknown never blocks (`backend.rs:571-584`, `:638-648` macOS; `:969-973`, `:1052-1061` Windows). Evidence is the AX role (macOS) or UIA `IsPassword` (Windows). Neither LIVE nor MAIN calls `IsSecureEventInputEnabled`.
- **Focus.** The destination is captured at stop time (`delivery/targets.rs`: `capture_stop_target_macos` `:344`, `capture_stop_target_windows` `:115`) and revalidated repeatedly up to the instant before the chord (`backend.rs:628-634`, `:649-654`, `:683-688`, `:711-716`). A transient AX miss is retried, a real change sends the transcript to the clipboard (`:195-220`). Always-on focus observer started at launch (`LIVE/src-tauri/src/app_run.rs:214,329`).
- **Success rule (strict).** Mac: receipt required (`:795-803`), then exact AX read-back required; a blind target (terminal grid, opaque web editor) ends as `PasteSentUnconfirmed` (`:805-888`, `:261-270`). Windows: 40 ms receipt wait (`:332`, `:1110`), then UIA read-back required (`:1121-1166`).
- **Enter (paste-and-send).** Only after confirmed read-back and a fresh non-secure, same-editor check (`:898-941` macOS, `:1178-1206` Windows).
- **Per-app handling.** Terminal bundle list (`heardright_platform/src/macos/focus/context.rs:624-636`), Slate/ProseMirror placeholder and line-break canonicalisation (`context.rs:646+`, `lib.rs`), Chromium prewake (`macos/observation.rs`), Chromium/Electron UIA identity being advisory (`backend.rs:1024-1037`), WezTerm pane binding (`src-tauri/src/windows_wezterm.rs`), VS Code terminal rows and a persistent UIA worker (`src-tauri/src/windows_uia/worker.rs`, raw `windows` crate UIA), a TSF text-input processor (`tsf-tip/`), a CGEventTap paste probe (`src-tauri/src/paste_event_probe.rs`).
- **Media control.** Pauses and resumes the *identified* playing session and confirms the state after each command: GSMTC on Windows (`src-tauri/src/media_control/win.rs:53-130`), `MRMediaRemoteSendCommand` plus an `osascript` probe on macOS (`media_control/mac.rs:55-250`), shared lifecycle in `media_control/backend.rs`.
- **Streaming delivery.** Does not exist in the live build. `streaming_insertion.rs` is absent from LIVE and no Rust file mentions streaming insertion (verified by search).
- Platforms: macOS and Windows for everything above.

### 1.2 What `rightkit-automation` provides

- **Engine** (`RK/rightkit-automation/src/lib.rs`, `engine.rs`, `delivery.rs`): `Automation<P>` over a `Platform` trait, four-tier input ladder, allowlist policy, kill switch, leases, admission hook, receipts. `Automation::capture_pin`, `pin_is_current`, `replacement_pin`, `deliver_text` (`delivery.rs:115-260`).
- **Raw primitives**: `macos::ax` (`El`, `AxOptions`, attribute reads), `win::uia` (`El`, `focused_element_is_password`, `focused_field_context`, `terminal_rows`, `point_is_hyperlink`), `win::{send_input_batch, RawInput, with_deadline, with_deadline_security}`, `uia_text`, `send_media_key`, `clipboard`, `screen`, `capture_safety`.
- **`desktop` module, feature `desktop-context`** (`src/desktop.rs`, `src/desktop/**`, about 6.9k lines): focus continuity capture and validation, promised clipboard with receipt on both OSes, full-format Windows snapshot/restore, frontmost-window identity, AX focus pin and proof, native write, window text harvest, paste/enter chords, unicode typing. Added in 0.6.4 "moved from HeardRight `heardright_platform`" (`CHANGELOG.md`, 0.6.4 entry).
- Platforms: macOS and Windows only.

### 1.3 Behaviour differences

| # | Difference | Live | RightKit | Status |
|---|---|---|---|---|
| A1 | `desktop` module equals MAIN's platform crate, not LIVE's | n/a | Every file in `RK/rightkit-automation/src/desktop/` differs from `MAIN/heardright_platform/src/` only by path prefixes, brand strings and formatting (0 to 40 changed lines per file; e.g. `windows.rs` 18, `macos_focus_continuity.rs` 20, `harvest.rs` 0). Against LIVE the same files differ by 12 to 807 lines. | Verified (diffed all 15 files) |
| A2 | Proof, pin, native-write and Mac clipboard logic | `LIVE/heardright_platform/src/macos/focus/{pin,proof,native_write}.rs`, `macos/clipboard.rs` | Same decisions; the only code changes are AX calls routed through `rightkit_automation::macos::ax` helpers and removed unit tests | Verified (whitespace-insensitive diff, non-comment lines) |
| A3 | Own-window exclusion needs a new call | Hard-coded: window owner name starts with `heardright` (`LIVE/heardright_platform/src/macos/frontmost.rs`, same in MAIN) | `super::own_window_owner_prefix()`; when unset, **no** window is excluded (`RK/.../desktop/macos/frontmost.rs:106`, setter `desktop/macos/mod.rs:13`) | Verified. HeardRight must call `set_own_window_owner_prefix("heardright")` at startup or its pill can become a delivery destination. |
| A4 | Generic `deliver_text` writes natively before pasting | Forbidden: "No native AX write precedes it" (`LIVE backend.rs:594-598`) | Tier 1 `native_write` runs first when `max_tier >= Semantic` (`RK delivery.rs:406-443`) | Verified |
| A5 | Generic `deliver_text` types text as events | Synthetic typing retired as unverifiable (`LIVE backend.rs:587-593`) | Tier 3 posts `InputEvent::Text` to the pid (`RK delivery.rs:445-512`) | Verified |
| A6 | Generic `deliver_text` does not paste by default | Paste is the only path | Default `max_tier: Tier::Targeted`; clipboard paste is tier 4, opt-in (`RK delivery.rs:50-58`, `:514-521`) | Verified |
| A7 | No receipt in the generic path | Promised-clipboard receipt is the consumption proof (`LIVE backend.rs:733-803`) | `paste_foreground` uses plain `clip_write` and decides on AX/UIA read-back alone (`RK delivery.rs:580-660`). The receipt exists only in `desktop` (`desktop/macos/clipboard.rs:315,377`, `desktop/windows.rs:322,419`). | Verified |
| A8 | Generic path activates the target app | Never activates; refuses if focus moved | `restore_front(pin.pid)` before the chord, `give_back_front` after (`RK delivery.rs:604`, `:627`) | Verified |
| A9 | Allowlist | Delivers into any app | `Policy::check` refuses any app not on the allowlist; "Empty denies all" (`RK policy.rs:56`, `:98-123`). `com.anthropic.claudefordesktop` is in `ALWAYS_DENIED` (`policy.rs:28-39`), so the engine can never deliver into Claude desktop, which live HeardRight names as a common target (`LIVE backend.rs:1029`). | Verified. Applies to the engine only, not to `desktop` or the raw `ax`/`uia` calls. |
| A10 | Delivery orchestration is not in the crate | `backend.rs` 1,442, `targets.rs` 968, `windows_uia/` ~1,170, `windows_wezterm.rs` 453, `paste_event_probe.rs` 303, `tsf-tip` ~900 | No equivalent: no `RealDeliveryBackend`, `WindowsStopTarget`, `PasteEventProbe`, WezTerm pane binding or TSF tip anywhere in `rightkit-automation` (searched). `docs/MOVED_FROM.md` states "Dictation delivery policy and context binding stay in HeardRight" and `docs/LOCKED_RULES.md` lists TSF, stop-target selection and WezTerm pane binding as product rules. | Verified. The inventory nevertheless targets these files at `rightkit-automation` with action EXTEND. |
| A11 | Media control | Explicit pause/play of an identified session, state confirmed after the command | Only `send_media_key(MediaKey::PlayPause)`: a system-wide toggle with no player identity and no state read (`RK media_keys.rs:96`). `rightkit-now-playing` publishes the host's own playback; it does not control other players. | Verified. Using the toggle would resume a paused player or pause the wrong one. Inventory marks media control NEW (`rightkit-media-control`), so this is a known hole. |
| A12 | Secure-field rule | Positive evidence is the only veto | Same rule in the engine (`RK delivery.rs:386-390`) and same functions in `desktop` (`desktop/macos/focus/context.rs:364`) | Verified, matches |
| A13 | Lossy clipboard snapshot | Refuse, do not overwrite (`LIVE backend.rs:985-993`) | Engine: `Why::ClipboardLossy` (`RK delivery.rs:591-597`); `desktop`: same snapshot type as MAIN | Verified, matches |
| A14 | Focus observer lifetime | Always on from launch | On demand, only between `begin_focus_coverage` and `end_focus_coverage` (`RK desktop/macos_focus_continuity.rs:11-16`; exports in `desktop.rs:22-32`) | Verified. This is MAIN's shape (see 1.5). |
| A15 | Streaming delivery | Not in live | `desktop` carries the typing primitives MAIN uses (`type_unicode_text`, `type_unicode_chunked`, `send_backspaces`); the reconcile logic is app code | Verified |
| A16 | macOS web-area ancestor walk (locked rule 2), AXObserver in the engine, single persistent UIA worker | Present in live | Listed as gaps for the *engine* in `docs/MOVED_FROM.md` and `LOCKED_RULES.md`. The `desktop` module does contain the AXObserver-based observer. | Not verified beyond the docs for the engine side |

### 1.4 What MAIN calls today

Both, split by layer:

- **Calls RightKit for raw primitives** (26 references): `MAIN/src-tauri/src/windows_uia/worker.rs:9-11,27` and `protocol.rs:9` (`win::uia::El`, `uia_text`), `src-tauri/src/macos_input.rs:13` and `pill/state/macos_events.rs:34` (`macos::ax`), `src-tauri/src/middle_mouse_win.rs:22,182`, `heardright_platform/src/macos/ffi.rs:10`, `macos_focus_continuity.rs:465-467`, `windows.rs:11` (`send_input_batch`), `heardright-command-host/src/focus.rs:457,470,489,499,541,728` (`with_deadline*`, password and field probes), `command_dispatch/keys.rs:7,137-153,672` (`RawKey`, `send_media_key` for voice-command media keys), `command_dispatch/mouse.rs:15`.
- **Does not call `rightkit_automation::desktop`**. `MAIN/heardright_platform/Cargo.toml:13,33` pins the crate with `default-features = false` and no `desktop-context`; the app keeps its own near-identical copy of all 15 files.
- **Does not call the engine** (`Automation`, `deliver_text`, `capture_pin`, `Policy`): no reference anywhere in MAIN.
- Delivery orchestration, WezTerm, UIA worker protocol, paste probe, TSF, media pause/resume and streaming insertion are all app code on MAIN.

Related, not reviewed here: `MAIN/heardright_core/src/lib.rs:35` re-exports `delivery` from `rightkit-transcript-library` (the old `heardright_core/src/delivery.rs`, including `paste_outcome`), and `RK/rightkit-dictation-command-host/src/delivery/` uses `automation::desktop`. Those belong to other packages.

### 1.5 Changes after the live build

Deliberate (each has an explaining comment or changelog entry):

- **Receipt-first success** instead of read-back-required. `paste_outcome`: success = keystroke sent AND (receipt OR (target key event AND read-back)); "No shipping dictation app gates success on AX/UIA readback" (`RK/rightkit-transcript-library/src/delivery.rs:938-959`; used at `MAIN backend.rs:1008,1504`). The live 40 ms Windows receipt constant is gone.
- **Enter no longer requires read-back**: after a receipt MAIN waits up to 400 ms for the insertion to land (60 ms when blind), then sends Return if the proven editor is current and non-secure (`MAIN backend.rs:378`, `:1040-1124`). Live required confirmed read-back (`LIVE backend.rs:898`). This is a real loosening of a safety gate; it reads as intentional but deserves an explicit owner confirmation.
- **Clipboard restore moved to a background thread** with a quiet window (200 ms after the last receipt, capped at 1 s), and the next delivery waits for it (`MAIN backend.rs:342`, `:385-432`, `:1137-1175`). Live restored inline.
- **Terminals with no confirm channel** report "sent, consumption unknown" instead of holding the pill (`MAIN backend.rs:1422`).
- **WezTerm pre-paste pane probe** (`MAIN backend.rs:1335-1347`; `windows_wezterm.rs` 453 to 702 lines).
- **Paste chord after hotkey with physical-modifier settling** on Windows (`MAIN backend.rs:1875-1877`; `heardright_platform/src/windows.rs` new `send_paste_chord_after_hotkey`, `settle_physical_modifiers`).
- **On-demand focus observer** (no periodic wake at idle, issue #293).
- **UIA and AX plumbing moved onto RightKit** (`windows_uia/worker.rs` no longer uses the raw `windows` crate).
- **Streaming insertion** added as a Developer-mode feature (`MAIN/src-tauri/src/streaming_insertion.rs:1-7`).
- **Unit tests removed** from the platform crate (owner decision 2026-11-03, recorded in `RK/rightkit-automation/CHANGELOG.md` 0.6.5).
- Middle-mouse handling moved out to `rightkit-hotkeys` (`middle_mouse.rs` 260 to 42 lines).

Possible regressions: none found by reading. Not verified by running either build.

### 1.6 Verdict

**Right shape with listed gaps.** The part of the crate HeardRight would actually adopt, `desktop`, reproduces the live proof, pin, secure-field and clipboard behaviour and is byte-near to MAIN; adoption is a path rename plus one new startup call (A3). The gaps are that MAIN has not switched to it, that roughly 6k lines of delivery orchestration and all of media pause/resume have no RightKit home yet, and that the crate's headline `Automation::deliver_text` must not be used for dictation (A4 to A9).

---

## 2. `rightkit-asr`

### 2.1 What live HeardRight does

- One runtime enum, `AsrRuntime` (`LIVE/heardright-inference/src/asr/runtime.rs:425-452`): `Parakeet(OnnxAsr, ..)` on Windows, `CoreMlParakeet` on macOS, `SherpaKws`, `ProbeDisabled`, `WhisperCoreMl` (macOS), `WhisperWin` (Windows).
- **Models** (`asr/models.rs:10-17`): Parakeet unified static-15 s (`unified_static15_b128_sym_bits4_timestamp_hybrid` ONNX, `parakeet-unified-en-0.6b` CoreML), Parakeet TDT v3 (`parakeet_tdt_v3_static1500_qint8_20260722`, `parakeet-tdt-v3`), Whisper multilingual (CoreML bundle on Mac, whisper.cpp ggml on Windows).
- **Execution**: Mac is native CoreML through `objc2-core-ml` (`coreml_asr/`, `coreml/stage.rs`, `coreml_prediction_bridge.m`); Windows is ONNX Runtime with DirectML or CPU through the in-repo `heardright-onnx-asr` crate (420-line `model.rs`) and `onnx_beam_decode.rs`.
- **Decode policy** in `heardright-parakeet-core`: beam width 2, emit reward 0.15 (Mac) / 0.10 (Windows), Windows duration cap index 2 (`policy.rs:8,17,21`; cap read at `onnx_beam_decode.rs:40-43`), context bias, N0 filler detok.
- **Batch plus scheduled windows, not token streaming**: final decode per utterance (`transcribe_final_result_under_lease` `runtime.rs:824`), background 15 s window commits while recording (`ScheduledStatic15` `:1179-1413`), file transcription (`:1078`), probe decodes for commands (`:875-950`).
- **Command alignment and window health** on the CoreML path (`coreml_asr/command_alignment.rs` 482 lines, `window_health.rs` 403 lines; exported in `coreml_asr.rs:20-28`).
- **Wake word and commands**: `sherpa_kws.rs` loads the Sherpa-ONNX C library with `libloading` (`:44-46`, `:143-155`), a patched 1.13.4 build (keyword-path TTL 38 frames, `LIVE/src-tauri/resources/kws/manifest.json:9-15`). Three spotters (wake, confirm, hey-wake with a runtime threshold), CPU, 1 thread, 8 active paths, 0 trailing blanks, score 1.0, threshold 0.25 (`sherpa_kws.rs:16-20`), max keyword span 2 s (`:37`).
- **Whisper on Windows**: resident whisper.cpp context over FFI, with a per-utterance `whisper-cli.exe` fallback after failures (`whisper_win.rs:1-10`, `:217`).
- Surface the rest of the app calls: `AsrRuntime::{load, load_probe, transcribe*, start_idle_wake, feed_idle_wake, stop_idle_wake, reset_probe_stream, transcribe_file}`, `SpeechVad`, `calibration`, `file_transcribe`, `control_cleanup`, `trigger_audio_cut`.

### 2.2 What `rightkit-asr` provides

- `kws` (`RK/rightkit-asr/src/kws/`, about 5.4k lines): Rust port of the Sherpa-ONNX 1.13.4 keyword spotter running the same three ONNX files on `rightkit-onnx`. `KeywordSpotter::{new, with_model, create_stream, is_ready, decode, result, detection, reset}`, `KeywordStream::accept_waveform`, `KwsModel::load`, onset estimation. `active_path_ttl_frames` and `trailing_blank_reset` are config (`kws/mod.rs:164,168`). CPU only, 16 kHz.
- `parakeet_core`: `backend`, `beam`, `detok`, `policy`, `scoring`, `types`, plus `bias`, `tdt_beam`, `command_alignment`.
- `onnx_tdt` (feature `onnx-tdt`): the ONNX execution moved from `heardright-onnx-asr`.
- `parakeet_coreml` (macOS): `CoreMlParakeet` with beam, context bias, long-form.
- `whisper`: CoreML (macOS) and whisper.cpp via dlopen on all desktop OSes, "no CLI fallback" (`whisper/mod.rs:3`).
- Also Nemotron, unified streaming, EOU, CTC, Cohere, multitalker, Sortformer, long-form, routing. None of these is used by HeardRight.

### 2.3 Behaviour differences

| # | Difference | Live | RightKit | Status |
|---|---|---|---|---|
| S1 | Keyword spotter backend | Patched Sherpa-ONNX C library | Rust port, TTL as config | Verified. Deliberate (decision 2026-11-01). |
| S2 | KWS constants | threads 1, paths 8, trailing blanks 0, score 1.0, threshold 0.25, TTL 38, span 2 s | MAIN passes the same values into the port (`MAIN/heardright-inference/src/kws_cascade.rs:36-40`, `:63`, `:149-166`, `:2122-2155`). Port defaults differ (`max_active_paths: 4`, no TTL; `RK kws/mod.rs:249-254`) but MAIN overrides every one. | Verified |
| S3 | KWS model and keyword files | `resources/kws/*` | Encoder, decoder, joiner, tokens, `keywords.txt`, `keywords-wake.txt`, `keywords-hey-wake.txt` are byte-identical LIVE vs MAIN. `keywords-confirm.txt` differs: `ZEPHYR_STOP` threshold `#0.15` to `#0.20`. | Verified (cmp). The threshold change is app data, not RightKit; whether it was deliberate is not established from the code. |
| S4 | KWS numerical parity | Native Sherpa build | Changelog claims exact detections and timestamps when Sherpa's own filterbank frames are injected, and float-rounding differences from raw audio that "can move a token timestamp on a near-tied path" (`RK/rightkit-asr/CHANGELOG.md`, 0.3.1 entry) | Not verified. The parity test and fixtures it names are not in the tree I read (no `tests/` directory in `RK/rightkit-asr`). The owner's "Mac & Windows are tested" is the only evidence. |
| S5 | Mac CoreML engine | Command-locator alignment, window-health telemetry, hypothesis batching, fused graph | `parakeet_coreml` header: "Not ported from HeardRight ...: fused decoder+joint graph, hypothesis batching, frontend look-ahead worker, output-backing probes, window-health telemetry, command-locator alignment" (`RK parakeet_coreml/mod.rs:27-30`) | Verified. `CoreMlParakeet` cannot replace the live engine: spoken-command cleanup depends on alignment (`LIVE runtime.rs:510-565`). |
| S6 | Windows duration cap | 2 (`LIVE policy.rs:21`) | 0.4.0 (pinned): 2. Source tree 0.4.2: 4 (`RK parakeet_core/policy.rs:25`) | Verified. MAIN already overrides locally to 4 (`MAIN onnx_beam_decode.rs:95-105`), so a bump to 0.4.2 changes nothing for MAIN but both differ from live. |
| S7 | Emit reward, beam width, detok | 0.15 / 0.10, width 2, N0 filler strip | Same constants (`RK policy.rs:8,17,105-110`); detok refactored to index-based with the same grouping logic | Verified for constants; detok equivalence read, not run |
| S8 | Whisper Windows fallback | CLI fallback after FFI failures | None by design | Verified. Loss only if HeardRight adopts `rightkit_asr::whisper`; it has not. |
| S9 | Verified whisper load on Windows | n/a | `WhisperCpp::load_verified` fails closed on Windows because the catalog rows are `planned` (changelog 0.3.1) | Not verified in code; taken from the changelog |
| S10 | ONNX execution copy has diverged | `heardright-onnx-asr/src/model.rs` 420 lines | `RK onnx_tdt/model.rs` 1,169 lines vs MAIN's own copy 1,361 lines (202 differing lines). 0.4.2 removed `PredictorOutput` and the split-predictor path, which MAIN's copy still exports (`MAIN/heardright-onnx-asr/src/lib.rs:36-39`). | Verified by diff counts; I did not read the 202 lines one by one |
| S11 | Streaming vs batch | Batch per utterance plus scheduled 15 s windows | RightKit has true streaming engines (Nemotron, unified) that HeardRight does not use; the scheduling logic (`ScheduledStatic15`) is app code and has no RightKit counterpart | Verified |

### 2.4 What MAIN calls today

Both. RightKit for three things, own code for the rest:

- `rightkit_asr::kws`: `MAIN/heardright-inference/src/kws_cascade.rs:26,28` (the only KWS backend; loaded at `asr/runtime.rs:734`), and the iOS C surface `MAIN/heardright_core/src/kws_ffi.rs:28`.
- `rightkit_asr::parakeet_core::{backend, beam, detok, policy, scoring, types}`: re-exported at `MAIN/heardright-parakeet-core/src/lib.rs:20`.
- `rightkit_asr::parakeet_coreml::sampled_model_fingerprint`: `MAIN/heardright-inference/src/coreml_asr/engine.rs:131`.
- **Still own code**: `heardright-parakeet-core/src/{bias,tdt_beam,command_alignment}.rs` (local copies that differ from RightKit's by 4, 62 and 2 lines), the whole `heardright-onnx-asr` crate (not `rightkit_asr::onnx_tdt`), `coreml_asr/`, `coreml/`, `whisper_coreml/`, `whisper_win*.rs`, `onnx_beam_decode.rs`, `asr/runtime.rs`, `calibration.rs`, `file_transcribe.rs`, `control_cleanup.rs`, `trigger_audio_cut.rs`.
- No Sherpa library reference remains in MAIN Rust code or manifests (searched); `ios-native/Vendor` no longer holds `sherpa-onnx.xcframework`.

### 2.5 Changes after the live build

Deliberate:

- Sherpa C library replaced by the Rust port (recorded decision).
- TDT model: v3 to Ultra (`MAIN asr/models.rs`: `parakeet_tdt_ultra_static1500_fp16int4_20261022`, `parakeet-tdt-ultra`).
- Windows duration cap 2 to 4, with the measurement cited in the RightKit changelog (#913: fewer decoder calls, WER 5.53% to 5.35%).
- KWS onset candidates and cut-start switches added (`kws_cascade.rs:12-18`); defaults "reproduce today's cut (the emission timestamp) exactly" per the comment (not verified).
- Env experiment switches (`HR_*`) removed in favour of launch choices (`engine_paths`).

Unclear: `ZEPHYR_STOP` confirm threshold 0.15 to 0.20 (S3). No comment explains it in the files I read.

Regressions: none identified by reading.

### 2.6 Verdict

**Right shape with listed gaps.** What MAIN consumes (KWS, decode core) carries the live constants and the live model files, and the one deliberate rewrite is in use on both desktop OSes. The gaps are that parity of the port with the patched Sherpa build is asserted but not checkable from this tree (S4), that three decode-core files and the ONNX execution crate are duplicated between the app and RightKit and have started to drift (S10), and that the RightKit CoreML and Whisper engines are missing live behaviours (S5, S8) so HeardRight cannot drop its own yet.

---

## 3. `rightkit-vad`

### 3.1 Live

`LIVE/heardright-inference/src/vad.rs` (522 lines): Silero v5 16 kHz op15 (`silero_vad_16k_op15.onnx`), 512-sample frames with 64-sample context (`:14-16`), ORT CPU, graph optimisation disabled, 1 intra and 1 inter thread (`:351-361`), one zero warm-up frame whose state is kept (`:327-336`), speech when `score >= threshold` (`:426`), threshold 0.20 (`:24`), async load with up to 30 s of pending audio replayed (`:18`). Optional noise calibration `calibrate()` (`:119-134`), reached only when `HR_VAD_AUTO_CALIBRATE` is set (`LIVE/heardright-runtime/src/worker/capture_device.rs:241-254`); the comment there says auto-calibration "BREAKS in practice". A CoreML backend exists for the parity harness only (`:78-86`). Mac and Windows.

### 3.2 RightKit

`RK/rightkit-vad/src/`: `Silero` (`load`, `load_with`, `from_bytes`, `SileroOptions` with threads, spinning, own thread pool, `Warmup::{ResetAfter, KeepState}`), `FrameScorer` trait, `FrameStream` (lossless frame carry, `set_threshold`, `aggregate`), plus span and chunking policy (`scores_to_spans`, `chunk_spans`, `RmsCalibration`) that HeardRight does not use. Graph optimisation disabled (`silero.rs:268`), threads from options (`:251-253`).

### 3.3 Differences

| # | Difference | Status |
|---|---|---|
| V1 | Session settings, frame protocol and `>=` threshold match live; `Warmup::KeepState` is documented "as HeardRight does" (`RK silero.rs:69`) and MAIN selects it (`MAIN vad.rs:368`). Library default is `ResetAfter` (`silero.rs:127`), so the option must stay set. | Verified |
| V2 | Env-gated calibration removed on MAIN; threshold is always 0.20 (`MAIN vad.rs:19,121`). | Verified. Off by default in live, so no shipped behaviour is lost. |
| V3 | On Windows MAIN gives VAD its own thread pool with spinning off (`MAIN vad.rs:352-373`); live used ORT defaults. | Verified. Post-live tuning in the app, expressed through RightKit options. |
| V4 | CoreML parity backend stays app-owned (`MAIN vad.rs:60-72`). | Verified |

### 3.4 What MAIN calls

RightKit, for production: `MAIN/heardright-inference/src/vad.rs:7` (`FrameScorer`, `FrameStream`), `:66` and `:270` (`rightkit_vad::Silero::load_with`), `:352-373` (`SileroOptions`). App code keeps loading, pending-audio replay, ORT binding and the harness-only CoreML scorer.

### 3.5 Changes after live

All deliberate: scorer moved to RightKit, calibration switch removed with the other env experiments, Windows thread-pool isolation.

### 3.6 Verdict

**Right shape for HeardRight.** Already adopted, same model, same frame protocol, same threshold and warm-up behaviour.

---

## 4. Top gaps, in priority order

1. **Delivery orchestration has no RightKit home** (A10): `backend.rs`, `targets.rs`, UIA worker protocol, WezTerm, paste probe, TSF tip. The inventory says EXTEND `rightkit-automation`; the crate's own docs say this stays in the product. That contradiction needs a decision before any move.
2. **`Automation::deliver_text` is the wrong tool for dictation** (A4 to A9): native write first, typed events, no paste by default, no receipt, activates the app, allowlist with Claude desktop permanently denied.
3. **MAIN does not use `rightkit_automation::desktop`** although it is a near-copy (A1). Two copies of 6.9k lines will drift. Switching needs the own-window prefix call (A3).
4. **Media pause/resume is absent from RightKit** (A11); the existing media-key toggle is not a substitute.
5. **Post-live delivery semantics changed** (1.5): success on receipt alone and Enter without read-back. Deliberate by the comments, but it is the behaviour the next release will ship in place of the live one.
6. **KWS port parity is not checkable here** (S4): the parity tests the changelog cites are not in the crate directory.
7. **ASR duplication and drift** (S10): `heardright-onnx-asr` and three `parakeet-core` files exist in both places.
8. **RightKit CoreML and Whisper engines lack live behaviours** (S5, S8), so the Mac engine and Windows Whisper cannot move yet.
9. **Unexplained data change**: `ZEPHYR_STOP` confirm threshold 0.15 to 0.20 (S3).

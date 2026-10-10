# RightKit crates audit, group 1 (read-only, static)

Scope: `crates/{agent-room-core, rightkit-agent-tools, rightkit-align, rightkit-asr, rightkit-audio, rightkit-automation, rightkit-browser, rightkit-cache, rightkit-channel-memory, rightkit-compose, rightkit-contracts, rightkit-control, rightkit-crash, rightkit-daemon}`.

Method and limits: Read/grep/`ls`/`wc -l` only. No cargo, no `cargo tree`, no compile. "Hits" are grep counts of `dep::`, `use dep`, `dep!` in `src/` (and `tests`/`examples`). Public-API counts are an upper bound: every `pub fn/struct/enum/trait/type` declared in `src/` including bins, impl methods and items inside private modules (`pub(crate)` counted separately as "restricted"). Claims about transitive tree size come from `Cargo.lock` edges and are marked unverified where a feature graph would be needed to confirm. Consumers: workspace `crates/*/Cargo.toml`, `desktop-app-template`, `spikes`, `packages/*` JSON, plus app-repo adoption from `docs/ADOPTION.md:28-41`.

## Verdict tally

| Crate | entries | needed | unused | feature-gate-it | replace-with | heavy |
|---|---|---|---|---|---|---|
| agent-room-core | 10 | 8 | 0 | 0 | 2 | 0 |
| rightkit-agent-tools | 9 | 9 | 0 | 0 | 0 | 0 |
| rightkit-align | 2 | 2 | 0 | 0 | 0 | 0 |
| rightkit-asr | 15 | 13 | 0 | 1 | 0 | 1 |
| rightkit-audio | 14 | 13 | 0 | 1 | 0 | 0 |
| rightkit-automation | 4 | 2 | 0 | 2 | 0 | 0 |
| rightkit-browser | 9 | 8 | 0 | 0 | 0 | 1 |
| rightkit-cache | 2 | 2 | 0 | 0 | 0 | 0 |
| rightkit-channel-memory | 3 | 2 | 0 | 0 | 0 | 1 |
| rightkit-compose | 3 | 3 | 0 | 0 | 0 | 0 |
| rightkit-contracts | 3 | 2 | 0 | 0 | 0 | 1 |
| rightkit-control | 7 | 5 | 0 | 2 | 0 | 0 |
| rightkit-crash | 6 | 5 | 0 | 1 | 0 | 0 |
| rightkit-daemon | 21 | 17 | 1 | 0 | 3 | 0 |

(rightkit-daemon replace-with = axum, url, subtle; `rightkit-contracts` is counted needed but its cost is contracts' `jsonschema`, fixed upstream.)

## Machete hints, verified

| Hint | Result | Evidence |
|---|---|---|
| rightkit-daemon: rustls-pki-types | TRUE POSITIVE, remove | Only use is `rustls::pki_types::...` (`src/http.rs:20-21`, `tests/tls.rs:23,44`); zero `rustls_pki_types::` anywhere. `rustls` with `std` already enables pki-types/std. `Cargo.toml:30`. |
| rightkit-control: webview2_com_038, windows_061 | FALSE POSITIVE (but over-enabled) | Used through rename-imports: `use ::{webview2_com_038 as webview2_com, windows_061 as windows};` at `src/embedded/win_view.rs:18`, plus `examples/offscreen_fixture_win.rs:28-35`. Static scanners miss the alias. Real issue: `default = ["wv2-0_38"]` (`Cargo.toml:16`) turns both on for every Windows build even without `tauri-plugin`, though the only consumer (`embedded`) is gated on `tauri-plugin` (`src/lib.rs:31`). |
| rightkit-audio: windows-core | FALSE POSITIVE | `#[implement(IMMNotificationClient)]` at `src/lifecycle.rs:198`; windows-implement 0.60.2 expands to `::windows_core::...` paths (`gen.rs:72-294`, `lib.rs:150`), which need a direct `windows-core` dependency. Keep. |

## agent-room-core

### Profile
- Purpose: state model and local HTTP/SSE host for multi-seat agent review rooms, SQLite-backed, plus the `room` CLI.
- Version 0.3.0; `publish = false`, `license = "UNLICENSED"` (`Cargo.toml:3-8`), yet listed in `packages/release/rightkit-versions.json` `stagedCargo` (line 275) and `inventory-check.mjs`. Inconsistent with "not published".
- LOC (src 4,988): `auth.rs` 207, `protocol.rs` 1308, `runtime.rs` 92, `server.rs` 1351, `store.rs` 389, `lib.rs` 8, `bin/room.rs` 1633.
- Public API: 61 fn, 25 struct, 8 enum, 0 trait.
- Features: `default = ["http"]`; `http = [dep:axum, dep:tokio, dep:tokio-stream]` gates `server` and the `room` bin (`required-features`).
- Dependents: none in `crates/*`, `desktop-app-template`, `spikes`; ADOPTION.md:28 "none recorded, 0 consumers". Referenced only by release/ownership metadata.

### Dependencies
| Dep | Spec | Optional | Used where (hits) | Verdict | Reason |
|---|---|---|---|---|---|
| axum | 0.8 default features | yes (http) | `server.rs` (8) | needed | Router, Json, Form, Query, SSE. Trim defaults (`macros`, `tracing`, `tower-log` not needed), as `rightkit-daemon` does. |
| rand | 0.9 | no | `auth.rs:2,205`, `bin/room.rs:6,718` (4) | replace-with: getrandom | Only `rng().fill_bytes` for 32-byte tokens, duplicated twice (`auth.rs:204`, `room.rs:716`). Workspace standard is `getrandom` 0.3 (mcp, oauth, models, secrets, tauri, scenario, http). Lock carries rand 0.8/0.9/0.10; this is the 0.9 consumer besides tokenizers. |
| rusqlite | 0.40.1 `bundled` | no | `store.rs` (3) | replace-with: rightkit-db | `store.rs:106` hand-sets WAL/foreign_keys pragmas; `rightkit-db` (`open_writer`, bundled default) is the suite's correct-by-default bootstrap. Same rusqlite, no tree change; consistency win. |
| serde | 1 derive | no | 6 files (7) | needed | |
| serde_json | 1 | no | 5 files (74) | needed | |
| sha2 | 0.10 | no | 5 files (5) | needed | Lock has sha2 0.10.9 and 0.11.0; cache/fs/models use 0.11. Align versions or route through `rightkit-fs` `digest`. |
| tokio | 1, features rt-multi-thread, macros, net, sync, time | yes (http) | `bin/room.rs`, `server.rs` (5) | needed | Already trimmed (no `full`). |
| tokio-stream | 0.1 `sync` | yes (http) | `server.rs` (3) | needed | `BroadcastStream`, `iter`. |
| tempfile (dev) | 3 | - | tests (23) | needed | |
| tower (dev) | 0.5 util | - | `p0_server.rs`, `p1_server.rs` (2) | needed | `ServiceExt::oneshot`. |

### Overlap and split notes
- `runtime.rs` (`runtime.json` discovery plus owner-only file checks) overlaps `rightkit-service` (service.json discovery, current-user-only) and `rightkit-fs` (owner-only files, atomic writes). Candidate to adopt those instead of bespoke code.
- `server.rs` is the third loopback axum/hyper host in this group (with `rightkit-daemon/http.rs` and `rightkit-mcp`/`rightkit-oauth` server).
- Split: yes, mild. `protocol` (1308, pure state machine, no I/O) + `store` + `auth` is the library; `bin/room.rs` (1633, process spawning, codex/claude CLI argument assembly at lines 1348-1399) is an orchestrator CLI that does not belong in a "core" crate. Move the bin to its own crate or app.

## rightkit-agent-tools

### Profile
- Purpose: one typed tool interface for agents over automation, in-app control (via rightkit-service), browser, speech, plus `right-ctl` CLI.
- Version 0.1.2; `publish = false`. Not in `stagedCargo`.
- LOC (src 1,925): `backends/` 839 (`desktop.rs` 286, `control.rs` 242, `web.rs` 190, `speech.rs` 112), `bin/right-ctl.rs` 275, `toolbox.rs` 372, `spec.rs` 188, `mcp.rs` 78, `exposure.rs` 73, `lib.rs` 68, `journal.rs` 32.
- Public API: 44 fn, 13 struct, 5 enum, 3 trait.
- Features: `default = ["automation","browser","speech"]`; `automation`, `browser` (+tokio), `speech`, `mcp` (opt-in, rightkit-mcp).
- Dependents: none (ADOPTION.md:29, 0 consumers).

### Dependencies
| Dep | Spec | Optional | Used where (hits) | Verdict | Reason |
|---|---|---|---|---|---|
| rightkit-service | 0.1.0 path | no | `lib.rs`, `backends/control.rs`, `backends/speech.rs`, `bin/right-ctl.rs` (5) | needed | Transport to in-app control. |
| rightkit-automation | 0.1.0 path | yes | `backends/desktop.rs`, `right-ctl.rs` (4) | needed | Gated. |
| rightkit-browser | 0.2.2 path | yes | `backends/web.rs`, `right-ctl.rs` (2) | needed | Gated but default-on, so the CLI always carries chromiumoxide/tokio. Consider default off for `browser`. |
| rightkit-speech-client | 0.3.0 path | yes | `backends/speech.rs` (1) | needed | Gated. |
| rightkit-mcp | 0.5.0 path | yes | `mcp.rs` (1) | needed | Opt-in. |
| serde | workspace | no | `spec.rs` (1, derive) | needed | |
| serde_json | workspace | no | 7 files (14) | needed | |
| tokio | 1 rt-multi-thread, time | yes (browser) | `backends/web.rs:16,30,163` (3) | needed | Owned runtime with `worker_threads(2)` for chromiumoxide handler tasks (`web.rs:30`); multi-thread is justified because tasks must run between `block_on` calls. |
| tempfile (dev) | 3 | - | `tests/e2e.rs:91` (1) | needed | Test requires `mcp` feature (`Cargo.toml` `[[test]]`). |

### Overlap and split notes
- Description and `lib.rs:8` say it wraps `rightkit-control`, but there is no dependency: `backends/control.rs:14-27` hard-codes a `CONTROL_METHODS` copy of `rightkit_control::embedded::METHODS`. Drift risk; either depend on rightkit-control with `tauri-plugin` off (not possible: `embedded` is gated) or move the method list into `rightkit-service`-level contract.
- No split. Thin adapter crate; no overlap beyond the above.

## rightkit-align

### Profile
- Purpose: script-to-ASR word alignment with lexical/timing confidence and ambiguity rejection.
- Version 0.0.0; `publish = false`; `license = "MIT"`, `repository = ".../adrdsouza/claude"` (`Cargo.toml:6-9`) differs from the rest (`bogusyogi/rightkit`, `MIT OR Apache-2.0`).
- LOC (src 1,041): `lib.rs` 375, `normalise.rs` 276, `solver.rs` 272, `timing.rs` 118.
- Public API: 9 fn, 7 struct, 2 enum, 5 restricted.
- Features: none.
- Dependents: none (not in stagedCargo; ADOPTION n/a).

### Dependencies
| Dep | Spec | Optional | Used where (hits) | Verdict | Reason |
|---|---|---|---|---|---|
| rightkit-transcript | path (no version) | no | `lib.rs:28,142`, `timing.rs:10` (4) + test | needed | `Word`, `Span`, `EndSource`. Path dep without `version` blocks publishing if that is ever wanted. |
| serde | 1.0.228 derive | no | `lib.rs`, `timing.rs` (derive on 11 types) | needed | Does not use `workspace = true`. |

### Overlap and split notes
- `lib.rs:12-17` documents why it is not inside `rightkit-transcript`. Acceptable. Overlaps in spirit with `rightkit-asr/src/longform.rs` (`timed_pieces_to_words`, `group_cues`) which also turns tokens into timed words.
- No split.

## rightkit-asr

### Profile
- Purpose: Parakeet ONNX + Whisper (CoreML / whisper.cpp) speech decoding runtime; vendored fork of `parakeet-rs` 0.3.5 (`UPSTREAM.toml`, `PATCHES.md`).
- Version 0.0.2; `publish = true` (`Cargo.toml:6`), listed in registry (`rightkit-versions.json:276,335`). ADOPTION.md:31 shows 0 app consumers.
- LOC (src 12,857): `sortformer.rs` 1255, `whisper/` 1500 (`cpp.rs` 385, `coreml/` 789, `mod.rs` 197, `windowing.rs` 129), `parakeet_core/` 991, `parakeet_coreml/` 786, `multitalker.rs` 771, `nemotron.rs` 583, `parakeet_unified.rs` 577, `longform.rs` 559, `model_transducer.rs` 461, `cohere.rs` 447, `transducer.rs` 399, `timestamps.rs` 359, `parakeet_eou.rs` 308, `model_cohere.rs` 313, `model_tdt.rs` 293, `audio.rs` 290, `model_nemotron.rs` 266, `model_multitalker.rs` 251, `execution.rs` 247, `onnx_beam.rs` 238, `decoder.rs` 211, `decoder_tdt.rs` 204, `model_eou.rs` 197, `model_unified.rs` 188, `conditioning.rs` 170, `parakeet.rs` 159, `vocab.rs` 151, `parakeet_tdt.rs` 146, `lib.rs` 118, `routing.rs` 102, `model.rs` 94, `transcriber.rs` 72, `error.rs` 59, `config.rs` 51, `chunked.rs` 41.
- Public API: 265 fn, 71 struct, 10 enum, 3 trait, 3 type, 12 restricted.
- Features: `default = ["cpu","ort-defaults"]`; `cpu = []` (no-op), `load-dynamic = []` (no-op, "kept so existing lists resolve"), `ort-defaults`, `ort-download`, `cuda`, `tensorrt`, `coreml`, `directml`, `migraphx`, `openvino`, `webgpu`, `nnapi`, `sortformer`, `multitalker` (=sortformer), `cohere`, `preload-dylibs`. EP features are pass-through to `rightkit-ort`.
- Dependents in workspace: `rightkit-transcript` optional (`asr-roundtrip`, `Cargo.toml:24,30`). No `desktop-app-template` use. Registry/release metadata only.

### Dependencies
| Dep | Spec | Optional | Used where (hits) | Verdict | Reason |
|---|---|---|---|---|---|
| half | 2 | no | `model_transducer.rs:276,296`, `whisper/coreml/bridge.rs:12` (3) | needed | f16 tensors. |
| hound | 3.5 | no | `audio.rs`, `error.rs` (5) | needed | Duplicate of `rightkit-audio/src/wav.rs:20` mono-f32 reader. |
| ndarray | 0.17 | no | 18 files (32) | needed | ORT tensor plumbing. |
| realfft | 3 | no | `audio.rs`, `sortformer.rs`, `whisper/coreml/mel.rs` (3) | needed | Three separate STFT/mel sites in this crate. |
| serde | 1.0 derive | no | `config.rs`, `transducer.rs` (2) | needed | |
| serde_json | 1.0 | no | 5 files (8) | needed | |
| libloading | 0.9 | no | `whisper/cpp.rs` only (6) | feature-gate-it | Sole user is the whisper.cpp FFI (385 lines, `pub(crate) mod cpp` unconditional, `whisper/mod.rs:16`). Put `WhisperCpp` behind a `whisper-cpp` feature; macOS-only consumers skip it. Lock carries libloading 0.7/0.8/0.9. |
| rightkit-ort | path 0.2.2 (+ndarray, half) | no | 13 files (28) + `ort::` 156 | needed | Single ORT pin owner. |
| rightkit-vad | 0.0.2 default-features=false | no | `chunked.rs:7` only (1) | needed | Weak: only the `Span` type (41-line module). Light, but note the coupling. |
| tokenizers | 0.22.2 default-off, `onig` | no | `decoder.rs`, `parakeet_eou.rs`, `cohere.rs`, `whisper/coreml/mod.rs` (7) | heavy | Used for `from_file`, `decode`, `token_to_id`, `get_vocab_size` only. Lock edges: onig (C regex), rayon, rayon-cond, esaxx-rs, regex, derive_builder, spm_precompiled, monostate, compact_str, ahash. In-crate `vocab.rs` (151) and `parakeet_core/detok.rs` (223) already hand-roll token tables. Replacing with a small decode-only path would remove the C build. High effort. |
| objc2 | 0.6 `exception` (macOS) | no | `parakeet_coreml/decode.rs`, `whisper/coreml/*` (8) | needed | |
| objc2-core-ml | 0.3 (macOS) | no | `whisper/coreml/*` (2) | needed | |
| objc2-foundation | 0.3 (macOS) | no | `whisper/coreml/bridge.rs` (1) | needed | |
| rightkit-media (dev) | path 0.2.3 | - | `tests/e2e_whisper.rs:27` | needed | |
| rightkit-speech-client (dev) | path 0.3.0 | - | `tests/certify_heardright.rs:18` | needed | |

### Overlap and split notes
- Subtitles/cues: `longform.rs:454-546` (`group_cues`, `render_subtitles`, `render_timing_json`, `timed_exports`) duplicates `rightkit-transcript` `segment.rs` / `export.rs` (cue segmentation, SRT/VTT/JSON).
- Silence cuts: `longform.rs:112` `quiet_cut_in` overlaps `rightkit-vad` policy.
- WAV load/resample: `audio.rs:8` `load_audio` vs `rightkit-audio/src/wav.rs`, `rightkit-qa/src/media.rs:176`, `rightkit-qts`, `rightkit-vad` (all use `hound`).
- Detokenization: `parakeet_core/detok.rs`, `vocab.rs`, and HF `tokenizers` coexist.
- Split: yes. Three independent responsibilities in one 12.9k-line crate: (1) Parakeet/ONNX engines (TDT, EOU, unified, nemotron, cohere), (2) macOS CoreML halves (`parakeet_coreml/` 786 + `whisper/coreml/` 789, objc2 deps), (3) whisper.cpp FFI + routing, (4) diarization (`sortformer` 1255 + `multitalker` 771 + `model_multitalker` 251, already feature-gated). Cleanest cut: `rightkit-asr-coreml` (macOS) and `rightkit-asr-diarize`. Also drop the two no-op features (`cpu`, `load-dynamic`) when convenient.

## rightkit-audio

### Profile
- Purpose: typed FFmpeg filter graphs, loudness gates, room tone, ducking, WAV I/O, and cpal live capture with resample/downmix/recovery.
- Version 0.2.0; publishable (no `publish=false`); registry 0.2.0. App consumers: cutright, heardright (ADOPTION.md:32,112-115). No in-workspace crate depends on it.
- LOC (src 3,626): `capture/` 1586 (`session.rs` 497, `metrics.rs` 258, `device.rs` 256, `resample.rs` 212, `downmix.rs` 169, `route.rs` 107, `mod.rs` 55, `staging.rs` 32), `lifecycle.rs` 468, `measure.rs` 426, `chain.rs` 300, `room_tone.rs` 270, `ducking.rs` 151, `wav.rs` 83, `preset.rs` 83, `filter.rs` 76, `profile.rs` 69, `bus.rs` 49, `lib.rs` 40, `error.rs` 25.
- Public API: 98 fn, 31 struct, 9 enum, 3 type, 4 restricted.
- Features: `default = ["capture"]`; `capture` = cpal, rubato, ringbuf, objc2-core-audio, objc2-core-foundation, windows, windows-core.
- Dependents: apps only (above).

### Dependencies
| Dep | Spec | Optional | Used where (hits) | Verdict | Reason |
|---|---|---|---|---|---|
| rightkit-media | path 0.2.3 | no | `error.rs`, `measure.rs` (5) | needed | FFmpeg runner. |
| serde | 1.0.228 derive | no | 5 files (5) | needed | |
| serde_json | 1.0.150 | no | `preset.rs`, `error.rs` (3) | needed | |
| thiserror | 2 | no | `error.rs` (1) | needed | |
| hound | 3 | no | `wav.rs` (8) | needed | |
| cpal | 0.18.2 | yes | `capture/*` (28) | needed | Gated by `capture`. |
| rubato | 5.0.0 | yes | `capture/resample.rs` (1) | needed | |
| ringbuf | 0.5 | yes | `capture/session.rs`, `downmix.rs` (7) | needed | |
| tracing | 0.1 | no | `capture/session.rs:193`, `lifecycle.rs:378-440` (5) | feature-gate-it | Both users are behind the `capture` feature, but the dep is unconditional; filter-graph-only consumers pay for it. Make it optional and add to `capture`. |
| objc2-core-audio | 0.3.2 (macOS) | yes | `capture/device.rs` (1) | needed | |
| objc2-core-foundation | 0.3.2 (macOS) | yes | `capture/device.rs` (1) | needed | |
| windows | 0.62.2 (Windows) | yes | `lifecycle.rs` (10) | needed | 6 features; `Win32_System_Com_StructuredStorage` has no visible direct use (`lifecycle.rs`), likely a transitive gate for COM/audio signatures; verify on a Windows host before trimming. |
| windows-core | 0.62.2 (Windows) | yes | none by name; `#[implement]` at `lifecycle.rs:198` | needed | Machete false positive (macro expands to `::windows_core`). |
| tempfile (dev) | 3 | - | `tests/audio_e2e.rs` (2) | needed | |

### Overlap and split notes
- WAV decode duplicates: `wav.rs` vs `rightkit-asr/src/audio.rs:8`, `rightkit-qa/src/media.rs:176`.
- Loudness: `measure.rs` (EBU R128 via ffmpeg) vs `rightkit-media/src/loudnorm.rs` (measurement + converging normalisation) vs `rightkit-qa/src/media.rs` loudness validators. Three implementations of one capability.
- Room tone selection (`room_tone.rs`) vs `rightkit-vad` segmentation.
- Split: yes. Two independent halves: offline FFmpeg graph rendering (chain, filter, bus, ducking, measure, preset, profile, room_tone, wav, ~1,550 LOC, depends on rightkit-media) and live capture + endpoint lifecycle (1,586 + 468 LOC, depends on cpal/rubato/ringbuf/objc2/windows). The `capture` feature already marks the seam; a `rightkit-capture` crate would keep cpal/windows out of graph-only consumers' manifests.

## rightkit-automation

### Profile
- Purpose: brand-neutral desktop automation (macOS AX via direct FFI, Windows UIA) with four-tier input ladder, safety, lease, evidence receipts.
- Version 0.1.0; `publish = false`; but in `stagedCargo` (`rightkit-versions.json:278`). No app consumers (ADOPTION.md:33).
- LOC (src 7,196): `engine.rs` 1138, `macos/` 1249 (`ax.rs` 387, `ffi.rs` 282, `appkit.rs` 203, `mod.rs` 183, `events.rs` 142, `capture.rs` 52), `win/` 1204 (`sys.rs` 681, `uia.rs` 365, `mod.rs` 158), `delivery.rs` 481, `png.rs` 402, `policy.rs` 400, `mock.rs` 380, `model.rs` 328, `keys.rs` 277, `proof.rs` 233, `lease.rs` 203, `in_app.rs` 173, `events.rs` 152, `focus.rs` 118, `platform.rs` 102, `redact.rs` 102, `admission.rs` 83, `lib.rs` 78, `json.rs` 71, `paths.rs` 22.
- Public API: 306 fn, 55 struct, 20 enum, 7 trait, 8 type, 6 restricted.
- Features: none. Test `e2e_mac` has `harness = false`.
- Dependents: `rightkit-agent-tools` (optional). Nothing else.

### Dependencies
| Dep | Spec | Optional | Used where (hits) | Verdict | Reason |
|---|---|---|---|---|---|
| rightkit-service | 0.1.0 path | no | `paths.rs:7`, `in_app.rs:11,121` (4) | feature-gate-it | Used only for tier-2 in-app client (`in_app.rs`) and the app-data root (`paths.rs`, 22 lines). Brings serde, serde_json, libc (unix), windows 0.62 (windows), rightkit-framed-sidecar into a crate whose own manifest comment (`Cargo.toml:10-12`) claims "zero third-party dependencies on macOS" and a hand-rolled `json.rs`. The comment is stale. Gate behind an `in-app` feature. |
| serde_json | 1.0.150 | no | `in_app.rs:12` only (1) | feature-gate-it | Same gate; the crate otherwise writes JSON by hand (`json.rs`). |
| windows | 0.62, 15 features (Windows) | no | `win/sys.rs`, `win/uia.rs` (21) | needed | Wide feature list is all in use per `sys.rs:9-20`, `uia.rs:9-13`; `Win32_System_Variant` has no visible direct use, verify on Windows. Also a version split: 0.62 here vs 0.61 via control's wv2-0_38. |
| tempfile (dev) | 3 | - | tests/examples (2) | needed | |

### Overlap and split notes
- Heavy duplication with `rightkit-control` (neither depends on the other): `lease.rs` (203) vs control `lease.rs` (861) + `lease_store.rs` (434) (both "epoch fencing + acked seq", PTY-002); `admission.rs` (83) vs control `admission.rs` (532) (both EFF-001); `events.rs` (152) vs control `events.rs` (70); `json.rs` `esc` (`automation/json.rs:3-12` identical to `control/json.rs:3-12`); `keys.rs` (277) vs control `keys.rs` (219) vs `rightkit-browser/src/keys.rs` (118); macOS FFI blocks `macos/ffi.rs:41-112` vs `control/mac.rs:31-69` (same CoreFoundation/ApplicationServices externs, `CGEventPostToPid`, `AXUIElement*`). Also `rightkit-scenario` (leases with fencing tokens) and `rightkit-sandbox` (PTY lease). A shared `rightkit-effects` (lease + admission + events + json esc) would remove roughly 1,100 lines of duplicate.
- `png.rs` (402 lines, only `crc32` implementation in the workspace) is a hand-rolled encoder; `rightkit-qa/src/media.rs` has PNG validators.
- `in_app.rs` is the client half of `rightkit-control::embedded` (control `embedded/mod.rs` is the server); they share a protocol with no shared type crate.
- `mock.rs` (380) is a public module in the shipping lib.
- Split: yes. `macos/` and `win/` are platform backends behind `platform.rs` traits; the policy/proof/redact/lease/admission layer is platform-neutral. Candidate: neutral core + `-macos` / `-windows` backend crates.

## rightkit-browser

### Profile
- Purpose: Chrome DevTools Protocol browser runtime: multi-page sessions, named profiles, real CDP input, stale-ref observations, SSRF/egress policy with pinning proxy.
- Version 0.2.3 (registry 0.2.2, ADOPTION.md:34 "1 stale" pin in coderight); publishable. Consumers: coderight, genright; in-workspace `rightkit-agent-tools` (optional), `rightkit-qa` (optional `browser` feature).
- LOC (src 4,270): `policy.rs` 1158, `page.rs` 1100, `config.rs` 759, `session.rs` 677, `proxy.rs` 190, `proc.rs` 180, `keys.rs` 118, `error.rs` 49, `lib.rs` 39.
- Public API: 88 fn, 14 struct, 9 enum, 6 type, 17 restricted (modules are private; re-exports from `lib.rs`).
- Features: none.

### Dependencies
| Dep | Spec | Optional | Used where (hits) | Verdict | Reason |
|---|---|---|---|---|---|
| chromiumoxide | 0.9 default features | no | `page.rs`, `session.rs` (12) | heavy | Lock (`Cargo.lock:~727`) shows 0.9.1 with edges to async-tungstenite, chromiumoxide_cdp (large generated CDP types), reqwest 0.13.5, which, windows-registry. The reqwest edge looks like an unneeded fetcher-style feature; unverified without a feature graph. Try `default-features = false, features = ["tokio-runtime"]` and confirm with `cargo tree -e features`. |
| futures | 0.3 | no | `page.rs`, `policy.rs`, `session.rs` (3) | needed | `StreamExt`, `BoxFuture`. |
| serde | 1 derive | no | `page.rs` (1) | needed | |
| serde_json | 1 | no | `page.rs`, `policy.rs` (4) | needed | |
| tempfile | 3 | no | `session.rs:32,59` (production ephemeral profile `TempDir`), tests elsewhere | needed | Legit runtime use. |
| thiserror | 2 | no | `error.rs` (17 variants) | needed | |
| tokio | 1, features rt-multi-thread, macros, sync, time, fs, net, io-util | no | `page.rs`, `policy.rs`, `session.rs`, `proxy.rs` (30) | needed | Trim: `fs` has no use (`tokio::fs` never appears), `macros` and `rt-multi-thread` are exercised only by `#[tokio::test(flavor="multi_thread")]` in `tests/` and a `current_thread` builder at `policy.rs:781`. Move `macros`/`rt-multi-thread` to dev-deps; drop `fs`. chromiumoxide may re-enable some. |
| url | 2 | no | `policy.rs:459`, `proxy.rs:106` (2) | needed | |
| rightkit-process | path 0.3.0 | no | `proc.rs:7` (1) | needed | `OwnedChild`. |

### Overlap and split notes
- `policy.rs` (1158) + `proxy.rs` (190) = 1,348 lines of network egress policy (RFC1918/link-local/NAT64 address classification, DNS pinning proxy) that is independent of CDP. Overlaps in subject with `rightkit-scenario/src/net.rs:31` (`is_loopback`) and any SSRF checks in `rightkit-mcp`/`rightkit-models`. Split candidate: `rightkit-netpolicy`, reusable by `rightkit-http`, `rightkit-mcp`, `rightkit-models`.
- EFF-001 admission hook in `policy.rs` duplicates the admission pattern in `rightkit-automation/admission.rs` and `rightkit-control/admission.rs`.
- `keys.rs` is a third key table (CDP form).

## rightkit-cache

### Profile
- Purpose: size-capped disk cache with persistent LRU, atomic writes, group invalidation, plus stable-prefix prompt-cache keys.
- Version 0.1.0; publishable; registry 0.1.0; consumers coderight, mailright (ADOPTION.md:35).
- LOC (src 590): `disk.rs` 359, `prefix.rs` 150, `key.rs` 67, `lib.rs` 14.
- Public API: 30 fn, 7 struct, 2 enum.
- Features: none. Dependents in workspace: none.

### Dependencies
| Dep | Spec | Optional | Used where (hits) | Verdict | Reason |
|---|---|---|---|---|---|
| sha2 | 0.11 | no | `key.rs:5,16` (1) | needed | Single use. Equivalent exists in `rightkit-fs` `digest` feature (also sha2 0.11): `sha256_bytes`, `hex`. Low value to change; note the version split vs agent-room-core (0.10). |
| tempfile (dev) | 3 | - | tests (2) | needed | |

### Overlap and split notes
- `disk.rs:241-252` hand-rolls temp-file + rename; `rightkit-fs::atomic_write` does the same.
- Split: yes, trivially. `prefix.rs` (prompt stable-key/break tracking, "from CodeRight", `CONDUCTOR_CACHE_KEY_PREFIX` at `prefix.rs:66`) has nothing to do with disk caching. Fold it into an LLM/prompt crate or keep, but do not let it define the crate's identity.

## rightkit-channel-memory

### Profile
- Purpose: `channel-memory.v1` schema types, validation, version gate, proof-citation and scope checks for PostRight P3 / GenRight G7.
- Version 0.1.0; `publish = false`. Not in stagedCargo. No consumers.
- LOC (src 806): `model.rs` 444, `validate.rs` 214, `error.rs` 65, `schema.rs` 43, `lib.rs` 40.
- Public API: 4 fn, 29 struct, 14 enum, 1 restricted.
- Features: none.

### Dependencies
| Dep | Spec | Optional | Used where (hits) | Verdict | Reason |
|---|---|---|---|---|---|
| serde | 1.0.228 derive | no | `model.rs` (4) | needed | |
| serde_json | 1.0.150 | no | `schema.rs`, `validate.rs` (5) | needed | |
| jsonschema | 0.58.6 default-off | no | `schema.rs` (2) | heavy | Lock edges: fancy-regex, regex, referencing, uuid-simd, email_address, fraction, bytecount, strum, data-encoding. Fine for six contracts, over the top for one. Version pinned to match `rightkit-contracts` on purpose (`Cargo.toml:17`). Disappears as a separate cost if merged into contracts. |

### Overlap and split notes
- MERGE candidate: `schema.rs` (43 lines) is a near line-for-line copy of `rightkit-contracts/src/schema.rs` (OnceLock validator, Draft 2020-12, identical error formatting). `validate.rs:13` "contract name and version" gate re-implements `rightkit-contracts` `negotiate` (`version.rs:58`). Make `channel-memory.v1` a seventh `Contract` (or a `channel-memory` module) in `rightkit-contracts`; drops a crate and one validator copy.
- `schema.rs:7-8` `include_str!("../../../contracts-pending/...")` reaches outside the crate directory; not packageable as-is (fine while `publish = false`).

## rightkit-compose

### Profile
- Purpose: evaluated composition model (layers, keyframed tracks, motion blur, shadows); pure `evaluate(composition, t_ms)` shared by preview and export.
- Version 0.0.0; `publish = false`; `license = "MIT"`, `repository = ".../adrdsouza/claude"` (metadata differs from rest). No consumers (ADOPTION.md:37).
- LOC (src 1,550): `eval.rs` 542, `validate.rs` 474, `model.rs` 299, `track.rs` 187, `error.rs` 17, `lib.rs` 31.
- Public API: 6 fn, 20 struct, 8 enum, 1 trait, 1 type.
- Features: none.

### Dependencies
| Dep | Spec | Optional | Used where (hits) | Verdict | Reason |
|---|---|---|---|---|---|
| serde | 1.0.228 derive | no | `eval.rs`, `model.rs`, `track.rs` (3) | needed | |
| thiserror | 2 | no | `error.rs` (6 variants) | needed | Replaceable by a 25-line manual `Display`/`Error` impl, but thiserror 2 is already in the workspace lock for ~15 crates, so no tree saving. |
| serde_json (dev) | 1.0.150 | - | `tests/composition.rs:820-847` (7) | needed | |

### Overlap and split notes
- No code overlap found. Smallest dependency footprint of the group. Keep as is.

## rightkit-contracts

### Profile
- Purpose: versioned content-pipeline contracts (content brief, delivery envelope, record session/event, demo plan, daemon API): schema validation, serde types, version negotiation; plus `rightkit-contracts` CLI.
- Version 0.1.2; `publish = false`; in stagedCargo (`rightkit-versions.json:281`). In-workspace dependents: `rightkit-daemon`, `rightkit-record`. No app consumers.
- LOC (src 2,339): `types/` 1312 (`record_session.rs` 307, `record_event.rs` 244, `content_brief.rs` 211, `daemon_api.rs` 191, `delivery_envelope.rs` 168, `demo_plan.rs` 159, `mod.rs` 32), `semantic.rs` 359, `lib.rs` 183, `version.rs` 137, `fixtures.rs` 116, `error.rs` 91, `main.rs` 92, `schema.rs` 49.
- Public API: 18 fn, 80 struct, 41 enum, 1 type, 2 restricted.
- Features: none.

### Dependencies
| Dep | Spec | Optional | Used where (hits) | Verdict | Reason |
|---|---|---|---|---|---|
| serde | 1.0.228 derive | no | 8 files (8) | needed | |
| serde_json | 1.0.150 | no | 6 files (9) | needed | |
| jsonschema | 0.58.6 default-off | no | `schema.rs` (2) | heavy | Large tree (see channel-memory). `rightkit-daemon` uses only the typed structs (`config.rs:4`, `routes.rs:4`) and never calls `validate`, yet pays for jsonschema. Add a `schema` feature (default on) gating `schema.rs`, `semantic.rs`, `fixtures.rs`, `main.rs`; daemon uses `default-features = false`. `rightkit-record` does call `validate` (`recorder.rs:21`, `verify.rs:11`) and keeps the default. |

### Overlap and split notes
- `pub mod fixtures` (`lib.rs:24`, 116 lines) is CLI/test support in the public lib; gate with the CLI/`schema` feature.
- Merge target for `rightkit-channel-memory` (see above).
- Types (1,312) vs validation machinery (~740) are separable; the feature gate above gets the benefit without a crate split.

## rightkit-control

### Profile
- Purpose: background native input, AX reads, WebDriver bridge, in-app Tauri control server, offscreen launch, for driving real desktop apps without stealing focus.
- Version 0.1.11; publishable; registry 0.1.11. App consumers cockpit, cutright, genright (ADOPTION.md:39, "1 stale"). In-workspace: `rightkit-qa`, `rightkit-shell/example-app`, `spikes/rk2-macos/fixture-app` (all with `tauri-plugin`).
- LOC (src 8,052): `embedded/` 2325 (`mod.rs` 904, `mac_view.rs` 884, `win_view.rs` 537), `offscreen.rs` 1105 (+ `offscreen_mac.rs` 301, `offscreen_win.rs` 255), `lease.rs` 861, `mac.rs` 840, `admission.rs` 532, `cli.rs` 509, `webdriver.rs` 449, `lease_store.rs` 434, `keys.rs` 219, `json.rs` 81, `events.rs` 70, `lib.rs` 41, `bin/` 30.
- Public API: 173 fn, 34 struct, 19 enum, 4 trait, 1 type, 22 restricted.
- Features: `default = ["wv2-0_38"]`; `tauri-plugin` (tauri, rightkit-service, serde_json); `offscreen-private-spi` (macOS private SPI, off); `wv2-0_38` (windows 0.61 + webview2-com 0.38); `wv2-0_39` (0.62 + 0.39). Exactly one wv2 generation should be on (`win_view.rs:13-20`).
- Stale artefacts: `registration.patch` in crate root; `examples/control-test-app/target/` (a build dir inside the crate tree, noisy for any recursive tool).

### Dependencies
| Dep | Spec | Optional | Used where (hits) | Verdict | Reason |
|---|---|---|---|---|---|
| tauri | 2 | yes | `embedded/*`, `offscreen.rs:546-582` (11) | needed | Gated by `tauri-plugin`. |
| rightkit-service | 0.1.0 path | yes | `embedded/mod.rs:45` (code), `events.rs:3` (doc only) | needed | Gated. |
| serde_json | 1.0.150 | yes | `embedded/mod.rs`, `win_view.rs` (14) | needed | Gated. Elsewhere hand-written `json.rs`. |
| windows (as `windows_061`) | 0.61 (Windows) | yes | `embedded/win_view.rs:18`, example (alias import) | feature-gate-it | Machete false positive. But default-on `wv2-0_38` enables it without `tauri-plugin`, where nothing consumes it (`embedded` is gated). Make the wv2 features implied by `tauri-plugin` (e.g. `tauri-plugin-wv2-0_38`) or drop `wv2-0_38` from default. Also `win_view.rs:6` header says "webview2-com 0.39 / windows 0.61", inconsistent. |
| webview2-com (as `webview2_com_038`) | 0.38 (Windows) | yes | same | feature-gate-it | Same. |
| windows (as `windows_062`) | 0.62 (Windows) | yes | `win_view.rs:20` | needed | Opt-in. |
| webview2-com (as `webview2_com_039`) | 0.39 (Windows) | yes | `win_view.rs:20` | needed | Opt-in. Lock holds both generations (0.38.2 and 0.39.1). |

### Overlap and split notes
- Control-plane duplication with `rightkit-automation` listed there (lease, admission, events, json, keys, macOS FFI). `CONTROL_METHODS` copied into `rightkit-agent-tools/backends/control.rs:14`.
- Split: yes. Two independent halves in one crate: (a) out-of-process driving: `mac.rs`, `cli.rs`, `webdriver.rs`, `offscreen*.rs`, control plane (about 4.6k lines, no third-party deps) and (b) in-app Tauri plugin `embedded/` (2.3k lines + tauri + service + WebView2 generations). The `tauri-plugin` feature is the seam; `rightkit-control-embedded` would stop the wv2 feature matrix leaking into CLI-only consumers. Also the `rightkit-control` bin is macOS-only and prints "macOS only" on other targets (`bin/rightkit-control.rs:5-9`); gate with `required-features` or cfg-target `[[bin]]`.

## rightkit-crash

### Profile
- Purpose: privacy-first panic capture, redacted crash reports, native dumps, support bundles.
- Version 0.2.0; publishable; registry 0.2.0; consumer heardright (ADOPTION.md:40). No in-workspace dependents.
- LOC (src 935): `native.rs` 206, `hook.rs` 125, `bin/rightkit-dump-helper.rs` 125, `bundle.rs` 140, `report.rs` 97, `retention.rs` 78, `pending.rs` 68, `archive.rs` 67, `lib.rs` 29.
- Public API: 16 fn, 6 struct, 1 enum, 3 restricted.
- Features: none (dependencies use `default-features = false` for rightkit-logs).

### Dependencies
| Dep | Spec | Optional | Used where (hits) | Verdict | Reason |
|---|---|---|---|---|---|
| rightkit-logs | 0.2.0 path, default-features=false | no | `native.rs`, `bundle.rs`, `report.rs`, `pending.rs`, `lib.rs` (5) | needed | Drops ureq. Redaction (`scrub_*`) lives in logs; no duplicate here. |
| serde | 1 derive | no | `report.rs`, `bundle.rs` (2) | needed | |
| serde_json | 1 | no | 4 files (8) | needed | |
| zip | 2, default off, `deflate` | no | `archive.rs` only (5) | feature-gate-it | Only the user-initiated support bundle (`bundle.rs`/`archive.rs`, 207 lines) needs it; panic-hook-only consumers pay for zip + flate2 + crc32fast. Put behind a `support-bundle` feature (default on). `rightkit-models` also uses zip 2, so tree is shared if both are present. |
| windows-sys | 0.61.2, 6 features (Windows) | no | `bin/rightkit-dump-helper.rs` only (3) | needed | Bin-only dependency; lib consumers on Windows still get it in the manifest. Fine, light. |
| rightkit-logs (dev) | `testing` | - | tests (2) | needed | |

### Overlap and split notes
- `retention.rs` (newest-N plus size cap prune) overlaps pruning logic elsewhere (rightkit-logs retention, agent-room-core prune, rightkit-models). Not a clean duplicate; note only.
- Dump helper bin + `native.rs` (capture of Windows minidumps/macOS samples) is a separable platform responsibility; at 331 lines it does not warrant its own crate. No split.

## rightkit-daemon

### Profile
- Purpose: daemon host capability: HTTPS listener (rustls), device enrolment and signed requests, OAuth callback and signed webhook route classes, blake3-keyed resumable uploads, CAS mutations, durable state, lifecycle events.
- Version 0.1.0; `publish = false`; not in stagedCargo; no dependents (ADOPTION.md:41).
- LOC (src 1,820): `daemon.rs` 1035, `http.rs` 171, `routes.rs` 164, `config.rs` 83, `events.rs` 83, `util.rs` 83, `state.rs` 87, `oauth.rs` 47, `signing.rs` 42, `lib.rs` 25.
- Public API: 33 fn, 17 struct, 2 enum, 2 trait, 1 type.
- Features: `default = ["oauth"]`; `oauth = [dep:rightkit-oauth]`.

### Dependencies
| Dep | Spec | Optional | Used where (hits) | Verdict | Reason |
|---|---|---|---|---|---|
| rightkit-contracts | path 0.1.0 | no | `config.rs:4`, `routes.rs:4`, `daemon.rs` (5) | needed | Types only; drags jsonschema (see contracts, fix there). |
| rightkit-oauth | path 0.3.0, `server` | yes (oauth) | `oauth.rs` (1) | needed | Also in dev-deps so tests build with the feature off. |
| serde | 1.0.228 derive | no | `state.rs` (1) | needed | |
| serde_json | 1.0.150 | no | 4 files (14) | needed | |
| axum | 0.8 default off, `http1`,`tokio` | no | `http.rs` only (6) | replace-with: hyper `service_fn` | Used as `Router::new().fallback(entry).with_state(daemon)` (`http.rs:28-31`): a single catch-all handler, no routing/extractors beyond `State` and `Request`. hyper is already a direct dep. `http` + `http-body-util` would replace it. Saves the axum+tower tree only when `oauth` is off (rightkit-oauth `server` already brings axum). Medium effort. |
| tokio | 1, rt-multi-thread, macros, net, sync, time, io-util | no | `http.rs` (9) | needed | `select!` needs `macros` (`http.rs:140,165`). `io-util` is likely redundant (tokio-rustls enables it). |
| tokio-rustls | 0.26 default off `ring`,`tls12` | no | `http.rs:25` (1) | needed | |
| rustls | 0.23 default off `ring`,`std`,`tls12` | no | `http.rs` (6) | needed | |
| hyper | 1 `http1`,`server` | no | `http.rs:16` (1) | needed | |
| hyper-util | 0.1 | no | `http.rs` (3) | needed | `TokioIo`, `TokioTimer`, graceful. |
| ring | 0.17 | no | `util.rs`, `signing.rs`, `daemon.rs`, `http.rs` (6) | needed | HMAC, Ed25519 verify, SHA-256, RNG; same backend as the rustls provider, good. |
| blake3 | 1 | no | `daemon.rs:653,774` (2) | needed | Keyed upload digest. |
| base64 | 0.22 | no | `daemon.rs:11-12` (2) | needed | |
| rustls-pki-types | 1 `std` | no | none (only `rustls::pki_types`) | unused | Remove (`Cargo.toml:30`). |
| url | 2.5 | no | `daemon.rs:936` (1) | replace-with: form_urlencoded | Sole use is `url::form_urlencoded::parse`. `url` pulls idna (+ unicode tables) for a query-string split; the tiny `form_urlencoded` crate (already in the tree as url's dep) does exactly this. |
| subtle | 2.6 | no | `util.rs:64` (1) | replace-with: ring::constant_time | `subtle::ConstantTimeEq::ct_eq` on byte slices; `ring::constant_time::verify_slices_are_equal` is already linked. |
| rightkit-http (dev) | path 0.2.0 `testing` | - | tests (2) | needed | |
| rightkit-oauth (dev) | path 0.3.0 `server` | - | tests (2) | needed | |
| tempfile (dev) | 3 | - | tests (3) | needed | |
| tower (dev) | 0.5 util | - | tests (1) | needed | |
| rcgen (dev) | 0.13 | - | tests (1) | needed | Test certs. |

### Overlap and split notes
- Hashing/hex helpers in `util.rs:27,60` duplicate `rightkit-fs/src/digest.rs` (`hex`, `sha256_bytes`); RNG is a third path (ring `SystemRandom`) beside `getrandom` and `rand`.
- Three loopback HTTP hosts in the workspace with separate hand-written plumbing: this, `agent-room-core/server.rs`, and `rightkit-oauth` server / `rightkit-mcp` server.
- Split: yes. `daemon.rs` is 1,035 lines containing enrolment/signing, webhook drain, OAuth callback dispatch, resumable uploads (`object_path`, blake3, upload sessions at `daemon.rs:653-774`) and durable job/CAS state. Resumable uploads are an independent responsibility; `http.rs` (TLS listener) is already separate. Candidate: `rightkit-uploads` extracted, leaving daemon as the host.

## Cross-cutting findings

1. Three-way copy of the control plane (EFF-001 admission, PTY-002 lease, events, JSON escape, key tables, macOS AX/CG FFI): `rightkit-automation`, `rightkit-control`, and partially `rightkit-browser/policy.rs`, `rightkit-scenario`. Extract one `rightkit-effects` crate.
2. `rightkit-contracts` + `rightkit-channel-memory`: same validator, version gate, and dependency; merge.
3. `jsonschema` cost is paid by crates that only need typed structs (`rightkit-daemon`); feature-gate in contracts.
4. Version splits visible in `Cargo.lock`: sha2 0.10/0.11 (agent-room-core vs cache), rand 0.8/0.9/0.10 (agent-room-core, qts), windows 0.61/0.62 and webview2-com 0.38/0.39 (control, automation, audio, service), getrandom 0.2/0.3/0.4, libloading 0.7/0.8/0.9.
5. WAV/loudness/mel/cue-export logic is implemented 2-4 times across `rightkit-asr`, `rightkit-audio`, `rightkit-media`, `rightkit-qa`, `rightkit-transcript`, `rightkit-vad`.
6. Metadata drift: `rightkit-align` and `rightkit-compose` carry `repository = .../adrdsouza/claude` and `license = "MIT"`; `agent-room-core` is `publish = false`/`UNLICENSED` yet in `stagedCargo`; `rightkit-automation` is `publish = false` yet in `stagedCargo`; `rightkit-contracts` too.
7. Nothing in this group is consumed by `desktop-app-template` (it pins only logs, process, tauri, db, jobs, framed-sidecar).

# Apps group 3: HeardRight, VoiceRight, ScrapeRight — RightKit adoption inventory

Date 2026-10-10. Read-only. Sources: `rightkit/packages/release/rightkit-versions.json` (`registry` section = latest published), `docs/CAPABILITY-MAP.md`, `docs/audits/2026-10-08-inventory-final.md:148-171`, per-app greps and `right-release ownership`.

Caveats that apply to every table below:
- LOC are `wc -l` of `.rs`/`.ts`/`.tsx`/`.swift` files excluding `target`, `node_modules`, `.build`. "est" marks a judgment of lines touched (call sites or the app-policy-free part), not a file total. Tests are included in per-crate totals and excluded from "duplicate LOC" unless stated.
- Similarity claims come from a line-diff (`difflib`) of same-named files against `rightkit/crates/*`. Anything marked "verify" needs an API-parity read before the handover chat commits to it.
- `rightkit/INDEX.md` is stale against `rightkit-versions.json` (INDEX says asr, vad, ocr, tts are "staged, never published"; the registry section records asr 0.0.2, vad 0.0.2, ocr 0.1.2, tts 0.1.0). This report follows the registry, as instructed.
- Not yet published (no registry record), so apps cannot consume them: `rightkit-hotkeys`, `rightkit-overlay`, `rightkit-llm`, `rightkit-sync`. `rightkit-automation` is `publish = false` (`crates/rightkit-automation/Cargo.toml`).
- `RightKitSwift`: registry records 0.1.1 (staged 0.3.1), but both apps pin higher (HeardRight `ios-native/project.yml:19` 0.3.2; ScrapeRight `ios-native/project.yml:22` and `macos-native/ScrapeRightAppleCredentials/Package.swift:11` 0.3.7). The versions file is behind the Swift repo; RightKit-side record fix, not an app fix.

Latest published (registry) for crates/packages referenced below: rightkit-asr 0.0.2, audio 0.2.0, browser 0.2.2, crash 0.2.0, db 0.3.2, fs 0.3.3, framed-sidecar 0.1.0, http 0.2.4, license 0.1.3, logs 0.2.0, models 0.2.7, ocr 0.1.2, ort 0.2.2, pdf 0.2.0, playback 0.1.1, process 0.3.3, qts 0.2.4, secrets 0.1.2, service 0.1.3, settings 0.1.0, shell 0.1.4, speech-client 0.3.2, tauri 0.1.2, tts 0.1.0, vad 0.0.2; npm app-shell 0.3.0, legal 0.3.1, legal-ui 0.2.0, license 0.1.8, logs 0.1.4, media-ui 0.3.0, onboarding 0.3.0, platform-ui 0.2.0, qa-chrome 0.1.3, release 0.2.147, settings-ui 0.3.0, tauri 0.2.0, theme 0.2.1, updates 0.2.5.

---

## HeardRight

### 1. Shape
Live app: `/Volumes/D/claude/heardright/tauri-app-next/` (Cargo workspace at `tauri-app-next/Cargo.toml`, 15 members, one lockfile; pnpm root `heardright/package.json` plus `tauri-app-next/package.json`). Last commit 2026-10-10 (`c87afb5e`), clean tree. Targets: macOS (arm64 + x86_64 universal), Windows, iOS (`ios-native/`, 58,225 Swift LOC, xcodegen `project.yml`). `heardright_capture/` is an empty directory (only `.claude`): replaced by `rightkit-audio` in commit `d7054381`.

| Crate / package | Rust LOC | Notes |
|---|---:|---|
| src-tauri (`heardright_next`) | 54,993 | Tauri shell: tray, pill overlay, hotkeys, history (SQLCipher), license, cloud sync, settings, updates |
| heardright-runtime | 21,189 | sidecar worker, IPC, `speech_service/` host (1,411) |
| heardright-inference | 20,846 | ASR runtime, Parakeet CoreML/ORT, Whisper, Silero VAD (`vad.rs` 611), KWS (1,330) |
| heardright-engine | 16,000 | engine sidecar binary and 40+ examples |
| heardright_core | 14,853 | text pipeline, settings blob, PTT policy, protocol |
| heardright-text-pipeline | 8,942 | polish + L3 LLM cleanup (`l3_cleanup/` 5,048) |
| heardright_platform | 8,267 | AX/UIA focus, clipboard, window query |
| heardright-command-host | 7,606 | voice commands, focus, delivery |
| heardright-telemetry | 5,150 | outbox, upload, writer, crash_dump |
| heardright-app-runtime | 2,756 | engine supervisor |
| heardright-model-store | 2,853 | adapter over rightkit-models plus app policy |
| heardright-parakeet-core | 1,863 | tdt_beam/bias/detok/policy (beam/backend/scoring already re-exported from rightkit-asr) |
| heardright-onnx-asr | 1,487 | ORT env + session |
| tsf-tip | 988 | Windows TSF, dormant |
| heardright-dump-helper | 125 | Windows minidump helper |
| macos-native/HeardRightPillMac | 7,575 Swift | pill, credentials helper |
| src/ (React) | 21,623 TS/TSX | pages 8,163, components 5,365, lib 3,616, pill 2,724 |

### 2. RightKit adoption today (pins)

| Pin | path:line | Pinned | Latest | State |
|---|---|---|---|---|
| rightkit-models | tauri-app-next/heardright-model-store/Cargo.toml:11 | =0.2.6 | 0.2.7 | stale (patch) |
| rightkit-crash | heardright-telemetry/Cargo.toml:12 | =0.2.0 | 0.2.0 | ok |
| rightkit-logs | heardright-telemetry/Cargo.toml:13; src-tauri/Cargo.toml:39 | =0.2.0 | 0.2.0 | ok |
| rightkit-asr | heardright-parakeet-core/Cargo.toml:12 | =0.0.2 | 0.0.2 | ok |
| rightkit-speech-client | heardright-engine/Cargo.toml:43; heardright_core/Cargo.toml:23; heardright-runtime/Cargo.toml:13 | =0.3.0 | 0.3.2 | stale x3 (patch) |
| rightkit-audio | heardright-engine/Cargo.toml:45; heardright-runtime/Cargo.toml:16; src-tauri/Cargo.toml:74 | =0.2.0 | 0.2.0 | ok |
| rightkit-service | heardright-runtime/Cargo.toml:14 | =0.1.0 | 0.1.3 | stale (patch) |
| rightkit-license | src-tauri/Cargo.toml:37 | =0.1.3 | 0.1.3 | ok |
| rightkit-tauri | src-tauri/Cargo.toml:41 | =0.1.1 | 0.1.2 | stale (patch) |
| rightkit-process | heardright-app-runtime/Cargo.toml:15 | =0.3.2 | 0.3.3 | stale (patch) |
| @rightkit/app-shell | tauri-app-next/package.json:157 | 0.1.0 | 0.3.0 | stale, 0.x minor jump x2 (breaking risk) |
| @rightkit/legal | package.json:158 | 0.3.1 | 0.3.1 | ok |
| @rightkit/legal-ui | package.json:159 | 0.1.3 | 0.2.0 | stale, minor jump |
| @rightkit/license | package.json:160 | 0.1.8 | 0.1.8 | ok |
| @rightkit/logs | package.json:161 | 0.1.4 | 0.1.4 | ok |
| @rightkit/onboarding | package.json:162 | 0.1.0 | 0.3.0 | stale, minor jump x2 |
| @rightkit/settings-ui | package.json:163 | 0.1.0 | 0.3.0 | stale, minor jump x2 |
| @rightkit/tauri | package.json:164 | 0.1.1 | 0.2.0 | stale, minor jump |
| @rightkit/theme | package.json:165 | 0.1.0 | 0.2.1 | stale, minor jump x2 |
| @rightkit/updates | package.json:166 | 0.2.5 | 0.2.5 | ok |
| @rightkit/qa-chrome | package.json:179 | 0.1.3 | 0.1.3 | ok |
| @rightkit/release | package.json:180 | 0.2.135 | 0.2.147 | stale |
| RightKitSwift | ios-native/project.yml:19; ios-native/Tests/*/Package.swift:8,4 | exact 0.3.2 | registry 0.1.1 / staged 0.3.1 | ahead of record (registry drift) |

Stale pins: 14 (7 Cargo lines across 5 crates, 7 npm). Breaking-risk gaps (0.x minor jumps, unverified against changelogs): the six npm UI packages above. Cargo gaps are all patch level. Not adopted at all (no pin): rightkit-ort, -vad, -secrets, -db, -http, -fs, -settings, -shell, -hotkeys, -overlay, -llm, -sync, -automation, -qa, -sandbox, -watch, -search, -tts, -cache, @rightkit/shell, @rightkit/qa, @rightkit/platform-ui.

Earlier-wave items already done in the tree (do not re-plan): 3.2 model-store now adapts `rightkit-models` (no more ScrapeRight git dependency; no `git =` in any manifest); 3.3 capture on `rightkit-audio` capture; 3.4 partial (system identity/metrics re-exported from `rightkit_logs::system`, `crash_dump.rs` calls `rightkit_crash::capture_native`, `lib.rs:5`, `crash_dump.rs:39,55`); 3.5 partial (`heardright-parakeet-core/src/lib.rs:22` re-exports `rightkit_asr::parakeet_core::{backend,beam,scoring}`); 3.10 Windows job (`heardright-app-runtime/src/engine_supervisor/lifecycle.rs:43` is `rightkit_process::WindowsJob`).

### 3. Still-to-move table

Status key: ON = already on RightKit; DUP = in-house duplicate; N/A; GAP = RightKit gap.

| Capability | Status | In-house location (LOC) | Target owner | Note |
|---|---|---|---|---|
| Telemetry outbox + uploader + writer | DUP | heardright-telemetry `telemetry_outbox.rs` 1,065; `telemetry_upload/schedule.rs` 687, `lifecycle.rs` 196, `payload.rs` 164; `telemetry_writer.rs` 351 (total 2,463) | rightkit-logs 0.2.0 `outbox.rs`/`upload.rs`/`writer.rs` | rightkit-logs source says "merged from HeardRight telemetry_outbox.rs" (`crates/rightkit-logs/src/outbox.rs:3`). Keep event shapes (`events.rs` 663, `calibration.rs` 619, `contract.rs` 267) and consent policy app-side |
| System identity/metrics | ON | `heardright-telemetry/src/lib.rs:5` | rightkit-logs | done |
| Crash/dump/support bundle | DUP (partial) | `crash_dump.rs` 165 (wrapper), `heardright-dump-helper` 125 (near-identical to `rightkit-crash/src/bin/rightkit-dump-helper.rs`, differs at char 63), src-tauri `crash_report.rs` 70, `panic_redact.rs` 47, `hang_watchdog.rs` 129, `diagnostics_archive.rs` 107 (total 643) | rightkit-crash 0.2.0 (`hook.rs`, `bundle.rs`, `archive.rs`, `bin/`) | helper binary is a straight swap; hang watchdog may stay app policy |
| Model download/install | DUP (adapter remains) | `heardright-model-store`: `archive_install.rs` 244, `model_download_grant.rs` 170, `model_fetch.rs` 181, `shared.rs` 212, `resumable_download.rs` 146 (total 953); app policy to keep: `ane_cache_prune.rs` 186, `retired_model_cleanup.rs` 297, `model_paths.rs` 405, `whisper_model.rs` 446 | rightkit-models 0.2.7 (`archive.rs`, `rightapps.rs`, `download.rs`) | only 3 files reference `rightkit_models`; `archive_install.rs` still uses flate2/tar directly (`archive_install.rs:144`) |
| ASR beam/detok/bias/policy | DUP | heardright-parakeet-core `tdt_beam.rs` 285 (0.94 similar), `bias.rs` 243 (0.84), `detok.rs` 347 (0.78), `policy.rs` 192 (0.76) = 1,067; inference `whisper_coreml/logits.rs` 116 (0.99) + `mel.rs` 92 (0.95) = 208 | rightkit-asr 0.0.2 (`parakeet_core/`, `whisper/coreml/`) | module paths already stable via re-export; golden decoder outputs required |
| ASR long-form windowing | DUP (verify) | inference `asr/windowing.rs` 217 + parts of `asr/runtime.rs` 2,270 | rightkit-asr `longform.rs` | ScrapeRight's port header cites this file as source (`scraperight/crates/speech/src/asr_window.rs:5-9`) |
| VAD (Silero + spans) | DUP | heardright-inference `vad.rs` 611 | rightkit-vad 0.0.2 | rightkit-vad owns scorer + hysteresis + padding |
| ORT env/session | DUP | heardright-onnx-asr `environment.rs` 125 + `session.rs` 286; `ORT_DYLIB_PATH` set in `vad.rs:513,522`, `heardright-runtime/src/entry.rs:385`, `src-tauri/src/platform_setup.rs:141` (est total 470) | rightkit-ort 0.2.2 | `ort` stays pinned `=2.0.0-rc.13` (`heardright-inference/Cargo.toml:37,57,72`; `heardright-onnx-asr/Cargo.toml:21,31`), already rc.13 |
| WAV I/O | DUP (small) | `hound = 3.5` in engine, inference, runtime | rightkit-audio `wav.rs` | est 100 |
| Audio capture | ON | n/a | rightkit-audio capture | done (3.3) |
| Process / sidecar supervision | DUP (partial) | heardright-app-runtime `engine_supervisor/supervisor.rs` 962, `lifecycle.rs` 281, `threads.rs` 303, `engine_process.rs` 206 (est touched 1,000) | rightkit-process 0.3.3 (`restart.rs`, `health.rs`, `stall.rs`, `owned.rs`) | only `WindowsJob` adopted |
| Sandbox | N/A | no app sandboxing | rightkit-sandbox | not applicable to a dictation app |
| Global hotkeys / PTT | DUP, owner unpublished | src-tauri `fn_hotkey/*` 859, `middle_mouse.rs` 324, `middle_mouse_win.rs` 231, `hotkey.rs` 231, `hotkey_setup.rs` 129, `recording_cancel_hook/*` 1,029; heardright_core `ptt_policy.rs` 309 (total 3,112); `tauri-plugin-global-shortcut` at `src-tauri/Cargo.toml:63` | rightkit-hotkeys 0.1.0 (`ptt.rs`, `chord.rs`, `macos/fn_hotkey.rs`, `macos/middle_mouse.rs`, `windows/`) | BLOCKER: no registry record, cannot be pinned. `src-tauri/src/fn_hotkey/tap.rs:8` imports `crate::ptt_policy`, which mirrors `rightkit-hotkeys/src/ptt.rs` |
| Overlay / pill | DUP, owner unpublished | src-tauri `pill/state.rs` 1,221, `pill/commands.rs` 630, `pill_overlays.rs` 925, `pill_bridge_macos.rs` 381, `pill_dispatch_queue.rs` 128; heardright_core `pill/model.rs` 199; Swift `HeardRightPillMac` | rightkit-overlay 0.1.0 (306 LOC: state, hit test, placement) | BLOCKER: unpublished. RightKit crate is small; est 800 reusable |
| Desktop automation (AX/UIA/SendInput) | DUP, owner private | heardright_platform 8,267; command-host `focus.rs` 1,050 and `command_dispatch/keys.rs` 1,130; src-tauri `windows_uia/` | rightkit-automation (7,196 LOC) | BLOCKER: `publish = false`. Scanner reports 104 hits (`automation.api.uia-com` x51, `ax-element` x46, `sendinput` x7) |
| Secrets / keyring | DUP | src-tauri `credential_store.rs` 320 (Windows Credential Manager SQLCipher key), `license/keychain.rs` 258, `l3_keys.rs` 331, `commands/l3_keys.rs` 56 (total 965); `keyring-core` at `src-tauri/Cargo.toml:87` | rightkit-secrets 0.1.2 | persisted credential names must not change (`l3_keys.rs` comments) |
| Swift Keychain | DUP | `HeardRightAppleCredentials` 7 SecItem hits, `ios-native/Shared` 5, `ios-native/HeardRight` 4 | RightKitSwift `RightKitAppleCredentials` | ScrapeRight already moving (see below) |
| SQLite / SQLCipher | DUP | src-tauri `history/sqlcipher_store.rs` 444, `sqlcipher_conn.rs` 206, `migrations.rs` 90, `queries.rs` 105, `recovery.rs` 982 (rusqlite 0.40 at `src-tauri/Cargo.toml:127,185,203`); est touched 800 | rightkit-db 0.3.2 | bootstrap + key + recovery policy; schema stays app-owned |
| Settings persistence | DUP | src-tauri `settings.rs` 603; heardright_core `settings/blob.rs` 458 | rightkit-settings 0.1.0 (664 LOC) | est 1,000 |
| HTTP client | DUP | `ureq 3.4.2` in 5 manifests: `heardright-model-store/Cargo.toml:15`, `heardright-telemetry/Cargo.toml:17`, `heardright-text-pipeline/Cargo.toml:16`, `src-tauri/Cargo.toml:78`; ~35 call sites (`l3_cleanup/transport.rs` 10, `telemetry_upload/schedule.rs` 7, `license/lease.rs` 4) | rightkit-http 0.2.4 | est 500 touched |
| LLM clients (L3 cleanup) | DUP, owner unpublished | `heardright-text-pipeline/src/l3_cleanup/transport.rs` 386, `providers.rs` 280, `state.rs` 216 | rightkit-llm 0.1.0 | BLOCKER: unpublished |
| Cloud sync | DUP, owner unpublished | src-tauri `cloud_sync.rs` 1,552, `_ack.rs` 538, `_scheduler.rs` 383, `_pending.rs` 103, `_backoff.rs` 101 (+833 tests) | rightkit-sync 0.1.0 (3,179 LOC) | BLOCKER: unpublished; CloudKit helper in `rightkit/swift` |
| Licensing (Rust) | ON + DUP | `rightkit-license` + `rightkit-tauri` used; in-house `license/lease.rs` 786 (HR-ASAP-001 device lease, own Ed25519 verify at `lease.rs:287`), `lifecycle.rs` 389, `removal.rs` 227, `device.rs` 158, `license_clock.rs` 286 | rightkit-license / rightkit-tauri | GAP: device-lease client and clock-rollback policy are not in RightKit; either extract into rightkit-license or keep app-side |
| Licensing (JS) | ON | `src/lib/license/*` 4,176 total TS incl. UI; uses `@rightkit/license`, `@rightkit/tauri` | @rightkit/license | `lib/license/deviceLeases.ts:5` notes SDK lacks lease routes |
| Updater | ON (JS), DUP (Rust) | `src/lib/autoUpdates.ts` 243 uses `@rightkit/updates`; src-tauri `background_updates.rs` 160 + `commands/updates.rs` 227 on `tauri-plugin-updater` | @rightkit/updates, rightkit-shell | est 300 |
| Tauri shell (tray, single instance, autostart) | DUP | src-tauri `tray.rs` 211, `login_launch.rs` 171, `tauri-plugin-single-instance` (`Cargo.toml:112`), `tauri-plugin-autostart` (`:115`) | rightkit-shell 0.1.4 | scanner `desktop-shell` 2 hits |
| Speech service host | GAP | heardright-runtime `speech_service/` 1,411 (`transport.rs:160` owner-only pipe SDDL) | rightkit-service 0.1.3 + rightkit-speech-client 0.3.2 | `rightkit-speech-client/src/lib.rs:10` says "Hosting stays in HeardRight": host side is a RightKit gap |
| Legal / onboarding / settings UI / theme / app-shell | ON (stale pins) | `src/pages/LegalAcceptance.tsx` 116, `onboarding/*`, `lib/theme.ts` 77 | @rightkit/legal-ui, onboarding, settings-ui, theme, app-shell | residual: `theme.ts:43`, `App.tsx:211` prefers-dark; `pages/Settings.tsx:136` UA sniff vs @rightkit/platform-ui |
| QA harness | DUP | `tauri-app-next/scripts/qa-mini-clipboard-geometry.mjs` (7 CDP calls), `qa-rightkit-phase1-native.mjs:126,128` | @rightkit/qa 0.4.0 / @rightkit/qa-chrome, rightkit-qa | est 400 |
| OCR | N/A | none | rightkit-ocr | not used |
| TTS (qts) | N/A | none | rightkit-tts | not used |
| ffmpeg / media | N/A (macOS AVFoundation) | `heardright-inference/src/av_decode.rs` 170 (AVFoundation, no ffmpeg) | rightkit-media | keep |
| MCP / oauth | N/A | none | rightkit-mcp / rightkit-oauth | not used |
| KWS (sherpa) | GAP | `heardright-inference/src/sherpa_kws.rs` 1,330 | none | no RightKit owner |

Totals. Duplicates with a published owner today (rows DUP not marked unpublished/private): 14 items, about 11,100 LOC (telemetry 2,463; crash 643; model-store 953; ASR primitives 1,275; VAD 611; ORT 471; secrets 965; db 800; settings 1,061; shell 382; process 1,000; HTTP 500). Duplicates blocked on an unpublished or private owner: 5 items, about 16,300 LOC (hotkeys 3,112; LLM 882; sync 3,510; overlay 800; automation 8,040).

### 4. Ownership scanner
Command: `right-release ownership --app /Volumes/D/claude/heardright --json` (saved `apps-g3-heardright-ownership.json`). Tree clean, scan ran. `ok:false` because of `ownership-coverage` (10 files unscanned, all committed symlinks under `ios-native/Tests/**`) and 24 exception-record `problems` (records at the protected exception file miss `occurrences`, `policyDigest`, `minimumScannerVersion`, `minimumCatalogVersion`, `issued`, `expires`, `requestId`). 0 block findings, 180 advise findings (162 source, 18 dependency).

Rows 71: 16 `blocked` (use not declared), 13 `declared-dependency`, 42 `unscanned`.

| Capability (owner) | Findings | Where |
|---|---|---|
| desktop-automation (rightkit-automation) | 104 | heardright_platform macOS AX 26+, src-tauri `windows_uia/` 31, command-host UIA 16, SendInput 7 |
| onnx-runtime (rightkit-ort) | 19 | inference/onnx-asr Cargo + `ort::init`, `ORT_DYLIB_PATH` in `vad.rs:513,522`, `entry.rs:385`, `platform_setup.rs:141`, examples |
| os-credential-store (RightKitSwift) | 16 | Swift SecItem in macos-native + ios-native |
| desktop-overlay (rightkit-overlay) | 12 | `WS_EX_NOACTIVATE`, `ignoresMouseEvents` |
| qa-evidence (@rightkit/qa) | 7 | CDP calls in scripts |
| http-client (rightkit-http) | 4 | ureq manifests |
| audio (rightkit-audio) | 3 | `hound` manifests |
| sqlite-storage (rightkit-db) | 3 | rusqlite in src-tauri |
| desktop-shell, tauri-host-adapters (@rightkit/tauri 2, rightkit-tauri 1), theme-ui | 2, 3, 2 | autostart/single-instance; `license/device.rs:86`; prefers-dark |
| global-hotkeys, local-services, licensing, platform-ui, release-tooling | 1 each | `tauri-plugin-global-shortcut`; `transport.rs:160`; `lease.rs:287`; `Settings.tsx:136`; `assemble-macos-ax-qa.mjs:31` |

`declared-dependency` (adopted): speech-recognition, crash-reporting, structured-logging, model-installation, owned-processes, shared-speech-service, app-shell-ui, legal-assent, legal-ui, onboarding-ui, browser-qa, settings-ui, update-policy.

### 5. Handover list (HeardRight chat)
1. Bump stale pins in one commit: rightkit-models 0.2.7, speech-client 0.3.2 (3 lines), service 0.1.3, tauri 0.1.2, process 0.3.3; @rightkit/release 0.2.147; then the six UI packages in a separate step because of 0.x minor jumps (app-shell 0.3.0, legal-ui 0.2.0, onboarding 0.3.0, settings-ui 0.3.0, tauri 0.2.0, theme 0.2.1); est 60 LOC plus UI fallout; blocker: UI breaking-change read.
2. Telemetry outbox/uploader/writer → rightkit-logs@0.2.0 `Outbox`/uploader; est 2,463 LOC; blocker: payload/consent contract test, persisted `telemetry-outbox.json` shape must stay readable.
3. Parakeet primitives + Whisper CoreML helpers → rightkit-asr@0.0.2; est 1,275 LOC; blocker: decoder golden outputs, production path coverage unconfirmed.
4. Silero VAD `vad.rs` + onnx-asr ORT env/session → rightkit-vad@0.0.2 and rightkit-ort@0.2.2; est 1,080 LOC; blocker: DirectML/CoreML provider parity on both OSes.
5. Credentials: `credential_store.rs`, `license/keychain.rs`, `l3_keys.rs` → rightkit-secrets@0.1.2 (+ RightKitSwift for Swift SecItem); est 965 LOC; blocker: keychain service/account names are persisted user data, must stay byte-identical.
6. Model-store residue (`archive_install`, `model_download_grant`, `model_fetch`, `shared`, `resumable_download`) → rightkit-models@0.2.7; est 953 LOC; blocker: none beyond fixture-dir byte-identity.
7. Crash/diagnostics: swap `heardright-dump-helper` for `rightkit-dump-helper`, fold `crash_report`, `panic_redact`, `diagnostics_archive` → rightkit-crash@0.2.0; est 643 LOC; blocker: Windows minidump path and Tauri `externalBin` rename (`tauri.windows.conf.json:78`).
8. History/settings: SQLCipher bootstrap → rightkit-db@0.3.2; `settings.rs` + blob → rightkit-settings@0.1.0; est 1,860 LOC; blocker: on-disk DB/settings formats unchanged, recovery policy stays app-side.
9. ureq call sites → rightkit-http@0.2.4; est 500 LOC; blocker: none (L3 transport waits on rightkit-llm).
10. Needs RightKit action first: publish rightkit-hotkeys (3,112 LOC PTT/hotkeys), rightkit-overlay (~800), rightkit-llm (882), rightkit-sync (3,510); decide on rightkit-automation (private, 8,040 LOC); extract speech-service host (1,411) and device-lease client (786) as new RightKit capabilities.

---

## VoiceRight

### 1. Shape
`/Volumes/D/claude/voiceright/`: single Tauri 2 app, `package.json` (React 19, vite 8, vitest) and `src-tauri/Cargo.toml` (crate `voiceright`, lib `voiceright_lib`). Last commit 2026-10-10 (`c42cf61`); tree has 7 uncommitted entries (deleted `.agent/config.json`, `reference/dev-baseline/github-ci.yml`, edits to `LICENSE` and docs). Targets: Windows primary (NSIS, embedded QTS via `scripts/run-tauri-qts.ps1`), macOS configured but not shipped. No Cargo workspace; no in-house crates.

| Unit | LOC | Notes |
|---|---:|---|
| src-tauri/src/engine/mod.rs | 692 | voice store, clone/delete, provisioning, model dir, export |
| engine/qts_embedded.rs | 410 | in-process QTS via `qts` + `ort` |
| src-tauri/src/commands.rs | 242 | Tauri commands (app glue) |
| engine/qts_cli.rs | 159 | proof-only CLI subprocess path |
| engine/wav.rs | 50 | WAV header read/write |
| engine/artifacts.rs | 22 | model file names |
| lib.rs + main.rs | 35 | |
| src/ (React) | 523 | App.tsx 325; TitleBar 15, WindowControls 31, Kbd 42, usePlatform 34, tauri.ts 57 |
| reference/ | 4,963 | copied HeardRight shell, ViewRight license, ScrapeRight snippets (not built) |

### 2. RightKit adoption today
Only one pin exists.

| Pin | path:line | Pinned | Latest | State |
|---|---|---|---|---|
| @rightkit/release | package.json:49; pnpm-workspace.yaml:2 | 0.2.109 | 0.2.147 | stale |

Stale pins: 1. Other version debt: `ort = 2.0.0-rc.9` (`src-tauri/Cargo.toml:15`) vs suite rc.13 via rightkit-ort 0.2.2 (inventory item 3.17, medium-high risk). `qts` is a path dependency `../eval/qts-upstream/crates/qts` (`Cargo.toml:16`) and that directory does not exist, so the embedded build cannot resolve outside the author's machine (scanner coverage note: `local-dependency-path-unresolved`). `right-release.config.mjs:19` declares `distribution.provider = "r2-rightapps"`; the RightKit AGENTS invariant says private products use `r2` (verify, not part of this inventory).

### 3. Still-to-move table

| Capability | Status | In-house location (LOC) | Target owner | Note |
|---|---|---|---|---|
| TTS engine (Qwen3 QTS) | DUP | `engine/qts_embedded.rs` 410, `engine/qts_cli.rs` 159 (delete: proof-only) | rightkit-tts 0.1.0 (`client`/`engine` features) + rightkit-qts 0.2.4 | replaces the missing local `qts` path dependency |
| Voice clone / synth orchestration, voice store | DUP | `engine/mod.rs` 692 | rightkit-tts `client.rs`, `chunk.rs`, `protocol.rs` | voice-prompt persistence format must be proven unchanged |
| Model files, provisioning, remote download | DUP | `engine/mod.rs` provisioning (`provision_from_bundle`, `copy_if_needed`, `files_match`), `engine/artifacts.rs` 22; docs defer "remote model hosting/checksum/progress" (`docs/04-license-packaging.md`) | rightkit-models 0.2.7, rightkit-tts `models.rs` | do not hand-roll remote download; adopt |
| ONNX Runtime | DUP | `ort` rc.9 direct; `ORT_DYLIB_PATH` handling in `engine/qts_embedded.rs:216-219` and `engine/mod.rs:668,686,687`; `artifacts.rs:16` | rightkit-ort 0.2.2 | breaking: rc.9 to rc.13 |
| WAV I/O | DUP | `engine/wav.rs` 50, `read_wav_info` | rightkit-tts `wav.rs` / rightkit-audio `wav.rs` | |
| Updater | DUP | `tauri-plugin-updater` direct (`Cargo.toml`), `tauri.conf.json` endpoint | @rightkit/updates 0.2.5 | |
| Licensing | not started | docs say "copy ViewRight pattern later" (`docs/04-license-packaging.md`), `reference/viewright-license/` 3 files | @rightkit/license 0.1.8 + rightkit-license 0.1.3 + rightkit-tauri 0.1.2 | adopt, never copy `reference/`; delete `reference/` afterwards |
| Shell UI (title bar, window controls, kbd, platform hook) | DUP | `src/components/TitleBar.tsx` 15, `WindowControls.tsx` 31, `Kbd.tsx` 42, `hooks/usePlatform.ts` 34 (copies of `reference/heardright-shell/*`) | @rightkit/app-shell 0.3.0, @rightkit/platform-ui 0.2.0 | scanner `app-shell-ui` x4 |
| Settings / legal / onboarding / theme UI | not started | none | @rightkit/settings-ui, legal-ui, legal, onboarding, theme | add at feature time |
| Logging, crash, settings, secrets, db, http | not started | no logging/crash at all | rightkit-logs, crash, settings, secrets | no in-house duplicate; add rather than build |
| Audio capture, VAD, ASR, OCR, ffmpeg | N/A | none (TTS-only) | | note: clone reference audio staging may need rightkit-media later |
| Process / sandbox / hotkeys / overlay / MCP / oauth / sidecar protocol | N/A | none | | |
| QA harness | GAP/none | no rightkit-qa wiring; `scripts/check-boundary.mjs` is an in-house guard | rightkit-qa, @rightkit/qa | |

Duplicates with a published owner: 8 items, about 1,510 LOC (engine 1,333 incl. qts_cli 159 to delete; JS shell 179 less overlap). No unpublished-owner blockers. RightKit gaps: none specific; rightkit-tts needs to support the voice-clone prompt flow in `engine/mod.rs:94-143` (verify API parity first).

### 4. Ownership scanner
`right-release ownership --app /Volumes/D/claude/voiceright --json` failed: `protected-evidence`, "protected release checkout has tracked or untracked changes" (saved as `apps-g3-voiceright-ownership.json`, `ok:false`, no rows). Fallback `--staged` (scans the index, i.e. HEAD for an unstaged tree), saved `apps-g3-voiceright-ownership-staged.json`: `ok:true`, 0 blocking, 16 advisory, 1 coverage gap.

| Capability | Count | Where |
|---|---:|---|
| onnx-runtime (rightkit-ort) | 5 | `src-tauri/Cargo.toml` ort rc.9; `engine/mod.rs:668,686,687`; `reference/scraperight/main.rs:30` |
| text-to-speech (rightkit-qts) | 1 | `src-tauri/Cargo.toml` qts |
| app-shell-ui | 4 | `src/components/TitleBar.tsx:8,9`; `reference/heardright-shell/components/TitleBar.tsx:10,18` |
| theme-ui | 2 | `reference/heardright-shell/lib/theme.ts:102,121` |
| tauri-host-adapters | 3 | `reference/viewright-license/license.rs:60,73,82` |
| update-policy | 1 | `reference/viewright-license/updateLicense.ts:253` |

10 of the 16 hits are in `reference/` (non-built copies), which is itself an argument for deleting that directory. Coverage gap: `src-tauri/Cargo.toml` unscanned (`local-dependency-path-unresolved`).

### 5. Handover list (VoiceRight chat)
1. Replace local `qts` path dependency + `qts_embedded.rs`/`qts_cli.rs`/`engine/mod.rs` synth and voice flow → rightkit-tts@0.1.0 (+ rightkit-qts@0.2.4); est 1,100 LOC; blocker: voice-clone prompt and cloned-voice store compatibility, VoiceRight e2e (`pnpm verify:embedded`).
2. `ort 2.0.0-rc.9` and `ORT_DYLIB_PATH` handling → rightkit-ort@0.2.2 (rc.13); est 60 LOC; blocker: medium-high risk migration (inventory 3.17), Vulkan/ONNX vocoder smoke.
3. Model provisioning (bundle seeding, hash check, future remote download) → rightkit-models@0.2.7; est 250 LOC; blocker: model manifest for `qwen3-tts-0.6b-q8_0.gguf` and vocoder.
4. `engine/wav.rs` → rightkit-tts `wav` / rightkit-audio `wav`; est 50 LOC; blocker: none.
5. Bump @rightkit/release 0.2.109 → 0.2.147 (`package.json:49`, `pnpm-workspace.yaml:2`); est 2 lines; blocker: none.
6. UI shell: TitleBar, WindowControls, Kbd, usePlatform → @rightkit/app-shell@0.3.0 / @rightkit/platform-ui@0.2.0; est 120 LOC; blocker: none.
7. Updater → @rightkit/updates@0.2.5; licensing → @rightkit/license@0.1.8 + rightkit-license@0.1.3 + rightkit-tauri@0.1.2, then delete `reference/`; est 150 LOC new wiring, 4,963 LOC removed; blocker: RightApps backend registration for `voiceright` (listed as deferred in `docs/04-license-packaging.md`).

---

## ScrapeRight

### 1. Shape
`/Volumes/D/claude/scraperight/`: Cargo workspace (`Cargo.toml` members: src-tauri + 9 crates; `vendor/parakeet-rs` excluded and used by path), `package.json` (React 19, vite), `src-tauri/`, `macos-native/ScrapeRightAppleCredentials` (Swift package), `ios-native/` (45,741 Swift LOC, xcodegen). Last commit 2026-10-10 (`4dc55e4`). Working tree is dirty (29 entries): an uncommitted RightKit migration is in progress (parakeet-rs `ort` to `rightkit-ort`, `ort_common`, engine runtime, Swift credentials helper rewritten to RightKitSwift). The handover chat must coordinate with whichever chat owns those edits before touching them. Targets: macOS (universal DMG), Windows, iOS.

| Crate / package | Rust LOC | Notes |
|---|---:|---|
| src-tauri (`scraperight`) | 11,901 | shell: engine supervisor 1,177, pro_bridge 951, cloud_sync/* ~2,500, commands_models 598, license 570, browser 455+254, playback 287 |
| crates/engine | 14,829 | sidecar: extraction, history 1,227, whisper 972, queue, browser, article/TLS |
| vendor/parakeet-rs | 8,902 (7,834 in `src/`) | vendored parakeet-rs, same upstream (0.3.5) as rightkit-asr |
| crates/speech | 1,893 | Parakeet runtime: asr_window 648, coreml_asr 376, asr 316, coreml 277, vad 135 |
| crates/audio | 1,146 | yt-dlp + ffmpeg normalize to 16 kHz WAV |
| crates/protocol | 1,208 | wire types 658, license.rs 178 |
| crates/ocr | 1,069 | rightkit-ocr native + Windows PP-OCRv6 fallback |
| crates/store | 408 | rightkit-db history index |
| crates/detect | 316 | |
| crates/ort_common | 105 | candidate-path policy over rightkit-ort (keep) |
| crates/model-store | 171 | adapter over rightkit-models (3.1 done; was 4,429) |
| macos-native | 375 Swift | credentials helper |
| src/ (React) | 10,917 TS/TSX | Detail 1,257, DesktopWorkspace 986, Settings 698 |

### 2. RightKit adoption today (pins)

| Pin | path:line | Pinned | Latest | State |
|---|---|---|---|---|
| rightkit-ort | crates/ort_common/Cargo.toml:10; crates/ocr/Cargo.toml:35; crates/speech/Cargo.toml:17; crates/engine/Cargo.toml:19; src-tauri/Cargo.toml:29; vendor/parakeet-rs/Cargo.toml:100 (uncommitted) | =0.2.0 | 0.2.2 | stale x6 (patch) |
| rightkit-ocr | crates/ocr/Cargo.toml:17 | =0.1.2 | 0.1.2 | ok |
| rightkit-license | crates/protocol/Cargo.toml:12; src-tauri/Cargo.toml:59 | =0.1.3 | 0.1.3 | ok |
| rightkit-models | crates/model-store/Cargo.toml:14 | =0.2.6 | 0.2.7 | stale |
| rightkit-framed-sidecar | crates/engine/Cargo.toml:17; src-tauri/Cargo.toml:27 | =0.1.0 | 0.1.0 | ok |
| rightkit-fs | crates/engine/Cargo.toml:18; src-tauri/Cargo.toml:28 | =0.3.0 | 0.3.3 | stale x2 (patch) |
| rightkit-secrets (secrets crate) | crates/engine/Cargo.toml:57; src-tauri/Cargo.toml:62 | =0.1.0 | 0.1.2 | stale x2 (patch) |
| rightkit-db | crates/store/Cargo.toml:9 | =0.2.1 | 0.3.2 | stale, 0.x minor jump (breaking risk) |
| rightkit-logs | src-tauri/Cargo.toml:55 | =0.1.0 | 0.2.0 | stale, 0.x minor jump (breaking risk) |
| rightkit-tauri | src-tauri/Cargo.toml:60 | =0.1.1 | 0.1.2 | stale |
| @rightkit/app-shell | package.json:78 | 0.2.0 | 0.3.0 | stale, minor jump |
| @rightkit/legal-ui | package.json:79 | 0.1.3 | 0.2.0 | stale, minor jump |
| @rightkit/license | package.json:80 | 0.1.6 | 0.1.8 | stale |
| @rightkit/logs | package.json:81 | 0.1.3 | 0.1.4 | stale |
| @rightkit/media-ui | package.json:82 | 0.2.5 | 0.3.0 | stale, minor jump |
| @rightkit/onboarding | package.json:83 | 0.2.2 | 0.3.0 | stale, minor jump |
| @rightkit/platform-ui | package.json:84 | 0.1.2 | 0.2.0 | stale, minor jump |
| @rightkit/settings-ui | package.json:85 | 0.2.7 | 0.3.0 | stale, minor jump |
| @rightkit/tauri | package.json:86 | 0.1.0 | 0.2.0 | stale, minor jump |
| @rightkit/theme | package.json:87 | 0.2.0 | 0.2.1 | stale |
| @rightkit/updates | package.json:88 | 0.2.5 | 0.2.5 | ok |
| @rightkit/legal | package.json:101 | 0.3.0 | 0.3.1 | stale |
| @rightkit/qa-chrome | package.json:102 | 0.1.0 | 0.1.3 | stale |
| @rightkit/release | package.json:103 | 0.2.133 | 0.2.147 | stale |
| RightKitSwift | ios-native/project.yml:22; macos-native/ScrapeRightAppleCredentials/Package.swift:11 (uncommitted) | exact 0.3.7 | registry 0.1.1 / staged 0.3.1 | ahead of record |

Stale pins: 27 (14 Cargo lines, 13 npm). Breaking-risk gaps (0.x minor): rightkit-db 0.2.1 to 0.3.2, rightkit-logs 0.1.0 to 0.2.0, and the seven npm UI packages. Not adopted (no pin): rightkit-http, -pdf, -playback, -browser, -process, -vad, -asr, -settings, -sync, -qa, @rightkit/qa.

Mixed signal: `src-tauri/src/diagnostics.rs` already uses `rightkit_logs` (6 refs) at 0.1.0 while `logging.rs` still builds its own tracing-appender stack.

### 3. Still-to-move table

| Capability | Status | In-house location (LOC) | Target owner | Note |
|---|---|---|---|---|
| Model store | ON | `crates/model-store` 171 adapter | rightkit-models | 3.1 done (was 4,429); bump pin |
| Framed sidecar | ON | engine `main_stdio.rs`, `engine_supervisor.rs:48` use `rightkit_framed_sidecar` | rightkit-framed-sidecar | 3.15 done; `scraperight-protocol` keeps app wire types (658) |
| ORT resolution | ON | `crates/ort_common` 105 | rightkit-ort | 3.12 done; bump pin |
| Native OCR | ON | `crates/ocr/src/lib.rs:93-95` calls `rightkit_ocr::{vision,win_native}` | rightkit-ocr 0.1.2 | 3.9 done for native |
| OCR Windows ONNX fallback | DUP | `crates/ocr/src/support/boxes.rs` 234 and `preprocess.rs` 61 (byte-identical to `rightkit-ocr/src/support/*`), `model_files.rs` 59, `session.rs` 46, `decode.rs` 30, fallback logic in `lib.rs` (~300); est total 730 | rightkit-ocr `onnx` feature (default-features is off at `Cargo.toml:17`) | the Cargo comment says fallback "stays here because bound to the model store"; rightkit-ocr now ships a "verified two-stage ONNX fallback" |
| ASR Parakeet runtime | DUP | `vendor/parakeet-rs/src` 7,834 (same upstream as rightkit-asr: `rightkit-asr/UPSTREAM.toml`, `README.md:64`); `crates/speech` `asr_window.rs` 648 (ported from HeardRight, near `rightkit-asr/src/longform.rs`), `asr.rs` 316, `coreml_asr.rs` 376, `coreml.rs` 277, `vad.rs` 135, `wav.rs` 25 (= 1,777) | rightkit-asr 0.0.2 (`transducer`, `model_tdt`, `longform`, `parakeet_coreml`) + rightkit-vad 0.0.2 | verify: API parity (`ScrapeRightTransducer`), CoreML 4-stage bundle support in `parakeet_coreml` (786 LOC). Total est 9,600 |
| Whisper turbo (Pro add-on) | DUP (verify) | engine `whisper.rs` 972 (whisper-cli subprocess), `asr_route.rs` 386 | rightkit-asr `whisper/cpp.rs` | |
| Audio normalize (ffmpeg/yt-dlp) | DUP | `crates/audio` `lib.rs` 624, `process.rs` 243, `args.rs` 232, `wav.rs` 25 | rightkit-media 0.2.4 (verified FFmpeg runner), rightkit-audio `wav.rs` | yt-dlp invocation stays app-side; est 600 |
| Compatibility playback | DUP | src-tauri `playback.rs` 287 ("source-preserving compatibility playback using LGPL FFmpeg") | rightkit-playback 0.1.1 | same stated purpose |
| Logging | DUP | src-tauri `logging.rs` 125, engine `logging.rs` 142, `tracing-appender` in both manifests (`src-tauri/Cargo.toml:52`, `crates/engine/Cargo.toml:56`) | rightkit-logs 0.2.0 | est 267; log file locations/policy app-owned |
| HTTP | DUP | `ureq` at `src-tauri/Cargo.toml:57`, `crates/engine/Cargo.toml:50`; 7 call sites (`article.rs`, `article_browser.rs` x2, `tiktok.rs`, `whisper.rs`, `commands_models.rs`, `pro_bridge.rs` x2) | rightkit-http 0.2.4 | est 350 |
| PDF | DUP | engine `extraction/pdf.rs` 93, `extraction_documents.rs` 243; `pdf-extract` at `crates/engine/Cargo.toml:39`, `pdfium-render` :40 | rightkit-pdf 0.2.0 (text extraction option) | est 336; verify extraction parity |
| Browser automation | DUP | src-tauri `browser.rs` 455, `browser_agent.rs` 254; engine `browser/agent.rs` 262, `agent_process.rs` 200, `browser/mod.rs` 81, `extraction/article_browser.rs` 349 (total 1,601; `agent-browser` subprocess) | rightkit-browser 0.2.2 (CDP runtime; block-tier capability) | est 1,600; behavior change (subprocess to CDP) needs e2e |
| QA browser (puppeteer) | DUP, BLOCK | `puppeteer-core` at `package.json:113`; used by `tests/helpers/qaBrowser.ts`, `scripts/qa-capture.ts`, 3 interaction tests | @rightkit/qa-chrome 0.1.3 + @rightkit/qa 0.4.0 | scanner blocking finding |
| Process supervision | DUP | src-tauri `engine_supervisor.rs` 1,177, `engine_supervisor_frames.rs` 87, `helper_process.rs` 324 (header: "ported from HeardRight") | rightkit-process 0.3.3 | est 900; no pin today |
| Secrets/keyring | ON (Rust), DUP (Swift) | `rightkit-secrets` in engine and shell; Swift helper `main.swift` 358 deleted in working tree and moved to RightKitSwift 0.3.7 | rightkit-secrets, RightKitSwift | uncommitted; verify before commit |
| Licensing | ON + DUP | src-tauri `license.rs` 570, `pro_bridge.rs` 951, `crates/protocol/src/license.rs` 178; own Ed25519 verifiers at `protocol/src/license.rs:68`, `pro_bridge.rs:436`; `tauri.rs.standard-command-fn` at `license.rs:527` | rightkit-license, rightkit-tauri | est 300 |
| SQLite | ON | `crates/store` 408 on rightkit-db | rightkit-db | stale pin with breaking-risk gap |
| Atomic write / safe fs | ON | rightkit-fs pinned in engine and shell | rightkit-fs | bump pin; no in-house `write_atomic` found |
| Cloud sync (CloudKit) | DUP, owner unpublished | src-tauri `cloud_sync/coordinator.rs` 718, `schema.rs` 582, `transport.rs` 298, `engine.rs` 290, `ledger.rs` 181, `cloudkit.rs` 125, `cloud_sync.rs` 248, `commands_sync.rs` 76 (= 2,518) | rightkit-sync 0.1.0 (3,179 LOC) | BLOCKER: unpublished |
| Shell UI | DUP | `src/components/DesktopWorkspace.tsx` 986 (drag regions lines 277,298,624), `TitleBar.tsx` 136 (lines 24,36), `lib/theme.ts` 193 (prefers-dark lines 103,173) | @rightkit/app-shell 0.3.0, @rightkit/theme 0.2.1 | est 400 |
| Settings/legal/onboarding/updates UI | ON | `views/Settings.tsx` 698 etc. | @rightkit/settings-ui, legal-ui, onboarding, updates | stale pins |
| Settings persistence (Rust) | N/A/GAP (verify) | engine/shell settings in `scraperight-protocol::Settings` | rightkit-settings | not adopted; est unknown |
| TTS, MCP, oauth, LLM, hotkeys, overlay | N/A | none | | |
| Sandbox | N/A | none | rightkit-sandbox | |

Totals. Duplicates with a published owner: 12 items, about 15,400 LOC (ASR runtime 9,611; OCR fallback 730; playback 287; logging 267; HTTP 350; PDF 336; browser 1,601; process 900; license 300; audio 600; JS shell 400). Blocked on unpublished owner: cloud sync 2,518 LOC (rightkit-sync).

### 4. Ownership scanner
`right-release ownership --app /Volumes/D/claude/scraperight --json` failed: `protected-evidence`, tracked or untracked changes (saved `apps-g3-scraperight-ownership.json`, `ok:false`, no rows). Fallback `--staged`, saved `apps-g3-scraperight-ownership-staged.json` (scans the index, so it reports HEAD state and does not see the uncommitted migration): `ok:false`, 2 blocking, 30 advisory, 2 coverage gaps (`src-tauri/Cargo.toml` and `src-tauri/Cargo.lock` unscanned: lock format/package list).

Blocking:
- `browser.npm.puppeteer-core` (capability browser-automation, owner rightkit-browser, policy `banned`) at `package.json` (`^25.9.0`).
- `dependency-not-allowlisted` for `ort 2.0.0-rc.12` at `vendor/parakeet-rs/Cargo.toml` (HEAD). The working tree already removes it; committing that change should clear this block.

Advisory (30): os-credential-store 7 (`macos-native/.../main.swift`, deleted in working tree), app-shell-ui 5, audio 4 (`hound` in audio, engine, speech, parakeet-rs), tauri-host-adapters 3, http-client 2, structured-logging 2 (`tracing-appender`), onnx-runtime 2 (parakeet-rs ort, `crates/engine/src/main.rs:140` `ORT_DYLIB_PATH`), licensing 2, theme-ui 2, pdf-engine 1 (`pdf-extract`).

### 5. Handover list (ScrapeRight chat)
1. Land and commit the in-flight working tree first (parakeet-rs on rightkit-ort, ort_common/engine runtime, Swift credentials helper) so the HEAD scan clears the rc.12 block; working tree shows +220/-571 LOC (unstaged); blocker: confirm which chat owns those edits and that fixture/golden tests pass.
2. Remove `puppeteer-core` → @rightkit/qa-chrome@0.1.3 / @rightkit/qa@0.4.0 in `tests/helpers/qaBrowser.ts` and `scripts/qa-capture.ts`; est 200 LOC; blocker: none; clears the second blocking finding.
3. Pin reconciliation in one pass: rightkit-ort 0.2.2 (6 lines), models 0.2.7, fs 0.3.3 (2), secrets 0.1.2 (2), tauri 0.1.2, release 0.2.147, plus the breaking jumps rightkit-db 0.3.2 and rightkit-logs 0.2.0 and the seven npm UI packages; est 40 LOC plus fallout; blocker: db 0.2 to 0.3 and logs 0.1 to 0.2 changelogs.
4. `vendor/parakeet-rs` (7,834) + `crates/speech` ASR/window/VAD (1,777) → rightkit-asr@0.0.2 + rightkit-vad@0.0.2; est 9,600 LOC; blocker: API parity for the locked Parakeet TDT v3 route (Windows DirectML encoder split, macOS CoreML 4-stage bundle), decode golden tests on both OSes.
5. Browser stack (`browser.rs`, `browser_agent.rs`, engine `browser/*`, `article_browser.rs`) → rightkit-browser@0.2.2; est 1,600 LOC; blocker: replaces `agent-browser` subprocess with CDP, needs extraction e2e (article/TLS fallbacks).
6. Windows OCR ONNX fallback → rightkit-ocr `onnx` feature; est 730 LOC; blocker: model-files verification vs ScrapeRight's model store and Windows OCR golden tests.
7. Engine supervisor + helper process → rightkit-process@0.3.3; est 900 LOC; blocker: Windows job/kill-tree behavior and engine-restart tests.
8. Logging → rightkit-logs@0.2.0 (drop both `tracing-appender` stacks), HTTP → rightkit-http@0.2.4, playback → rightkit-playback@0.1.1, PDF → rightkit-pdf@0.2.0; est 1,240 LOC combined; blocker: log file locations and diagnostics bundle contract, PDF text-extraction parity.
9. Audio crate → rightkit-media@0.2.4 + rightkit-audio `wav`; est 600 LOC; blocker: ffmpeg staging/verification path differs from `rightkit-media` lifecycle.
10. Needs RightKit action first: publish rightkit-sync (2,518 LOC CloudKit coordinator); consider a shared license-lease/pro-bridge helper (`pro_bridge.rs` 951 plus `license.rs` 570) in rightkit-license/-tauri.

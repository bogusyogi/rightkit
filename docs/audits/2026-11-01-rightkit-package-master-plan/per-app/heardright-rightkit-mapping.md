# HeardRight: RightKit mapping

Repo `heardright` at `7946fbdc` (main). Generated 2026-11-01. Every tracked code file (Rust, TS/JS, Swift, Kotlin, Python, shell, CSS/HTML, TOML, SQL) is assigned to exactly one target. Full per-file list: [`heardright-rightkit-files.json`](heardright-rightkit-files.json).

Package names are final, after cross-app merging (see master plan in RightKit `docs/audits/2026-11-01-rightkit-package-master-plan.md`).

## Summary

Corrected 2026-11-11 by the shape review (`docs/audits/2026-11-11-shape-matrix/reports/library-and-swift-mapping-redraw.md`): `rightkit-library` (wrong capability) re-pointed to `rightkit-transcript-library`; nothing is planned into `RightKitUI` (scaffold and onboarding are app screens) or `RightKitWatch` (a Mac file watcher): the 19 Apple Watch files go to `RightKitWatchBridge`, `RightKitAudio`, a planned `RightKitWatchRecording` and the app. The 48 iOS test files the inventory JSON in the HeardRight tree maps to `RightKitHubQA` are app tests here (APPTEST), as before. Totals below are recomputed from the corrected rows; the per-file list in the app repo was not changed here.

| Action | Meaning | Files | Lines |
|---|---|---:|---:|
| NEW | new RightKit package (or new piece of one) | 716 | 172,908 |
| EXTEND | existing RightKit package gains this | 580 | 144,472 |
| ADOPT | RightKit already ships it; delete app copy & use owner | 67 | 17,484 |
| APPTEST | app journey/e2e test: stays with the app (P7, §6), runs on rightkit-qa | 242 | 35,984 |
| VENDOR-KEEP | vendored 3rd-party: keep as plain dependency, delete vendored copy | 41 | 50,957 |
| DROP | delete | 71 | 31,365 |
| SHELL | thin app composition that stays in app | 46 | 6,747 |
| CONFIG | app manifest/config that stays | 5 | 249 |
| NOTCODE | docs/data, not code | 73 | 24,175 |
| **Total** | | **1,841** | **484,341** |

Code leaving the app (NEW + EXTEND + ADOPT + LEGION + vendor + DROP): **417,186 lines**. Staying in the app as product code: **0 lines**; as shell/config: **6,996 lines** (2% of code), plus **35,984 lines** of app journey/e2e tests.

## Mapping table

| RightKit target | Kind | Action | What moves | Files | Lines | Where (sample) |
|---|---|---|---|---:|---:|---|
| `@rightkit/config` | npm | NEW | shared presets | 8 | 152 | `tauri-app-next/`, `tauri-app-next/.cargo/` |
| `@rightkit/dictation-ui` | npm | NEW | dictation home/controls, banners, command palette, PTT, sounds, metrics | 58 | 4,815 | `tauri-app-next/src/assets/`, `tauri-app-next/src/components/`, `tauri-app-next/src/components/ui/` +5 dirs |
| `@rightkit/dictation-ui` | npm | NEW | recording pill webview UI | 12 | 2,738 | `tauri-app-next/src/pill/` |
| `@rightkit/dictation-ui` | npm | NEW | polish & vocabulary pages | 4 | 1,122 | `tauri-app-next/src/pages/`, `tauri-app-next/src/pages/polish/` |
| `@rightkit/library-ui` | npm | NEW | transcript history & file transcribe | 11 | 2,870 | `tauri-app-next/src/components/`, `tauri-app-next/src/pages/`, `tauri-app-next/src/pages/history/` |
| `RightKitDictation` | Swift | NEW | iOS dictation: recording session, warm capture, long-form, import queue, live activity | 30 | 10,514 | `ios-native/HeardRight/`, `ios-native/Shared/` |
| `RightKitLibrary` | Swift | NEW | Swift transcript library, insights, cloud sync | 9 | 2,791 | `ios-native/HeardRight/`, `ios-native/Shared/` |
| `RightKitLibrary` | Swift | NEW | SwiftUI history & settings views | 3 | 2,193 | `ios-native/HeardRight/` |
| `RightKitWatchRecording` | Swift | NEW | watch recording, planned module (not in RightKit yet): recorder controller on `RightKitAudio.AudioFileRecorder`, processor, recovery, pending store & policy, state, mode, telemetry (was `RightKitWatch`, the macOS FSEvents file watcher) | 11 | 1,787 | `ios-native/HeardRightWatch/`, `ios-native/HeardRight/`, `ios-native/Shared/` |
| `rightkit-dictation` | crate | NEW | dictation runtime: mic capture & conditioning, session state machine, JSON-RPC sidecar worker, wake listener, recording lifecycle | 175 | 60,504 | `tauri-app-next/heardright-app-runtime/`, `tauri-app-next/heardright-app-runtime/examples/`, `tauri-app-next/heardright-app-runtime/src/` +21 dirs |
| `rightkit-engine-host` | crate | NEW | engine binary framework: sidecar + headless CLI modes, command registration, contract export & TS codegen, human/JSON presentation, engine client | 1 | 374 | `tauri-app-next/heardright_core/src/` |
| `rightkit-mesh` | crate | NEW | device pairing (Swift) | 3 | 873 | `ios-native/HeardRight/` |
| `rightkit-ml-lab` | crate | NEW | model training & evaluation tooling: KWS/wake corpora & conditioning, ASR bakeoffs, polish-model training/eval, CoreML export | 295 | 53,827 | `scripts/`, `tauri-app-next/bakeoff/l1-exact-app-wire-regression-v1/`, `tauri-app-next/bakeoff/l1-four-family-training-v1/corpus-candidates/` +41 dirs |
| `rightkit-text-polish` | crate | NEW | deterministic polish, LLM cleanup (L1-L3), vocabulary & learning, screen context | 52 | 17,290 | `tauri-app-next/heardright-text-pipeline/`, `tauri-app-next/heardright-text-pipeline/src/`, `tauri-app-next/heardright-text-pipeline/src/l3_cleanup/` +7 dirs |
| `rightkit-text-polish` | crate | NEW | Swift polish clients (Apple Intelligence, BYOK) | 8 | 1,274 | `ios-native/HeardRight/`, `ios-native/Shared/` |
| `rightkit-voice-commands` | crate | NEW | voice command recognition/classification, launch & OS command adapters | 36 | 9,784 | `tauri-app-next/heardright-command-host/`, `tauri-app-next/heardright-command-host/src/`, `tauri-app-next/heardright-command-host/src/command_dispatch/` +3 dirs |
| `@rightkit/onboarding` | npm | EXTEND | onboarding pages | 22 | 3,012 | `tauri-app-next/src/pages/`, `tauri-app-next/src/pages/onboarding/` |
| `@rightkit/onboarding` | npm | EXTEND | mic & calibration onboarding steps | 7 | 1,316 | `tauri-app-next/src-tauri/src/`, `tauri-app-next/src-tauri/src/onboarding_calibration/` |
| `@rightkit/qa` | npm | EXTEND | contract tests & QA scripts | 83 | 13,214 | `tauri-app-next/scripts/`, `tauri-app-next/scripts/lib/` |
| `@rightkit/qa` | npm | EXTEND | deadcode baseline & contracts | 4 | 637 | `scripts/` |
| `@rightkit/react-shell` | npm | EXTEND | app frame composition, splash, engine API client (app keeps a short composition) | 3 | 946 | `tauri-app-next/src/` |
| `@rightkit/release` | npm | EXTEND | build/package/release scripts | 102 | 20,331 | `tauri-app-next/scripts/` |
| `@rightkit/release` | npm | EXTEND | release config logic moves into @rightkit/release; app keeps a JSON data config (P8) | 1 | 327 | `tauri-app-next/` |
| `@rightkit/settings-ui` | npm | EXTEND | settings, shortcuts, controls, model update | 32 | 6,175 | `tauri-app-next/src/components/about/`, `tauri-app-next/src/components/model-update/`, `tauri-app-next/src/components/settings/` +3 dirs |
| `@rightkit/theme` | npm | EXTEND | app stylesheet: brand tokens & shell css from @rightkit/theme / react-shell | 1 | 587 | `tauri-app-next/src/` |
| `@rightkit/theme` | npm | EXTEND | Swift brand tokens | 3 | 271 | `ios-native/HeardRight/` |
| `@rightkit/theme` | npm | EXTEND | icon, tray & installer art generation from the brand file | 2 | 240 | `tauri-app-next/src-tauri/scripts/`, `tauri-app-next/src-tauri/windows/installer/` |
| `RightKitIntents` | Swift | EXTEND | app intents & voice commands | 3 | 1,224 | `ios-native/Shared/` |
| `RightKitLiveActivity` | Swift | EXTEND | recording live activity views | 3 | 934 | `ios-native/HeardRightSharedUI/` |
| `RightKitSpeech` | Swift | EXTEND | on-device ASR & KWS bridges | 9 | 1,740 | `ios-native/HeardRight/` |
| `RightKitWatchBridge` | Swift | EXTEND | watch transfer: the two `WCSessionDelegate` halves (watch transfer, phone inbox) and delivery, on `TransferLedger` / `DualPathSender` (was `RightKitWatch`) | 3 | 972 | `ios-native/HeardRightWatch/`, `ios-native/HeardRight/`, `ios-native/Shared/` |
| `rightkit-android` | crate | EXTEND | Android app scaffold: activity, permissions, screens, QR scanner, mesh client | 17 | 1,296 | `android/app/src/main/java/com/orthiclabs/heardright/`, `android/app/src/main/java/com/orthiclabs/heardright/audio/`, `android/app/src/main/java/com/orthiclabs/heardright/models/` +3 dirs |
| `rightkit-app-settings` | crate | EXTEND | settings blob & patch | 3 | 1,470 | `tauri-app-next/heardright_core/src/settings/` |
| `rightkit-asr` | crate | EXTEND | Parakeet TDT core & ORT exec, CoreML, Whisper, KWS keyword spotting, VAD, calibration, file transcription | 88 | 33,816 | `tauri-app-next/heardright-inference/`, `tauri-app-next/heardright-inference/examples/`, `tauri-app-next/heardright-inference/src/` +10 dirs |
| `rightkit-automation` | crate | EXTEND | text delivery into focused app: paste, streaming insertion & reconcile, UIA/AX, terminal targets, TSF tip, focus continuity | 60 | 19,141 | `tauri-app-next/heardright_core/src/`, `tauri-app-next/heardright_platform/`, `tauri-app-next/heardright_platform/examples/` +11 dirs |
| `rightkit-automation` | crate | EXTEND | system media playback pause/resume while dictating (Mac/Win) | 6 | 1,273 | `tauri-app-next/src-tauri/src/`, `tauri-app-next/src-tauri/src/media_control/` |
| `rightkit-crash` | crate | EXTEND | out-of-process minidump writer, user feedback, diagnostics transport | 18 | 3,019 | `tauri-app-next/heardright-dump-helper/`, `tauri-app-next/heardright-dump-helper/src/`, `tauri-app-next/heardright-telemetry/` +5 dirs |
| `rightkit-license` | crate | EXTEND | API gap first: Pro-continuity driver around the Apple iCloud bridge (opt-in state, consume suppression, publish-or-consume la | 1 | 601 | `tauri-app-next/src-tauri/src/` |
| `rightkit-license` | crate | EXTEND | API gap first: update_credentials_from_record_text feeds the plain-header update lane; goes with the move to the owner's lice | 1 | 259 | `tauri-app-next/src-tauri/src/` |
| `rightkit-license` | crate | EXTEND | API gap first: Durable pending-removal drain: rightkit-license 0.2.5 deactivate_with_pending_release (client/mod.rs:199-208)  | 1 | 209 | `tauri-app-next/src-tauri/src/license/` |
| `rightkit-license` | crate | EXTEND | API gap first: Update-check and update-install credentials sent as plain headers by the tauri-plugin-updater lane; the owner' | 1 | 152 | `tauri-app-next/src-tauri/src/license/` |
| `rightkit-license` | crate | EXTEND | API gap first: Account session and device-list commands: rightkit-tauri-shell 0.6.0 (feature license) registers only the five | 1 | 145 | `tauri-app-next/src-tauri/src/` |
| `rightkit-license` | crate | EXTEND | API gap first: Windows link-code request and redeem commands: the shell exposes no link command and @rightkit/license no link | 1 | 89 | `tauri-app-next/src-tauri/src/license/` |
| `rightkit-license` | crate | EXTEND | API gap first: Platform-aware trial request: rightkit-license 0.2.5 LicenseClient::request_trial sends only {app, email} (cli | 1 | 76 | `tauri-app-next/src-tauri/src/license/` |
| `rightkit-models` | crate | EXTEND | model-serve worker | 1 | 291 | `cloudflare-workers/model-serve/` |
| `rightkit-models` | crate | EXTEND | API gap first: Core ML cache pruning (prune_orphaned_ane_temps, clear_owned_cache_root): rightkit-fs 0.4.7 has no recursive r | 1 | 186 | `tauri-app-next/heardright-model-store/src/` |
| `rightkit-overlay` | crate | EXTEND | recording pill overlay (native Win/Mac, dispatch queue) | 48 | 17,672 | `tauri-app-next/heardright_core/src/`, `tauri-app-next/heardright_core/src/pill/`, `tauri-app-next/macos-native/HeardRightPillMac/` +11 dirs |
| `rightkit-qa` | crate | EXTEND | QA commands & cleanup | 4 | 709 | `tauri-app-next/src-tauri/src/` |
| `rightkit-qa` | crate | EXTEND | test-only env switches | 2 | 44 | `tauri-app-next/heardright-dev-env/`, `tauri-app-next/heardright-dev-env/src/` |
| `rightkit-record` | crate | EXTEND | screenshot capture for context | 3 | 439 | `tauri-app-next/src-tauri/src/`, `tauri-app-next/src-tauri/src/shot/` |
| `rightkit-sync` | crate | EXTEND | CloudKit transcript sync scheduler | 5 | 2,081 | `tauri-app-next/src-tauri/src/` |
| `rightkit-tauri-shell` | crate | EXTEND | Tauri app bootstrap, lifecycle router, command surface & engine bridge (app keeps a short builder call) | 21 | 4,520 | `tauri-app-next/src-tauri/src/`, `tauri-app-next/src-tauri/src/commands/`, `tauri-app-next/src-tauri/src/commands/microphone/` |
| `rightkit-tauri-shell` | crate | EXTEND | API gap first: Hand-written WebView2 SetIsVisible, TrySuspend and Resume for the hidden hub (suspend_webview_rendering, resum | 1 | 1,223 | `tauri-app-next/src-tauri/src/` |
| `rightkit-tauri-shell` | crate | EXTEND | build identity, icon embedding, tauri config restore | 1 | 159 | `tauri-app-next/src-tauri/` |
| `rightkit-transcript-library` | crate | EXTEND | transcript history: SQLCipher store, key rotation, retention, recovery, export (was `rightkit-library`, which is the extraction-artifact index; HeardRight's history trait and records are already in `rightkit-transcript-library`) | 16 | 3,676 | `tauri-app-next/heardright_core/src/`, `tauri-app-next/src-tauri/src/`, `tauri-app-next/src-tauri/src/history/` |
| `RightKitAppleCredentials` | Swift | ADOPT | credentials | 1 | 300 | `ios-native/Shared/` |
| `RightKitAudio` | Swift | ADOPT | watch recording meter: `AudioFileRecorder` metering (was `RightKitWatch`) | 1 | 27 | `ios-native/HeardRightWatch/` |
| `RightKitDiagnostics` | Swift | ADOPT | diagnostics | 3 | 463 | `ios-native/Shared/` |
| `RightKitLicense` | Swift | ADOPT | licence & purchases | 6 | 1,456 | `ios-native/HeardRight/`, `ios-native/Shared/` |
| `RightKitSpeech` | Swift | ADOPT | model download/install, CoreML validation | 12 | 3,477 | `ios-native/HeardRight/` |
| `rightkit-asr` | crate | ADOPT | whisper model | 1 | 61 | `tauri-app-next/src-tauri/src/` |
| `rightkit-credentials` | crate | ADOPT | secrets | 2 | 752 | `tauri-app-next/src-tauri/src/` |
| `rightkit-hotkeys` | crate | ADOPT | push-to-talk hotkeys | 3 | 580 | `tauri-app-next/src-tauri/src/` |
| `rightkit-license` | crate | ADOPT | licence, trial, device link, pro bridge | 2 | 309 | `tauri-app-next/src-tauri/src/`, `tauri-app-next/src-tauri/src/license/` |
| `rightkit-models` | crate | ADOPT | model fetch/install/activation | 22 | 5,149 | `ios-native/Packages/SharedModelRoot/`, `ios-native/Packages/SharedModelRoot/Sources/SharedModelRoot/`, `ios-native/Packages/SharedModelRoot/Tests/SharedModelRootTests/` +2 dirs |
| `rightkit-models` | crate | ADOPT | model commands | 1 | 847 | `tauri-app-next/src-tauri/src/commands/` |
| `rightkit-tauri-shell` | crate | ADOPT | updates, autostart, tray, menubar, known folders, atomic replace, helpers | 13 | 4,063 | `tauri-app-next/src-tauri/src/` |
| `rightkit-onnx` | crate | VENDOR-KEEP | ONNX Runtime binary via rightkit-onnx | 24 | 43,900 | `ios-native/Vendor/onnxruntime.xcframework/ios-arm64/onnxruntime.framework/Headers/`, `ios-native/Vendor/onnxruntime.xcframework/ios-arm64_x86_64-simulator/onnxruntime.framework/Headers/` |
| `tauri-runtime-wry` | other | VENDOR-KEEP | patched Tauri runtime fork: upstream the patch, then use crates.io | 17 | 7,057 | `tauri-app-next/vendor/tauri-runtime-wry/`, `tauri-app-next/vendor/tauri-runtime-wry/src/`, `tauri-app-next/vendor/tauri-runtime-wry/src/dialog/` +2 dirs |
| `rightkit-asr` | crate | DROP | vendored copy of rightkit-asr: use published crate | 67 | 21,843 | `tauri-app-next/vendor/rightkit-asr/`, `tauri-app-next/vendor/rightkit-asr/src/`, `tauri-app-next/vendor/rightkit-asr/src/kws/` +5 dirs |
| `rightkit-asr` | crate | DROP | sherpa-onnx: already rewritten in rightkit-asr (Mac & Windows tested); delete once the iOS build is tested | 2 | 9,378 | `ios-native/Vendor/sherpa-onnx.xcframework/ios-arm64/Headers/sherpa-onnx/c-api/`, `ios-native/Vendor/sherpa-onnx.xcframework/ios-arm64_x86_64-simulator/Headers/sherpa-onnx/c-api/` |
| `rightkit-asr` | crate | DROP | prebuilt core xcframework: build from RightKit crates | 2 | 144 | `ios-native/Vendor/heardright-core.xcframework/ios-arm64-simulator/Headers/`, `ios-native/Vendor/heardright-core.xcframework/ios-arm64/Headers/` |
| `app-shell` | other | APPTEST | app journey/e2e tests | 181 | 28,886 | `ios-native/HeardRightSyncTests/`, `ios-native/HeardRightUITests/`, `ios-native/Tests/` +21 dirs |
| `app-shell` | other | APPTEST | app journey/e2e/unit tests of the app binary | 60 | 6,925 | `tauri-app-next/heardright_core/examples/`, `tauri-app-next/heardright_core/src/`, `tauri-app-next/heardright_core/tests/` +2 dirs |
| `app-shell` | other | APPTEST | watch recording controller smoke tests (was `RightKitWatch`) | 1 | 173 | `ios-native/HeardRightWatch/` |
| `app-shell` | other | SHELL | iOS app entry, content/tab views, sheets, menus, settings & account, widget and share-extension entries (was `RightKitUI`, which is a UI kit; the inventory JSON already marks these SHELL) | 23 | 3,903 | `ios-native/HeardRight/`, `ios-native/HeardRightShare/`, `ios-native/HeardRightWatchWidgets/` +1 dirs |
| `app-shell` | other | SHELL | onboarding screens with HeardRight copy (was `RightKitUI`; a shared onboarding scaffold only if a second app needs one) | 5 | 1,563 | `ios-native/HeardRight/` |
| `app-shell` | other | SHELL | watch app entry and the recording `AppIntent` declarations (was `RightKitWatch`) | 3 | 517 | `ios-native/HeardRightWatch/`, `ios-native/Shared/` |
| `app-shell` | other | SHELL | shell entry & commands | 7 | 440 | `tauri-app-next/heardright_core/`, `tauri-app-next/heardright_core/src/bin/`, `tauri-app-next/src-tauri/` +3 dirs |
| `app-shell` | other | SHELL | app data dir name over rightkit-fs | 1 | 142 | `tauri-app-next/heardright_core/src/` |
| `app-shell` | other | SHELL | android app | 4 | 104 | `android/`, `android/app/`, `android/gradle/` |
| `app-shell` | other | SHELL | iOS app entry & views | 2 | 74 | `ios-native/HeardRight/` |
| `app-shell` | other | SHELL | app entry | 1 | 4 | `tauri-app-next/src/` |
| `app-shell` | other | CONFIG | app manifests | 5 | 249 | `./`, `tauri-app-next/` |
| `none` | other | NOTCODE | generated third-party notices (each app ships its own; produced by @rightkit/legal) | 3 | 11,348 | `tauri-app-next/legal/`, `tauri-app-next/legal/third-party/` |
| `none` | other | NOTCODE | documentation pages | 68 | 11,139 | `docs/`, `docs/evidence/2026-10-03-polish-delta/`, `docs/evidence/2026-10-04-local-assistant-reuse/` +21 dirs |
| `none` | other | NOTCODE | datasets & docs | 2 | 1,688 | `data/HR26-EXT02/` |

## What stays in the app (SHELL)

Entry points, command tables & app-specific wiring over RightKit packages. Largest files:

| File | Lines |
|---|---:|
| `tauri-app-next/src-tauri/Cargo.toml` | 279 |
| `tauri-app-next/heardright_core/src/app_paths.rs` | 142 |
| `ios-native/HeardRight/HeardRightCore.h` | 72 |
| `android/app/build.gradle.kts` | 55 |
| `tauri-app-next/heardright_core/Cargo.toml` | 52 |
| `tauri-app-next/heardright_core/src/bin/condition_audio.rs` | 33 |
| `tauri-app-next/src-tauri/assets/dmg-background.html` | 30 |
| `tauri-app-next/src-tauri/scripts/whisper_mel_oracle.py` | 28 |
| `android/gradle/libs.versions.toml` | 19 |
| `android/settings.gradle.kts` | 16 |
| `android/build.gradle.kts` | 14 |
| `tauri-app-next/heardright_core/src/bin/deterministic_polish.rs` | 12 |
| `tauri-app-next/src-tauri/resources/media/now_playing_rate.js` | 6 |
| `tauri-app-next/src/vite-env.d.ts` | 4 |
| `ios-native/HeardRight/HeardRight-Bridging-Header.h` | 2 |

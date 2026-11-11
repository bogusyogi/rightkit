# ScrapeRight: RightKit mapping

Repo `scraperight` at `859298e` (main). Generated 2026-11-01. Every tracked code file (Rust, TS/JS, Swift, Kotlin, Python, shell, CSS/HTML, TOML, SQL) is assigned to exactly one target. Full per-file list: [`scraperight-rightkit-files.json`](scraperight-rightkit-files.json).

Package names are final, after cross-app merging (see master plan in RightKit `docs/audits/2026-11-01-rightkit-package-master-plan.md`).

## Summary

Corrected 2026-11-11 by the shape review (`docs/audits/2026-11-11-shape-matrix/reports/library-and-swift-mapping-redraw.md`): nothing is planned into `RightKitUI` (settings and app entry are app screens); the iOS E2E seeds and tests stay with the app, never `RightKitHubQA`. `rightkit-library` rows unchanged: it is ScrapeRight's index. Totals below are recomputed from the corrected rows; the per-file list in the app repo was not changed here.

| Action | Meaning | Files | Lines |
|---|---|---:|---:|
| NEW | new RightKit package (or new piece of one) | 140 | 46,095 |
| EXTEND | existing RightKit package gains this | 112 | 33,581 |
| ADOPT | RightKit already ships it; delete app copy & use owner | 80 | 11,537 |
| APPTEST | app journey/e2e test: stays with the app (P7, §6), runs on rightkit-qa | 37 | 8,510 |
| DROP | delete | 6 | 2,573 |
| SHELL | thin app composition that stays in app | 13 | 1,089 |
| CONFIG | app manifest/config that stays | 1 | 34 |
| NOTCODE | docs/data, not code | 6 | 16,745 |
| **Total** | | **395** | **120,164** |

Code leaving the app (NEW + EXTEND + ADOPT + LEGION + vendor + DROP): **93,786 lines**. Staying in the app as product code: **0 lines**; as shell/config: **1,123 lines** (1% of code), plus **8,510 lines** of app journey/e2e tests.

## Mapping table

| RightKit target | Kind | Action | What moves | Files | Lines | Where (sample) |
|---|---|---|---|---:|---:|---|
| `@rightkit/config` | npm | NEW | shared toolchain/lint/vite presets | 5 | 171 | `./`, `.cargo/` |
| `@rightkit/library-ui` | npm | NEW | library list, transcript/article reader, find-in-document, workbench | 16 | 4,730 | `src/components/`, `src/lib/`, `src/views/` |
| `@rightkit/library-ui` | npm | NEW | job queue UI: paste bar, drop overlay, job cards, queue board, engine event hook | 19 | 2,156 | `src/components/`, `src/hooks/`, `src/lib/` |
| `@rightkit/library-ui` | npm | NEW | tests | 15 | 1,308 | `tests/` |
| `RightKitLibrary` | Swift | NEW | SwiftUI queue, library, result detail, receive views | 4 | 919 | `ios-native/ScrapeRight/Views/` |
| `RightKitLibrary` | Swift | NEW | Swift artifact library & markdown export | 2 | 153 | `ios-native/ScrapeRight/Library/` |
| `rightkit-bench` | crate | NEW | benchmark corpora runner, scoring, receipts & claim gate | 17 | 15,276 | `benchmarks/article-quality/`, `benchmarks/article-quality/goldens/`, `benchmarks/article-quality/src/` +2 dirs |
| `rightkit-engine-host` | crate | NEW | engine binary framework: sidecar + headless CLI modes, command registration, contract export & TS codegen, human/JSON presentation, engine client | 9 | 4,419 | `crates/engine/src/`, `crates/protocol/src/`, `src/` |
| `rightkit-library` | crate | NEW | local artifact library: history authority, SQLite FTS index, delete/restore | 3 | 3,305 | `crates/engine/src/`, `crates/store/`, `crates/store/src/` |
| `rightkit-library` | crate | NEW | non-LLM enrichment: keywords, links, source tags, text segmentation | 5 | 975 | `crates/engine/src/` |
| `rightkit-media-fetch` | crate | NEW | yt-dlp download/normalize, playlists & channels, X/TikTok/Instagram post, carousel & image capture | 19 | 6,844 | `crates/audio/`, `crates/audio/src/`, `crates/audio/src/tests/` +2 dirs |
| `rightkit-media-fetch` | crate | NEW | URL/file job classifier, media-type table, URL & host helpers | 4 | 1,084 | `crates/detect/`, `crates/detect/src/` |
| `rightkit-media-fetch` | crate | NEW | fetch-pool vs compute-lane split | 1 | 493 | `crates/engine/src/` |
| `rightkit-media-fetch` | crate | NEW | Swift port of detect; replace with UniFFI binding | 1 | 94 | `ios-native/ScrapeRight/Queue/` |
| `rightkit-web-extract` | crate | NEW | article extraction ladder: native fetch w/ TLS fingerprint, browser render, readability, content filters | 7 | 1,837 | `crates/engine/src/`, `crates/engine/src/extraction/` |
| `rightkit-web-extract` | crate | NEW | Swift extractors: article, carousel, media URL, PDF, OCR, WebView render, fetch policy | 7 | 897 | `ios-native/ScrapeRight/Extract/` |
| `rightkit-web-session` | crate | NEW | signed-in web sessions: in-app sign-in webview, cookie import/export, profile dirs | 6 | 1,434 | `src-tauri/src/` |
| `@rightkit/legal` | npm | EXTEND | API gap first: api-gap: the shell legal receipt importer is Rust-side only; the renderer localStorage receipt cannot reach it | 1 | 20 | `src/legal/` |
| `@rightkit/qa` | npm | EXTEND | QA capture, visual diff, evidence, test inventory, isolation | 6 | 1,393 | `scripts/`, `scripts/lib/` |
| `@rightkit/qa` | npm | EXTEND | mock IPC for UI QA | 3 | 968 | `src/`, `src/lib/ipc/`, `tests/helpers/` |
| `@rightkit/react-shell` | npm | EXTEND | desktop chrome css, flash/toast, idle task, request gate, formatting | 10 | 3,595 | `src/`, `src/components/`, `src/lib/` |
| `@rightkit/react-shell` | npm | EXTEND | app frame composition, splash, engine API client (app keeps a short composition) | 3 | 689 | `src/` |
| `@rightkit/react-shell` | npm | EXTEND | shell/theme/platform/update tests | 12 | 560 | `tests/` |
| `@rightkit/release` | npm | EXTEND | bundled runtime staging & artifact verification | 6 | 676 | `scripts/` |
| `@rightkit/release` | npm | EXTEND | release config logic moves into @rightkit/release; app keeps a JSON data config (P8) | 1 | 265 | `./` |
| `@rightkit/settings-ui` | npm | EXTEND | runtime/doctor sections & prefs | 10 | 1,858 | `src/views/`, `src/views/settings/` |
| `@rightkit/theme` | npm | EXTEND | Swift brand tokens generated from brand.json | 3 | 138 | `ios-native/ScrapeRight/Theme/` |
| `RightKitImportQueue` | Swift | EXTEND | universal serial job queue & runner | 4 | 623 | `ios-native/ScrapeRight/Queue/` |
| `rightkit-asr` | crate | EXTEND | API gap first: api-gap: rightkit-transcript 0.4.0 export::stamp is private; see docs/rightkit-exceptions/outside-M-rows.md#it | 1 | 788 | `crates/engine/src/` |
| `rightkit-asr` | crate | EXTEND | API gap first: api-gap: rightkit-speech-client 0.4.0 transcribe_via_service_policy lacks running-host, capability, window-fit | 1 | 391 | `crates/speech/src/` |
| `rightkit-browser` | crate | EXTEND | API gap first: api-gap: rightkit-browser 0.3.6 BrowserPage has no user-agent or extra-headers setter; see docs/rightkit-excep | 1 | 654 | `crates/engine/src/browser/` |
| `rightkit-fs` | crate | EXTEND | API gap first: api-gap: rightkit-fs 0.4.7 AppDirs has no user Downloads folder; see docs/rightkit-exceptions/M8.md#item-1 | 1 | 261 | `crates/protocol/src/` |
| `rightkit-http` | crate | EXTEND | SSRF URL guard for non-rightkit-http transports | 1 | 409 | `crates/engine/src/` |
| `rightkit-job-queue` | crate | EXTEND | durable queue w/ fetch pool + single compute lane, quarantine, event sink | 7 | 5,177 | `crates/engine/src/` |
| `rightkit-license` | crate | EXTEND | API gap first: api-gap: scraperight is not in contracts/right-suite-api.v1.json, so right_suite(app) update auth refuses it;  | 1 | 657 | `src-tauri/src/` |
| `rightkit-license` | crate | EXTEND | Apple-device Pro continuity bridge | 1 | 527 | `src-tauri/src/` |
| `rightkit-logs` | crate | EXTEND | API gap first: api-gap: the shell logs clear covers the renderer log only (no extra sinks) and the shell has no panic hook wi | 1 | 430 | `src-tauri/src/` |
| `rightkit-models` | crate | EXTEND | API gap first: api-gap: rightkit-models 0.5.1 has no bundled-model seed or staging/replaced sweep; see docs/rightkit-exceptio | 1 | 533 | `crates/engine/src/` |
| `rightkit-pdf` | crate | EXTEND | API gap first: api-gap: rightkit-pdf 0.3.3 raster draws image XObjects only (no PDFium renderer, no text layer); see docs/rig | 1 | 129 | `crates/engine/src/extraction/` |
| `rightkit-send` | crate | EXTEND | Swift binding/UI over rightkit-send (receive scratchpad, clipboard offer) | 5 | 891 | `ios-native/ScrapeRight/Send/` |
| `rightkit-sync` | crate | EXTEND | CloudKit transport, phone-to-Mac queue relay, sync settings & runtime | 14 | 6,854 | `crates/engine/src/`, `src-tauri/src/`, `src-tauri/src/cloud_sync/` |
| `rightkit-tauri-shell` | crate | EXTEND | Tauri app bootstrap, lifecycle router, command surface & engine bridge (app keeps a short builder call) | 10 | 2,945 | `src-tauri/src/` |
| `rightkit-tauri-shell` | crate | EXTEND | API gap first: api-gap: rightkit-job-queue 0.5.1 cannot hold the first spawn until a first snapshot exists; see docs/rightkit | 1 | 471 | `src-tauri/src/` |
| `rightkit-tauri-shell` | crate | EXTEND | asset-protocol scope grants | 1 | 141 | `src-tauri/src/` |
| `rightkit-tauri-shell` | crate | EXTEND | build identity, icon embedding, tauri config restore | 1 | 19 | `src-tauri/` |
| `rightkit-transcript` | crate | EXTEND | transcript & article artifact writers, output path naming | 4 | 1,519 | `crates/engine/src/extraction/` |
| `@rightkit/app-update` | npm | ADOPT | release status & licence client requests | 1 | 218 | `src-tauri/src/` |
| `@rightkit/legal` | npm | ADOPT | legal notices | 3 | 240 | `scripts/` |
| `@rightkit/legal` | npm | ADOPT | legal config | 2 | 53 | `src/legal/` |
| `@rightkit/legal-ui` | npm | ADOPT | tests | 1 | 237 | `tests/` |
| `@rightkit/license` | npm | ADOPT | licence client | 2 | 192 | `src/lib/` |
| `@rightkit/media-ui` | npm | ADOPT | media player adapters | 2 | 353 | `src/lib/` |
| `@rightkit/media-ui` | npm | ADOPT | tests | 2 | 345 | `tests/` |
| `@rightkit/onboarding` | npm | ADOPT | models intro & setup gate | 3 | 384 | `src/lib/`, `src/views/onboarding/` |
| `@rightkit/settings-ui` | npm | ADOPT | models section client | 1 | 214 | `src/lib/` |
| `@rightkit/tauri` | npm | ADOPT | adapters over existing RK packages | 5 | 248 | `src/lib/` |
| `@rightkit/theme` | npm | ADOPT | generated brand css | 1 | 28 | `src/generated/` |
| `RightKitAppleCredentials` | Swift | ADOPT | Apple credential CLI helper | 2 | 335 | `macos-native/ScrapeRightAppleCredentials/`, `macos-native/ScrapeRightAppleCredentials/Sources/ScrapeRightAppleCredentials/` |
| `RightKitAppleCredentials` | Swift | ADOPT | synced credential store | 1 | 280 | `ios-native/Shared/` |
| `RightKitCloudSync` | Swift | ADOPT | app sync adapter/codec on RightKitCloudSync | 7 | 1,212 | `ios-native/ScrapeRight/Sync/`, `ios-native/Shared/CloudSync/` |
| `RightKitDiagnostics` | Swift | ADOPT | diagnostics | 1 | 18 | `ios-native/ScrapeRight/` |
| `RightKitImportQueue` | Swift | ADOPT | share extension intake | 2 | 57 | `ios-native/ScrapeRightShare/`, `ios-native/Shared/` |
| `RightKitLicense` | Swift | ADOPT | licence & Pro continuity config | 6 | 753 | `ios-native/ScrapeRight/Pro/`, `ios-native/Shared/` |
| `RightKitSpeech` | Swift | ADOPT | on-device transcription | 2 | 195 | `ios-native/ScrapeRight/Extract/` |
| `RightKitSpeech` | Swift | ADOPT | shared model install store | 1 | 132 | `ios-native/ScrapeRight/Models/` |
| `rightkit-app-settings` | crate | ADOPT | persisted settings reader | 1 | 163 | `crates/protocol/src/` |
| `rightkit-asr` | crate | ADOPT | Parakeet/Whisper routing & engines (use rightkit-asr + rightkit-speech-client) | 7 | 1,249 | `crates/engine/src/`, `crates/speech/`, `crates/speech/src/` |
| `rightkit-browser` | crate | ADOPT | CDP browser backend & address pin | 2 | 278 | `crates/engine/src/browser/` |
| `rightkit-child-process` | crate | ADOPT | helper process spawn/timeout | 1 | 278 | `src-tauri/src/` |
| `rightkit-crash` | crate | ADOPT | renderer crash capture | 1 | 21 | `src-tauri/src/` |
| `rightkit-fs` | crate | ADOPT | engine paths | 3 | 754 | `crates/engine/src/`, `crates/protocol/src/` |
| `rightkit-license` | crate | ADOPT | licence config & headless lookup | 2 | 284 | `crates/engine/src/`, `crates/protocol/src/` |
| `rightkit-logs` | crate | ADOPT | structured logging & support bundles | 2 | 256 | `crates/engine/src/`, `src-tauri/src/` |
| `rightkit-models` | crate | ADOPT | shared model store ops, migration between suite apps, seeding | 7 | 1,305 | `crates/engine/src/`, `crates/protocol/src/`, `src-tauri/src/` |
| `rightkit-ocr` | crate | ADOPT | OS OCR + PP-OCR fallback | 3 | 586 | `crates/engine/src/`, `crates/ocr/`, `crates/ocr/src/` |
| `rightkit-onnx` | crate | ADOPT | ORT location policy & readiness | 3 | 304 | `crates/engine/src/`, `crates/ort_common/`, `crates/ort_common/src/` |
| `rightkit-playback` | crate | ADOPT | compatibility playback | 2 | 276 | `crates/engine/src/`, `src-tauri/src/` |
| `rightkit-tauri-shell` | crate | ADOPT | engine sidecar host | 1 | 289 | `src-tauri/src/` |
| `none` | other | DROP | static design mockups, not shipped code | 6 | 2,573 | `_mockups/scraperight-redesign/`, `_mockups/scraperight-settings/`, `_mockups/scraperight-simple/` |
| `app-shell` | other | APPTEST | app journey/e2e tests | 32 | 7,847 | `crates/scraperight-ui-e2e/`, `crates/scraperight-ui-e2e/src/`, `crates/scraperight-ui-e2e/tests/` +6 dirs |
| `app-shell` | other | APPTEST | app journey/e2e/unit tests of the app binary | 5 | 663 | `crates/engine/examples/`, `crates/engine/src/`, `crates/engine/tests/` |
| `app-shell` | other | SHELL | settings screen (was `RightKitUI`) | 1 | 422 | `ios-native/ScrapeRight/Views/` |
| `app-shell` | other | SHELL | iOS app entry & views (was `RightKitUI`) | 3 | 288 | `ios-native/ScrapeRight/` |
| `app-shell` | other | SHELL | app composition & IPC contract | 6 | 187 | `./`, `src/`, `src/lib/ipc/` |
| `app-shell` | other | SHELL | engine binary, headless CLI & app wire contract | 2 | 112 | `crates/engine/`, `crates/protocol/` |
| `app-shell` | other | SHELL | tauri entry & command surface | 1 | 80 | `src-tauri/` |
| `app-shell` | other | CONFIG | app manifests & release config | 1 | 34 | `./` |
| `none` | other | NOTCODE | generated third-party notices (each app ships its own; produced by @rightkit/legal) | 3 | 15,533 | `legal/`, `legal/third-party/` |
| `none` | other | NOTCODE | documentation pages | 3 | 1,212 | `docs/`, `docs/handoff-2026-10-11/` |

## What stays in the app (SHELL)

Entry points, command tables & app-specific wiring over RightKit packages. Largest files:

| File | Lines |
|---|---:|
| `crates/engine/Cargo.toml` | 86 |
| `src-tauri/Cargo.toml` | 80 |
| `src/lib/ipc/index.ts` | 67 |
| `src/lib/ipc/subscribe.ts` | 51 |
| `src/lib/ipc/streamEvents.ts` | 44 |
| `crates/protocol/Cargo.toml` | 26 |
| `index.html` | 21 |
| `src/lib/ipc/qaGate.ts` | 3 |
| `src/vite-env.d.ts` | 1 |

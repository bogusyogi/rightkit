# Fresh app adoption evidence: CodeRight, HeardRight, ViewRight, MailRight, ScrapeRight

Date: 2026-10-10. Read-only pass. Inputs were each repository's `AGENTS.md`, current app manifests/source, & current RightKit source. Prior audits/reports/plans were ignored. No build, test, network, install, source edit, or commit was run.

## Reading local RightKit status

RightKit README defines package ownership & says consumers use published versions (`README.md:5-27`). Lifecycle/status is source, staged, published, adopted; `packages/release/rightkit-versions.json` is only local recorded status (`README.md:31-46`). Current source versions used for comparison below are direct Cargo manifests: db `0.3.2`, embed `0.3.1`, fs `0.3.3`, http `0.2.4`, logs `0.2.0`, ocr `0.2.0`, ort `0.2.2`, process `0.3.3`, secrets `0.1.2`, shell `0.1.4`, tauri `0.1.2`, watch `0.3.1`, models `0.2.7`, asr `0.0.2`, audio `0.2.0`, browser `0.2.3`, cache `0.1.0`, crash `0.2.0`, mcp `0.5.2`, oauth `0.3.0`, sandbox `0.3.3`, speech-client `0.3.2`, framed-sidecar `0.1.0` (`crates/rightkit-*/Cargo.toml:3`). Current npm source versions are in `INDEX.md:80-95`; local npm adopted/staged records are `packages/release/rightkit-versions.json:4-37`. Swift source is `swift/RightKitSwift/VERSION:1` (`0.3.8`); local release record says adopted `0.1.1`.

“Used” below means source import or manifest-backed runtime use found in this pass. “Candidate review” means no direct use was found; it is not an automatic removal instruction. “Keep” means product policy, schema, UI, orchestration, or platform glue remains app-owned even where it wraps RightKit.

Evidence path roots are absolute: CodeRight `/Volumes/D/claude/coderight`, HeardRight `/Volumes/D/claude/heardright`, ViewRight `/Volumes/D/claude/viewright`, MailRight `/Volumes/D/claude/mailright`, ScrapeRight `/Volumes/D/claude/scraperight`, RightKit `/Volumes/D/claude/rightkit`. Tables abbreviate these fixed roots; every `path:line` reference expands directly to its app root.

Exact raw declaration inventory is retained in parent evidence `app-rightkit-dependencies.csv`; tables here show short versions for readable source mapping. This report records source use separately from inferred adoption candidates.

## CodeRight

Root for every path in this section: `/Volumes/D/claude/coderight`.

### Actual declarations & observed use

| Target | Exact declaration | Observed use |
|---|---|---|
| Desktop/web | `apps/coderight-tauri/package.json:127-132,145`: npm `legal 0.3.1`, `license 0.1.8`, `logs 0.1.4`, `shell 0.1.0`, `tauri 0.1.1`, `updates 0.2.5`; dev `release 0.2.133` | `src/state/updateLicense.ts:6,15-16`; `src/lib/logs.ts:1`; `src/lib/notifications.ts:12`; `src/state/updateLicense.ts:6,15-16`; `components/updater/UpdateOverlay.tsx:2`; scripts use legal/release. Rust mirrors license/logs/shell/tauri/ort in `apps/coderight-tauri/src-tauri/src/{license.rs,logs.rs,notifications.rs,desktop_shell.rs,lib.rs}:1-1202`. |
| Desktop Rust | `apps/coderight-tauri/src-tauri/Cargo.toml:48-52`: license `0.1.3`; logs `0.2.0`, `default-features=false`; tauri `0.1.1`; shell `0.1.2`, `default-features=false`, `wv2-0_38`; ort `0.2.0`, `default-features=false` | `src-tauri/src/logs.rs:1,88-102`; notifications/desktop shell; `src-tauri/src/lib.rs:1191-1202` ort. `src-tauri/crates/auth/Cargo.toml:1+` also declares tauri `0.1.1` and is consumed by auth target. |
| Engine crates | `engine/Cargo.toml:148-159`: watch `0.1.0`, fs `0.1.0`, oauth `0.2.0`, http `0.1.0`, mcp `0.5.1`, secrets `0.1.0`, cache `0.1.0`, ort `0.2.0`/no defaults, db `0.2.1`, browser `0.2.0`; `engine/crates/media/Cargo.toml`: ocr `0.1.2`/no defaults/`native`; support bundle logs `0.2.0`; tools sandbox `0.2.4`, process `0.3.2` | Storage uses db/fs (`engine/crates/storage/src/lib.rs:129-149,181-191`); migrations `:36,1562`; mcp `engine/crates/mcp/src/manager.rs:9`; auth fs `engine/crates/tools/src/mcp_auth.rs:5`; sandbox/process tool modules; watch `engine/bins/coderight/src/api_fs_watch.rs:40`; runtime fs `:14,36,153,1471,1491-1500`; ort `engine/crates/ort_common/src/lib.rs:5`; ocr `engine/crates/media/src/lib.rs:581-605`. |
| Nested context | `context/package.json:47-53`: `@rightkit/hooks 0.1.1`; dev `ax 0.2.4`, `git 0.2.24`, `release 0.2.121`. Nested Cargo manifests declare db `0.3.0`, embed `0.2.2`, ort `0.2.0`, watch `0.3.0`, mcp `0.5.1`, process `0.3.2` | Context source search found no direct `@rightkit/hooks` import; package scripts/fixtures need review before removal. Nested Rust consumers are under `context/` and are separate from desktop root. `coderight-web/package.json` has no RightKit declaration; no RightKit source import found. |
| iOS | Generator input `apps/coderight-ios/project.yml:23-28,123-126`: RightKitSwift URL `from: 0.1.0`; products License & Diagnostics. Generated `CodeRight.xcodeproj/project.pbxproj:2368-2375` pins/resolves exact `0.1.1` | Imports: `CodeRight/License/LicenseStore.swift:3`, `LicenseView.swift:2`, `Tests/LicenseTests.swift:1`. Generated project is current exact-resolution evidence; project.yml remains generator drift and must be reconciled before changing source. |

### Candidate review & local duplication

`context/package.json:@rightkit/hooks` is only candidate review: no direct source import was found. Rust modules named `process_launch.rs`, `process_resource_authority.rs`, `process_tree.rs`, `browser.rs`, `browser_control.rs`, `browser_fetch.rs`, `sandbox.rs` are active CodeRight policy/control layers despite RightKit process/browser/sandbox declarations; retain until behavior parity is proved. `src-tauri/src/notifications.rs:64` explicitly preserves non-Windows delivery while shared shell coverage is incomplete.

Local wrappers map to existing APIs: `engine/crates/storage/src/lib.rs` → `rightkit-db` + `rightkit-fs`; `engine/bins/coderight/src/api_fs_watch.rs` → `rightkit-watch`; `engine/crates/media/src/lib.rs` → `rightkit-ocr`; `src-tauri/src/logs.rs` → `rightkit-logs`; license/update/auth files → `rightkit-license`, `rightkit-tauri`, `rightkit-updates`; notification/desktop shell → `rightkit-shell`. Keep app-level execution/context/semantic ownership, auth/session mapping, mobile editor/UI, GitHub/Cloudflare connectors, & notification fallbacks.

### Version/prerequisite gaps

Npm app pins trail local staged/source records: tauri `0.1.1` vs source `0.2.0` staged record; release `0.2.133` vs staged `0.2.147`; other npm pins are listed above and must be checked against staged records before handoff. Cargo pins trail current source for watch (`0.1.0` vs `0.3.1`), fs (`0.1.0` vs `0.3.3`), http (`0.1.0` vs `0.2.4`), oauth (`0.2.0` vs `0.3.0`), mcp (`0.5.1` vs `0.5.2`), secrets (`0.1.0` vs `0.1.2`), db (`0.2.1` vs `0.3.2`), browser (`0.2.0` vs `0.2.3`), ort (`0.2.0` vs `0.2.2`), sandbox (`0.2.4` vs `0.3.3`), process (`0.3.2` vs `0.3.3`), & ocr (`0.1.2` vs direct source `0.2.0`; generated INDEX records `0.1.2`, so release metadata needs reconciliation). iOS has generator/generated drift: project.yml lower-bound `from: 0.1.0`, generated project exact `0.1.1` (`CodeRight.xcodeproj/project.pbxproj:2368-2375`), source `0.3.8` & local release adopted baseline `0.1.1`; reconcile lifecycle state before selecting a newer tag.

### Handoff checklist

- Replace each Cargo/npm pin only after corresponding package publication & manifest/hash record; resolve ocr source-vs-INDEX discrepancy first.
- Convert `apps/coderight-ios/project.yml:23-28` to exact RightKitSwift tag; verify License/Diagnostics imports compile in every iOS target.
- Review `context/package.json` hooks usage in scripts/fixtures; remove only if no script/runtime edge remains.
- Acceptance: clean standalone install resolves no path/Git/workspace specs; license/update/logs/shell flows work; auth/mcp/fs-watch/storage/ocr/ort integration passes app QA; iOS license UI/tests pass; Windows notification fallback remains intact.

### Remaining local-functionality candidates (evidence-based)

| Local body | Matching RightKit capability | Disposition & acceptance gate |
|---|---|---|
| `apps/coderight-tauri/src/components/app/AppFrame.tsx:23-49,74-126,156-180`; `Sidebar.tsx:40-80,85-180`; `TitleBar.tsx:79-110`; `CommandPalette.tsx` | `@rightkit/app-shell`, `@rightkit/platform-ui` | Candidate after API fit. Shared frame/palette/window controls could replace generic shell; keep CodeRight session tree, provider panes, command actions & keyboard policy. Acceptance: palette focus/shortcut behavior, deep links, multi-window/session restore match. |
| `apps/coderight-tauri/src-tauri/src/os_shell/tray.rs:11-18,121-180`; `autostart.rs:21-119`; `window_state/mod.rs:16-24,110-180` | `rightkit-shell` | Candidate after parity. Shared tray/autostart/window primitives fit; keep app menu snapshot, platform fallbacks & persistence policy. Acceptance: tray actions, startup toggle, geometry bounds, crash/restart restore on macOS/Windows/Linux. |
| `engine/crates/tools/src/process_launch.rs:1-40`; `process_tree.rs:1-40`; `process_resource_authority.rs:1-40` | `rightkit-process` | Candidate after API gap. Shared ownership/termination can replace mechanics; keep CodeRight command allowlist, resource authority, daemon protocol & cancellation policy. Acceptance: descendant cleanup, timeout/restart, inherited handles & error mapping. |
| `engine/bins/coderight/src/api_fs_watch.rs:40`; `engine/bins/coderight/src/github_app/file_store.rs:32-49,66-94`; `apps/coderight-tauri/src/lib/transcriptSearch.ts` | `rightkit-fs`, `rightkit-watch`, `rightkit-search` | Existing fs/watch adoption is source-backed. Search is only candidate where backend has FTS/query contract; transcript/session fields & remote engine routing stay app-owned. Acceptance: same query semantics, ranking, lock ownership & watch debounce. |
| `apps/coderight-tauri/src/components/settings/SettingsModal.tsx`; `src-tauri/src/consent_dialog.rs` | `@rightkit/settings-ui`, `rightkit-settings` | Candidate after adapter fit. Generic sections/storage can move; consent, provider/API-key policy & dangerous-action confirmation stay local. Acceptance: migration/corruption recovery, secret references, accessibility & section search. |

These are local bodies with concrete shared matches, not package requirements; no migration is justified by declaration absence alone.

## HeardRight

Root for every path in this section: `/Volumes/D/claude/heardright`.

### Actual declarations & observed use

| Target | Exact declaration | Observed use |
|---|---|---|
| Desktop/web | `tauri-app-next/package.json:157-166`: app-shell `0.1.0`, legal `0.3.1`, legal-ui `0.1.3`, license `0.1.8`, logs `0.1.4`, onboarding `0.1.0`, settings-ui `0.1.0`, tauri `0.1.1`, theme `0.1.0`, updates `0.2.5`; dev qa-chrome `0.1.3`, release `0.2.135` (`:179-180`) | Imports/use: app-shell in Sidebar/PageShell/TitleBar/App; onboarding CSS + legal-ui `src/pages/onboarding/OnboardingImpl.tsx:12,14`; settings-ui `src/pages/Settings.tsx:9`, `src/lib/rightkitSettingsBackend.ts:1`; theme `src/lib/theme.ts:1`; logs `src/lib/logs.ts:1`; updates `src/lib/autoUpdates.ts:1`; tauri `src/lib/license/core.ts:12`; scripts use legal/release/qa-chrome. |
| Desktop Rust | `tauri-app-next/src-tauri/Cargo.toml:37,39,41,74`: license `0.1.3`, logs `0.2.0`, tauri `0.1.1`, audio `0.2.0`, no defaults, feature `capture` | license/logs/tauri; onboarding mic/devices `src-tauri/src/onboarding_mic.rs:52,79-160`; audio runtime. |
| Workspace Rust | `heardright-app-runtime/Cargo.toml:15` process `0.3.2`; `heardright-engine/Cargo.toml:43,45` speech-client `0.3.0`, audio `0.2.0`/`capture`; `heardright-model-store/Cargo.toml:11` models `0.2.6`; `heardright-parakeet-core/Cargo.toml:12` asr `0.0.2`/no defaults; `heardright-runtime/Cargo.toml:13-16` speech-client `0.3.0`, service `0.1.0`, audio `0.2.0`/`capture`; telemetry `Cargo.toml:12-13` crash `0.2.0`, logs `0.2.0`; `heardright_core/Cargo.toml:23` speech-client `0.3.0` | ASR `heardright-parakeet-core/src/lib.rs:22`; models/hash `heardright-model-store/src/model_fetch.rs:177`; speech-client in core/runtime; service atomic files `heardright-runtime/src/speech_service/files.rs:19`; process lifecycle `heardright-app-runtime/src/engine_supervisor/lifecycle.rs:43`; telemetry logs/JSONL/crash `heardright-telemetry/src/lib.rs:5`, `crash_dump.rs:39,55`. |
| Apple/native | `ios-native/project.yml:16-19` RightKitSwift exact `0.3.2`; first app products `:131-154`: UI, Purchases, CloudSync, Store, ImportQueue, LiveActivity, WatchBridge, Speech, License, Intents, AppleCredentials, Overlay; more target declarations `:199-212,256-261,292-295,330-333` | Actual imports cover UI (`RecordingPillView.swift`, `ContentViewHomeControls.swift`), Purchases (`StoreKitProController.swift`), CloudSync (`TranscriptCloudSync.swift`), Store (`RecordingOwnershipLease.swift`, transcript stores), ImportQueue (share extension), LiveActivity, WatchBridge, Speech (recording/ASR files), License, Intents, AppleCredentials. No `RightKitOverlay` import found; declaration is candidate review. |

### Candidate review & local duplication

`ios-native/project.yml:154` declares `RightKitOverlay`; source import search found no `RightKitOverlay`, so confirm target/linker use before retaining. Rust `condition_for_asr`, Zephyr wake logic, one detok path, model activation transaction, audio route/calibration, hotkeys/tray, Tauri/native recording UI, Watch/Live Activity/Intent orchestration remain HeardRight-owned. `heardright-runtime`/`heardright-engine` own ASR/provider policy over shared speech-client/audio; `heardright-model-store` owns product model policy over shared models; telemetry owns product event schema over shared logs/crash.

### Version/prerequisite gaps

Web pins are behind current staged npm records: app-shell `0.1.0` vs `0.3.0`, legal-ui `0.1.3` vs `0.2.0`, license `0.1.8` current, logs `0.1.4` current, onboarding `0.1.0` vs `0.3.0`, settings-ui `0.1.0` vs `0.3.0`, tauri `0.1.1` vs `0.2.0`, theme `0.1.0` vs `0.2.1`, release `0.2.135` vs `0.2.147`; legal/updates/qa-chrome match locally staged records. Cargo speech-client `0.3.0` vs source `0.3.2`, process `0.3.2` vs `0.3.3`, models `0.2.6` vs `0.2.7`, service `0.1.0` vs `0.1.3`; audio/asr/crash/logs match source. Swift `0.3.2` trails local source `0.3.8`; publication/status must be verified before changing pin.

### Handoff checklist

- Review Overlay declaration against target linker/source usage; remove only after target-level check.
- Publish/record compatible app-shell, legal-ui, onboarding, settings-ui, tauri, theme, speech-client, process, models, service, & Swift versions.
- Keep ASR/Whisper policy, Zephyr/`condition_for_asr`, detok, audio routing, model transactions, hotkeys/tray, recording UI, Watch/Live Activity/Intents.
- Acceptance: onboarding mic capture, warm capture, detok, ASR model activation, update/license/settings/logging, crash JSONL, Watch/Live Activity/Intent/share extension, & exact Swift package resolution pass app QA.

### Remaining local-functionality candidates (evidence-based)

| Local body | Matching RightKit capability | Disposition & acceptance gate |
|---|---|---|
| `tauri-app-next/src-tauri/src/hotkey.rs:4-60,103-193`; `src-tauri/src/fn_hotkey/tap.rs:356-381` | `rightkit-hotkeys` | Candidate after API gap. Shared registration/Fn tap mechanics may replace native plumbing; keep PTT hold/toggle semantics, 300 ms debounce, rollback, accessibility permission flow & event-to-ASR policy. Acceptance: Fn+Space/chord behavior, no duplicate registration, macOS/Windows parity. |
| `tauri-app-next/src-tauri/src/pill.rs:1-8`; `src-tauri/src/pill/state.rs:36-66,134-160,317-330`; `src/pill/PillOverlay.tsx` | `rightkit-overlay` | Candidate for geometry/state primitives only. RightKit overlay does not replace HeardRight native window creation, click-through, rendering or `pill://event` bridge. Acceptance: topmost/hover/visibility, monitor changes & accessibility behavior remain identical. |
| `tauri-app-next/src-tauri/src/settings.rs:20-130`; `src/pages/Settings.tsx:8-16,23-75`; `src/lib/rightkitSettingsBackend.ts:1` | `rightkit-settings`, `@rightkit/settings-ui` | UI is already adopted; persistence mechanics are candidate after adapter fit. Shared migrations/atomic writes/corruption recovery can replace generic file code, while allowlisted `SettingsBlob`, runtime sidecar sync, secrets & onboarding policy stay local. Acceptance: corrupt-file backup, migration, concurrent pill/engine reads & settings search. |
| `tauri-app-next/src-tauri/src/hotkey_setup.rs`; tray setup named in `src-tauri/Cargo.toml:110-121`; generated tray assets `src-tauri/scripts/gen_tray_icons.py:1-17` | `rightkit-shell` | Candidate after native-menu parity. Shared tray/menu/autostart/window primitives may replace generic shell; keep four-state recording icons, helper identity/heartbeat, click actions & platform policy. Acceptance: menu-bar/tray lifecycle, icon state, singleton behavior & restart recovery. |

Local ASR/provider choice, Zephyr wake, detok, model activation, audio route/calibration, Watch/Live Activity/Intent orchestration remain product capabilities even where shared speech/audio primitives match.

## ViewRight

Root for every path in this section: `/Volumes/D/claude/viewright`.

### Actual declarations & observed use

| Target | Exact declaration | Observed use |
|---|---|---|
| Desktop/web | `package.json:97-104`: doc-render `0.1.3`, legal-ui `0.1.3`, license `0.1.8`, logs `0.1.4`, media-ui `0.2.6`, pdf-ui `0.1.2`, tauri `0.1.1`, updates `0.2.5`; dev legal `0.3.1`, qa-chrome `0.1.2`, release `0.2.135` (`:148`) | doc-render in `src/documents/markdown/*`; media-ui `src/documents/ImageDocument.tsx:1,4,8-9`; pdf-ui in `src/documents/pdf/*`; legal-ui `src/app/AppRoot.tsx:2`; tauri `src/state/tauriBridge.ts:9`; logs `src/state/diagnostics.ts:5`; license/updates `src/state/updateLicense.ts:15,24`; scripts use legal/qa/release. |
| Desktop Rust | `src-tauri/Cargo.toml:58,60,62-65,68,70,73`: db `0.2.1`/`bundled`; watch `0.3.1`; shell `0.1.3`/no defaults/`wv2-0_38`; logs `0.1.0`; license `0.1.3`; tauri `0.1.1`; secrets `0.1.1`; http `0.2.1`; ocr `0.1.2`/no defaults/`native` | db/logs `src-tauri/src/logs.rs:1-2,183-193`; watch `watcher.rs:8-9`; shell `lib.rs:228`; tauri/license/secrets/http `commands/license.rs:11-13,380,478-486,764-804`; ocr `ocr_backend.rs:1,8,14-49` plus worker/commands. |

### Candidate review & local duplication

All declared ViewRight npm/Cargo packages had direct source or script use in this pass. Local wrappers map to matching APIs: `src-tauri/src/persistence.rs:5` app schema/transactions over rightkit-db; `logs.rs` IPC/event adapter over rightkit-logs; `watcher.rs` debounce/filter policy over rightkit-watch; `commands/license.rs` app entitlement endpoints over rightkit-license/tauri/secrets/http; `ocr_backend.rs`, `commands/ocr.rs`, `ocr_worker.rs` app payload/isolation/timeout around rightkit-ocr. Keep vendor/docx-editor fidelity, markdown semantics, PDF engine/AppKit/native behavior, encoding/chardetng, file associations, licensing adapter, & product UI.

### Version/prerequisite gaps

Npm pins behind local staged records: doc-render `0.1.3` vs `0.1.4`, legal-ui `0.1.3` vs `0.2.0`, media-ui `0.2.6` vs `0.3.0`, tauri `0.1.1` vs `0.2.0`, qa-chrome `0.1.2` vs `0.1.3`, release `0.2.135` vs `0.2.147`; license/logs/pdf-ui/updates/legal match records. Cargo pins behind current source: db `0.2.1` vs `0.3.2`, shell `0.1.3` vs `0.1.4`, logs `0.1.0` vs `0.2.0`, secrets `0.1.1` vs `0.1.2`, http `0.2.1` vs `0.2.4`, ocr `0.1.2` vs direct manifest `0.2.0`; generated INDEX still records ocr `0.1.2`, requiring release metadata reconciliation. watch/license match; tauri `0.1.1` trails direct source `0.1.2`.

### Handoff checklist

- Reconcile ocr source manifest vs generated INDEX before selecting pin.
- Publish/record doc-render, legal-ui, media-ui, tauri, qa-chrome, release, db, shell, logs, secrets, http, ocr, & tauri updates.
- Preserve docx editor, markdown/PDF fidelity, native PDF, schema/transactions, watcher filters, OCR isolation, file associations, & license adapter.
- Acceptance: markdown/image/PDF/docx open/render/edit/export, watcher recovery, OCR native path, license/update/logging, SQL/schema migration, Windows shell, & standalone install resolve published specs.

### Remaining local-functionality candidates (evidence-based)

| Local body | Matching RightKit capability | Disposition & acceptance gate |
|---|---|---|
| `src-tauri/crates/pdf/src/inspect.rs:1-31,53-119`; `compose.rs:1-29,102-125,631`; `save.rs:401-430`; `src-tauri/src/commands/pdf_adapter.rs:103-180,204,337` | `rightkit-pdf` | Strong migration candidate after API parity. Local engine already performs inspect/compose/staged atomic save, but adapter carries ViewRight source graph, font/layout, SHA and Tauri types; RightKit must expose equivalent contracts first. Acceptance: page tree inheritance, text replacement/addition, OCR correction, font subset, source identity recheck, staged save/commit & failure recovery. |
| `src/documents/pdf/PdfViewer.tsx:886-1163`; `PdfInlineTextEditor.tsx`; `pdfSave.ts:110-250`; `pdfBenchTransport.ts:68-85` | `@rightkit/pdf-ui`, `rightkit-pdf` | Keep ViewRight editor/render/probe orchestration. `pdf-ui` is read-only viewer, `rightkit-pdf` has no rendering/layout engine; migrate only shared viewer/probe helpers when edit-aware API exists. Acceptance: pdf.js worker/range probe, text overlay alignment, undo/redo, OCR and render-before-commit remain unchanged. |
| `src-tauri/src/site_root.rs:15,81-92,260-270,378`; `src/documents/html/htmlInline.ts:279-340`; CSP media rules `src/documents/html/HtmlViewer.tsx:249` | `rightkit-playback`, `rightkit-media` | No local FFmpeg/playback/probe implementation was found. These files stream HTML audio/video & MIME assets through ViewRight's origin/CSP; RightKit FFmpeg playback/media crates are not automatic requirements. Revisit only if app adds verified FFmpeg conversion or native playback. |
| `src/documents/ImageDocument.tsx:1-19`; `src/app/useAutoSave.ts:3`; `src/styles/index.css:908` | `@rightkit/media-ui` | Already adopted image viewer/editor; retain ViewRight save adapter and visual tokens. Acceptance: image edit snapshot/autosave, format limits & path broker behavior. |

The PDF crate is duplicated mechanism with a clear shared match, but app fidelity/source graph constraints make “after API gap” the safe disposition; no package addition follows from media MIME handling alone.

## MailRight

Root for every path in this section: `/Volumes/D/claude/mailright`.

### Actual declarations & observed use

| Target | Exact declaration | Observed use |
|---|---|---|
| Desktop/web | `package.json:67-70`: license `0.1.8`, logs `0.1.4`, tauri `0.1.1`, updates `0.2.5`; dev legal `0.3.1`, release `0.2.135` (`:93-94`) | license/updates `src/lib/license.ts:29-36`, `src/lib/updateAuth.ts:1-2`; logs `src/lib/telemetry.ts:22`; tauri `src/lib/ipc/secure.ts:1`; scripts legal/release. |
| Desktop Rust | `src-tauri/Cargo.toml:58,88-90,104,106,108,110-113`: db `0.2.1`/no defaults/`sqlcipher`; license `0.1.3`; logs `0.2.0`; tauri `0.1.1`; embed `0.2.1`; cache `0.1.0`; secrets `0.1.0`; oauth `0.2.0`/no defaults; http `0.1.0`; optional shell `0.1.2`/no defaults/`wv2-0_38` | db `src-tauri/src/db.rs:28`; embed `src-tauri/src/db/semantic/embedder.rs:14`; cache `attach_cache.rs:16`; secrets `credential_store.rs:11,155-157`; oauth/http `oauth.rs:6-7`, `google.rs:25-27`; logs `logs.rs:1`; shell desktop/tauri-run `desktop.rs:23,210-322`, `tauri_run.rs:17-30,295-307`; license/tauri `commands/license.rs:40-235`; web imports above. |

### Candidate review & local duplication

No declared MailRight package lacked direct source/script use in this pass. `src-tauri/src/db.rs` owns Gmail schema/query/SQLCipher transactions over rightkit-db; `db/semantic/*` owns sqlite-vec semantic index over rightkit-embed; `credential_store.rs` owns MailRight key namespace over rightkit-secrets; `oauth.rs`/`google.rs` own Gmail scopes/PKCE/endpoints over rightkit-oauth/http; logs/license/tauri files adapt IPC and entitlement records. Keep Gmail MIME/image sanitizer, remote-image protocol, composer/Tiptap, Gmail labels/search, semantic indexing, SQLCipher key/WAL invariant, & desktop shell.

### Version/prerequisite gaps

Npm license/logs/updates/legal match local staged/source records; tauri `0.1.1` trails staged `0.2.0`; release `0.2.135` trails staged `0.2.147`. Cargo db `0.2.1` vs `0.3.2`, tauri `0.1.1` vs `0.1.2`, embed `0.2.1` vs `0.3.1`, secrets `0.1.0` vs `0.1.2`, oauth `0.2.0` vs `0.3.0`, http `0.1.0` vs `0.2.4`, shell `0.1.2` vs `0.1.4`; cache/logs/license match.

### Handoff checklist

- Publish/record tauri, release, db, embed, secrets, oauth, http, shell versions; preserve SQLCipher feature & shell optionality.
- Exercise Gmail OAuth/scopes/PKCE, credential namespace, SQLCipher open/WAL checkpoint, label transaction, cache, semantic search, MIME/image sandbox, composer, update/license/logging.
- Acceptance: standalone install resolves published specs; no credential/token leaks through logs; Gmail sync/retry/offline behavior and Windows/macOS shell pass app QA.

### Remaining local-functionality candidates (evidence-based)

| Local body | Matching RightKit capability | Disposition & acceptance gate |
|---|---|---|
| `src-tauri/src/commands/settings.rs:1-14,24-38,40-70`; `src/views/SettingsView.tsx:1-18,23-120` | `rightkit-settings`, `@rightkit/settings-ui` | Candidate after adapter fit. Shared versioned persistence, migrations, atomic writes & standard frame can reduce generic code; keep account/multi-mailbox sections, privacy-redacted diagnostics, Gmail policy & secret references. Acceptance: migrations/corruption recovery, redaction, section search & key rotation. |
| `src-tauri/src/db/search.rs:1-16,21-38,39-70` | `rightkit-search` | Candidate after schema mapping. Parser/FTS predicate/snippet primitives match; retain Gmail operators, account/label/category scope, candidate planner & ranking. Acceptance: identical Gmail query semantics, FTS planner, snippets/highlights & SQLCipher behavior. |
| `src-tauri/src/attachments.rs:1-27,53-92,96-180`; `src-tauri/src/gmail/mime.rs:1-46,48-115`; `src-tauri/src/webimg.rs:1-16,27-32,54-64,80-178` | `rightkit-http` (bounded fetch only); no generic attachment/MIME owner | Keep app-specific attachment tree, Gmail MIME/RFC composition, CID/remote-image policy, cache keying & tracker/size gates. A bounded shared HTTP adapter is a candidate after credential/cancellation/error mapping; it does not replace Gmail policy. Acceptance: MIME round-trip, hostile URL blocking, byte caps, cache isolation & redaction. |
| `src/components/AttachmentPdfPreview.tsx:1-24,26-98,100-164`; `AttachmentPreviewModal.tsx:48-99,142-180`; `views/ReaderMessage.tsx:55-77` | `@rightkit/pdf-ui`, `rightkit-pdf` | Candidate only for standard PDF viewer pieces. Local password flow, 30-page cap, spreadsheet/text/image preview and download-only safety remain MailRight-owned; `rightkit-pdf` is not a renderer. Acceptance: encrypted PDF unlock, bounded rendering, unsafe spreadsheet handling & attachment identity. |
| `src/lib/html.ts:3-16,40-69`; `src/views/ReaderMessage.tsx:17,91,204`; `src-tauri/src/mail_settings.rs:1-15,21-125` | `@rightkit/doc-render` | Keep email HTML sanitization/CID rewrite/sandbox & Gmail vacation/forwarding endpoints. `doc-render` sanitizes Markdown/HTML for reader documents, but cannot replace MailRight's untrusted email iframe policy or Gmail settings API. Acceptance: no script/event handler escape, CID/remote image behavior & compose/read parity. |

No local MailRight body proves a generic media-player requirement; `rightkit-media` would apply only if attachment playback/conversion is introduced.

## ScrapeRight

Root for every path in this section: `/Volumes/D/claude/scraperight`.

### Actual declarations & observed use

| Target | Exact declaration | Observed use |
|---|---|---|
| Desktop/web | `package.json:78-88`: app-shell `0.2.0`, legal-ui `0.1.3`, license `0.1.6`, logs `0.1.3`, media-ui `0.2.5`, onboarding `0.2.2`, platform-ui `0.1.2`, settings-ui `0.2.7`, tauri `0.1.0`, theme `0.2.0`, updates `0.2.5`; dev legal `0.3.0`, qa-chrome `0.1.0`, release `0.2.133` (`:101-103`) | app-shell/media-ui `src/main.tsx:4,6,12`, `src/lib/theme.ts:1`, `views/DetailReader.tsx:2`; settings-ui `views/Settings.tsx:1,9`, `lib/settingsBackend.ts`; tauri `lib/license.ts`; license/updates `lib/updateAuth.ts`; logs `lib/logs.ts`; onboarding `SetupGate`; scripts legal/release/qa. `platform-ui`, `legal-ui`, `theme` have no direct package import in `src`/`scripts`; `src/generated/brand.css:1` records theme generator provenance. |
| Desktop Rust | `src-tauri/Cargo.toml:27,55,59-62`: framed-sidecar `0.1.0`, fs `0.3.0`, ort `0.2.0`/no defaults, logs `0.1.0`, license `0.1.3`, tauri `0.1.1`, secrets `0.1.0`; workspace engine `Cargo.toml:17-19,57` same framed/fs/ort/secrets; model-store models `0.2.6`; ocr ocr `0.1.2`/`native`, ort `0.2.0`/`session`; ort_common/speech ort `0.2.0`; protocol license optional; store db `0.2.1` | sidecar `crates/engine/src/main_stdio.rs:2`, `src-tauri/src/engine_supervisor.rs:48`; fs/license/secrets/tauri `src-tauri/src/license.rs:60-71,144-193,242-287,314-352,442-521`; logs `src-tauri/src/diagnostics.rs:1,119-145`; db `crates/store/src/lib.rs:38-39`; models `crates/model-store/src/discover_compat.rs:5-6`; ocr/ort `crates/ocr/src/lib.rs:48-57,93-97`, ort_common; main ort `src-tauri/src/main.rs:195-196`. |
| Apple/native | `ios-native/project.yml:19-22` RightKitSwift exact `0.3.7`; products `:97-112`: AppleCredentials, Artifacts, Diagnostics, ImportQueue, Purchases, CloudSync, Store, License; share extension `:124-127` AppleCredentials/ImportQueue. `macos-native/ScrapeRightAppleCredentials/Package.swift:11,17`: exact `0.3.7`, AppleCredentials | iOS imports Diagnostics, ImportQueue, Artifacts, License, Purchases, CloudSync, AppleCredentials in `ScrapeRight/` & share sources; macOS `main.swift:77` AppleCredentials. Every declared native product had observed source use. |

### Candidate review & local duplication

`@rightkit/platform-ui`, `@rightkit/legal-ui`, & `@rightkit/theme` have no direct source import found, but this is not stale-dependency evidence: `packages/app-shell/package.json:33-35` declares platform-ui as app-shell peer; `packages/onboarding/package.json:26-30` declares legal-ui as onboarding peer; `packages/settings-ui/package.json:27-34` declares theme as settings-ui peer; `src/generated/brand.css:1` records theme-generator provenance. Verify peer resolution & generation scripts before any removal. Rust `src-tauri/src/engine_supervisor.rs` owns sidecar lifecycle/restart/queue around framed-sidecar; `src-tauri/src/logging.rs:29-99` owns tracing-appender setup around rightkit-logs; license owns app entitlement bridge; model-store compatibility wrappers are explicit (`crates/model-store/src/lib.rs:38-49`); OCR owns ONNX/fallback routing around rightkit-ocr; speech/audio own Parakeet/Whisper/ytdlp modalities. Keep cloud-sync schema/iOS adapters, installer/update policy, model compatibility, extraction modalities, OCR fallback, & platform-specific key/entitlement glue.

### Version/prerequisite gaps

All npm pins except updates trail staged records: app-shell `0.2.0` vs `0.3.0`, legal-ui `0.1.3` vs `0.2.0`, license `0.1.6` vs `0.1.8`, logs `0.1.3` vs `0.1.4`, media-ui `0.2.5` vs `0.3.0`, onboarding `0.2.2` vs `0.3.0`, platform-ui `0.1.2` vs `0.2.0`, settings-ui `0.2.7` vs `0.3.0`, tauri `0.1.0` vs `0.2.0`, theme `0.2.0` vs `0.2.1`, legal `0.3.0` vs `0.3.1`, qa-chrome `0.1.0` vs `0.1.3`, release `0.2.133` vs `0.2.147`. Cargo fs `0.3.0` vs `0.3.3`, ort `0.2.0` vs `0.2.2`, logs `0.1.0` vs `0.2.0`, tauri `0.1.1` vs `0.1.2`, secrets `0.1.0` vs `0.1.2`, models `0.2.6` vs `0.2.7`, db `0.2.1` vs `0.3.2`; ocr app pin `0.1.2` conflicts with direct source `0.2.0` while generated INDEX says `0.1.2`, requiring release metadata reconciliation. Framed-sidecar/license match. iOS/macOS Swift exact `0.3.7` trails source `0.3.8`; local adopted record is older `0.1.1`.

### Handoff checklist

- Reconcile ocr source-vs-INDEX & publish/record all selected npm/Cargo/Swift versions before edits.
- Review no-direct-import web declarations against transitive package contracts & generated theme workflow.
- Preserve sidecar supervisor, model compatibility, OCR/ONNX fallback, Parakeet/Whisper/ytdlp, cloud-sync schema, entitlement bridge, & installer policy.
- Acceptance: scrape/import/share, sidecar restart/framing, model install/hash/compatibility, OCR fallback/native path, license/diagnostics/logging, iOS/macOS credential/share flows, & standalone install resolve published immutable specs.

### Remaining local-functionality candidates (evidence-based)

| Local body | Matching RightKit capability | Disposition & acceptance gate |
|---|---|---|
| `src-tauri/src/engine_supervisor.rs:1-30,32-54,67-129,145-209,542-565` | `rightkit-process` + `rightkit-framed-sidecar` | Candidate after API parity. Shared owned tree, bounded termination, restart/backoff & health can replace mechanics; keep framed protocol, 16 MiB line cap, pending-RPC drain, replay snapshots & ScrapeRight lifecycle state. Acceptance: descendant cleanup (including yt-dlp/FFmpeg), timeout, restart window/backoff, frame errors & replay. |
| `src-tauri/src/playback.rs:1-14,16-123,126-159,161-217` | `rightkit-media` + `rightkit-playback` | Strong migration candidate after publication/API fit. Shared verified FFmpeg/ffprobe, args, process ownership, cancellation & playback registry match local mechanics; keep transcript-row lookup, app cache scope, media-kind policy & asset protocol allowlist. Acceptance: verified binary/catalog, audio/video compatibility copy, cancellation, cache fingerprint & no broad asset exposure. |
| `crates/engine/src/extraction_audio.rs:5-13,91-152,257-325,508-571`; `crates/audio/src/process.rs` | `rightkit-media` + `rightkit-audio` | Migrate binary resolution/probe, owned FFmpeg execution, WAV I/O & generic chains after API fit; keep yt-dlp/cookie policy, trim limits, output schema, duration/codec fallbacks & job orchestration. Acceptance: 16 kHz mono contract, bounded child tree, codec probe, cancellation & provider policy. |
| `crates/speech/src/asr_window.rs:1-29,498-572`; `crates/engine/src/asr_route.rs`; `crates/engine/src/whisper.rs` | `rightkit-asr` | Candidate after decoder parity. Shared ONNX/CoreML/Whisper decode can replace generic runtime; keep model/artifact identity, Parakeet/Whisper entitlement routing, long-audio windows, detok, provider fallback & transcript schema. Acceptance: exact timestamps/text, cancellation/progress, macOS CoreML & Windows ONNX behavior. |
| `crates/engine/src/net.rs:1-53`; `crates/engine/src/extraction_audio.rs:701-709`; `crates/engine/src/browser/mod.rs:1-16,39-62`, `crates/engine/src/browser/agent.rs:1-27` | `rightkit-http`, `rightkit-browser` | HTTP timeout/retry/body bounds are candidates after API fit. Current browser adapter runs bundled agent-browser subprocess; source states former CDP backend was removed. Adopting RightKit browser would require protocol/backend parity, not a direct swap. Keep ScrapeRight SSRF/public-IP policy, redirect decisions, cookie-from-browser consent, agent-browser profile/stealth settings, extraction selectors & login-wall behavior. Acceptance: private/link-local/NAT64 rejection, redirect policy, profile isolation, trusted input & cancellation. |

These mappings identify reusable mechanics only; ScrapeRight's ingestion, extraction, model and platform policy stay app-owned.

## Cross-app handoff gates

1. Treat `rightkit-versions.json` as local release bookkeeping only; publication, immutable URL/hash, & target-qualified records must be established by release workflow.
2. Reconcile source/staged release intent before selecting consumer pins: OCR source `0.2.0` vs generated INDEX/staged `0.1.2`; Swift source `0.3.8` vs staged `0.3.1`. Swift adopted baseline `0.1.1` is separate lifecycle state. CodeRight's generated `/Volumes/D/claude/coderight/apps/coderight-ios/CodeRight.xcodeproj/project.pbxproj:2368-2375` resolves exact `0.1.1`, while project.yml retains lower-bound generator input `0.1.0`; repair that generator drift.
3. Keep app-owned policy/schema/orchestration identified above; adoption evidence supports replacing duplicated primitives only after API/feature parity, migration, & acceptance checks pass.
4. For every app, verify no Git/path/file/workspace dependency remains, exact features are preserved (`sqlcipher`, `capture`, `native`, `session`, `wv2-0_38`, `bundled`), & each native target resolves exact Swift tag.

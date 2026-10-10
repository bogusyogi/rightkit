# 3. App adoption inventory & handoffs

Audit date: 10 October 2026, Asia/Kolkata. RightKit baseline: `08252a681d9f9dc2911962a49fa0026a4c345dff`, with existing uncommitted OCR/fsindex & lockfile changes. Findings use current source; no previous audit findings were reused. Source edits, publication & app migration are outside this report delivery.

Evidence labels: **observed** = manifest/source fact; **candidate** = proposed removal or consolidation requiring focused verification; **keep** = justified boundary; **unproven** = unavailable runtime/registry evidence. Source version, staged version, recorded registry version & adopted baseline are distinct lifecycle states.

## Apple follow-up completed

Both requested Apple skills were applied after initial report delivery. All 14 products & native consumer/helper boundaries now have explicit verdicts. Native checks passed except obsolete archive-helper tests; source review identified privacy, cancellation, persistence & concurrency fixes. Product source remains unchanged.

- [apple-verification.md](/Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-10/evidence/apple-verification.md) — commands, results, compiler warnings, skill/toolchain provenance & qualification scope.
- [apple-closure.md](/Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-10/evidence/apple-closure.md) — one prioritized backlog with owners & exact acceptance checks; use this for follow-up instead of restarting full audit.

Concurrent workspace delta: Pulse now declares `@rightkit/release 0.2.146` (original snapshot: `0.2.141`) & adds source/installed QA receipt mappings. Its installed plan still records failure pending a release-safe driver. Concurrent edits affect Pulse, SwiftRight, Legion & RightKit release/Git tooling; original broad audit remains a source snapshot. Audited Swift package/helper source hashes remain unchanged. Original machine-readable inventories retain snapshot values; [workspace-drift.json](/Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-10/evidence/workspace-drift.json) records newer Pulse dependency value, commit & changed-file hashes for handoff. No other captured package dependency declaration changed at this follow-up snapshot.

## Handoff rules

“Declared” is not “fully adopted.” Tables separate source use, local policy wrappers, duplicated primitives, version drift & unimplemented targets. A missing dependency does not automatically require migration. Use only verified published coherent versions; staged values below describe intended release work.

Source census covers 17 named primary repositories, two workflow checkouts & three retained Membrane worktrees. FreeCAD is third-party fork/reference; PostRight is docs-only; Membrane copies represent one product. ScreenRight archive & missing Citadel are explicitly recorded. Build copies, research clones, caches & installed third-party applications are not separate RightKit consumer projects.

## Snapshot manifest census

| Checkout | Distinct declared RightKit npm/Cargo names | Manifests declaring them | Lexical references | Source identity |
|---|---:|---:|---:|---|
| `coderight` | 28 | 22 | 475 | `f96b73921ea6` |
| `heardright` | 22 | 9 | 621 | `c87afb5ed3af` |
| `viewright` | 20 | 2 | 193 | `27249b78b040` |
| `mailright` | 16 | 2 | 189 | `4deba6349d33` |
| `scraperight` | 24 | 9 | 410 | `4dc55e40d9c0` |
| `cutright` | 23 | 12 | 222 | `532fadfcc56c` |
| `genright` | 24 | 9 | 266 | `8463e35e6e77` |
| `voiceright` | 1 | 1 | 5 | `c42cf611f043` |
| `workright` | 0 | 0 | 3 | `f4a27f809945` |
| `pulse` | 6 | 4 | 52 | `d701b5ee5c65` |
| `swiftright` | 1 | 1 | 48 | `820eb26c7beb` |
| `legion` | 4 | 1 | 164 | `7cad7dc01af4` |
| `rustright` | 3 | 8 | 211 | `d18cc9c8ab6f` |
| `sellright` | 1 | 1 | 21 | `8774314134ad` |
| `rightsites` | 1 | 1 | 25 | `6612d4de5b09` |
| `postright` | 0 | 0 | 0 | `db0276f062d2` |
| `xtool` | 0 | 0 | 0 | `17dcc15c67b4` |
| `lightcraft` | 0 | 0 | 0 | `fd6a99894907` |
| `freecad` | 0 | 0 | 0 | `fe1bdd2aa0f7` |
| `membrane:membrane-main-f602-baseline` | 5 | 3 | 68 | `retained worktree; Git metadata unavailable` |
| `membrane:membrane-blueprint-findings` | 5 | 3 | 68 | `retained worktree; Git metadata unavailable` |
| `membrane:membrane-live-diagnostics-phase1-repair` | 5 | 3 | 68 | `retained worktree; Git metadata unavailable` |

Counts include workspace/dev/build declarations; lexical references can include comments or types. Swift products are enumerated in per-app sections. Zero declared npm/Cargo packages is not proof of zero shared functionality.

## Routing & priorities

| App/group | Handoff focus |
|---|---|
| CodeRight | Desktop/context dependency closures; old fs/watch/http/db versions; generated iOS vs project.yml pin consistency; shared mechanisms behind local policy. |
| HeardRight | UI generation alignment; speech/service/process/model versions; Apple products & Overlay usage; retain audio/ASR product policy. |
| ViewRight | Native logs/db/http/secrets/OCR; evaluate PDF/playback mechanisms separately from document fidelity. |
| MailRight | SQLCipher-safe db/embed/OAuth/http/secret updates; preserve Gmail schema, key namespace & content security. |
| ScrapeRight | Broad UI/native alignment; shared supervisor/processing candidates; Swift helpers; preserve extraction/model compatibility. |
| CutRight & GenRight | Preserve media/engine policy; align existing shared APIs; evaluate remaining transcript/process/model mechanisms. |
| VoiceRight & WorkRight | Largest initial-adoption surface: native inference/process/model & storage/secret/capture primitives; qualify API compatibility first. |
| Pulse & SwiftRight | Mixed native/UI targets; avoid unsupported toolchain/runtime swaps; preserve bridge ABI & service protocol. |
| Legion & RustRight | Release/AX source ownership & metadata; exact consumer version specs; keep broker/orchestrator policy local. |
| SellRight & RightSites | Exact Swift declarations plus legal/diagnostics use; no forced desktop UI packages. |
| Xtool & LightCraft | Runtime-compatible adoption only; larger native bindings remain explicit design work. |
| Membrane, ScreenRight, Citadel, PostRight | Restore/select current implementation source before assigning migration; distinguish retained, archived, absent & docs-only states. |


## Five core apps: pins, implementations & handoff checks

Date: 2026-10-10. Read-only pass. Inputs were each repository's `AGENTS.md`, current app manifests/source, & current RightKit source. Prior audits/reports/plans were ignored. No build, test, network, install, source edit, or commit was run.

### Reading local RightKit status

RightKit README defines package ownership & says consumers use published versions (`README.md:5-27`). Lifecycle/status is source, staged, published, adopted; `packages/release/rightkit-versions.json` is only local recorded status (`README.md:31-46`). Current source versions used for comparison below are direct Cargo manifests: db `0.3.2`, embed `0.3.1`, fs `0.3.3`, http `0.2.4`, logs `0.2.0`, ocr `0.2.0`, ort `0.2.2`, process `0.3.3`, secrets `0.1.2`, shell `0.1.4`, tauri `0.1.2`, watch `0.3.1`, models `0.2.7`, asr `0.0.2`, audio `0.2.0`, browser `0.2.3`, cache `0.1.0`, crash `0.2.0`, mcp `0.5.2`, oauth `0.3.0`, sandbox `0.3.3`, speech-client `0.3.2`, framed-sidecar `0.1.0` (`crates/rightkit-*/Cargo.toml:3`). Current npm source versions are in `INDEX.md:80-95`; local npm adopted/staged records are `packages/release/rightkit-versions.json:4-37`. Swift source is `swift/RightKitSwift/VERSION:1` (`0.3.8`); local release record says adopted `0.1.1`.

“Used” below means source import or manifest-backed runtime use found in this pass. “Candidate review” means no direct use was found; it is not an automatic removal instruction. “Keep” means product policy, schema, UI, orchestration, or platform glue remains app-owned even where it wraps RightKit.

Evidence path roots are absolute: CodeRight `/Volumes/D/claude/coderight`, HeardRight `/Volumes/D/claude/heardright`, ViewRight `/Volumes/D/claude/viewright`, MailRight `/Volumes/D/claude/mailright`, ScrapeRight `/Volumes/D/claude/scraperight`, RightKit `/Volumes/D/claude/rightkit`. Tables abbreviate these fixed roots; every `path:line` reference expands directly to its app root.

Exact raw declaration inventory is retained in parent evidence `app-rightkit-dependencies.csv`; tables here show short versions for readable source mapping. This report records source use separately from inferred adoption candidates.

### CodeRight

Root for every path in this section: `/Volumes/D/claude/coderight`.

#### Actual declarations & observed use

| Target | Exact declaration | Observed use |
|---|---|---|
| Desktop/web | `apps/coderight-tauri/package.json:127-132,145`: npm `legal 0.3.1`, `license 0.1.8`, `logs 0.1.4`, `shell 0.1.0`, `tauri 0.1.1`, `updates 0.2.5`; dev `release 0.2.133` | `src/state/updateLicense.ts:6,15-16`; `src/lib/logs.ts:1`; `src/lib/notifications.ts:12`; `src/state/updateLicense.ts:6,15-16`; `components/updater/UpdateOverlay.tsx:2`; scripts use legal/release. Rust mirrors license/logs/shell/tauri/ort in `apps/coderight-tauri/src-tauri/src/{license.rs,logs.rs,notifications.rs,desktop_shell.rs,lib.rs}:1-1202`. |
| Desktop Rust | `apps/coderight-tauri/src-tauri/Cargo.toml:48-52`: license `0.1.3`; logs `0.2.0`, `default-features=false`; tauri `0.1.1`; shell `0.1.2`, `default-features=false`, `wv2-0_38`; ort `0.2.0`, `default-features=false` | `src-tauri/src/logs.rs:1,88-102`; notifications/desktop shell; `src-tauri/src/lib.rs:1191-1202` ort. `src-tauri/crates/auth/Cargo.toml:1+` also declares tauri `0.1.1` and is consumed by auth target. |
| Engine crates | `engine/Cargo.toml:148-159`: watch `0.1.0`, fs `0.1.0`, oauth `0.2.0`, http `0.1.0`, mcp `0.5.1`, secrets `0.1.0`, cache `0.1.0`, ort `0.2.0`/no defaults, db `0.2.1`, browser `0.2.0`; `engine/crates/media/Cargo.toml`: ocr `0.1.2`/no defaults/`native`; support bundle logs `0.2.0`; tools sandbox `0.2.4`, process `0.3.2` | Storage uses db/fs (`engine/crates/storage/src/lib.rs:129-149,181-191`); migrations `:36,1562`; mcp `engine/crates/mcp/src/manager.rs:9`; auth fs `engine/crates/tools/src/mcp_auth.rs:5`; sandbox/process tool modules; watch `engine/bins/coderight/src/api_fs_watch.rs:40`; runtime fs `:14,36,153,1471,1491-1500`; ort `engine/crates/ort_common/src/lib.rs:5`; ocr `engine/crates/media/src/lib.rs:581-605`. |
| Nested context | `context/package.json:47-53`: `@rightkit/hooks 0.1.1`; dev `ax 0.2.4`, `git 0.2.24`, `release 0.2.121`. Nested Cargo manifests declare db `0.3.0`, embed `0.2.2`, ort `0.2.0`, watch `0.3.0`, mcp `0.5.1`, process `0.3.2` | Context source search found no direct `@rightkit/hooks` import; package scripts/fixtures need review before removal. Nested Rust consumers are under `context/` and are separate from desktop root. `coderight-web/package.json` has no RightKit declaration; no RightKit source import found. |
| iOS | Generator input `apps/coderight-ios/project.yml:23-28,123-126`: RightKitSwift URL `from: 0.1.0`; products License & Diagnostics. Generated `CodeRight.xcodeproj/project.pbxproj:2368-2375` pins/resolves exact `0.1.1` | Imports: `CodeRight/License/LicenseStore.swift:3`, `LicenseView.swift:2`, `Tests/LicenseTests.swift:1`. Generated project is current exact-resolution evidence; project.yml remains generator drift and must be reconciled before changing source. |

#### Candidate review & local duplication

`context/package.json:@rightkit/hooks` is only candidate review: no direct source import was found. Rust modules named `process_launch.rs`, `process_resource_authority.rs`, `process_tree.rs`, `browser.rs`, `browser_control.rs`, `browser_fetch.rs`, `sandbox.rs` are active CodeRight policy/control layers despite RightKit process/browser/sandbox declarations; retain until behavior parity is proved. `src-tauri/src/notifications.rs:64` explicitly preserves non-Windows delivery while shared shell coverage is incomplete.

Local wrappers map to existing APIs: `engine/crates/storage/src/lib.rs` → `rightkit-db` + `rightkit-fs`; `engine/bins/coderight/src/api_fs_watch.rs` → `rightkit-watch`; `engine/crates/media/src/lib.rs` → `rightkit-ocr`; `src-tauri/src/logs.rs` → `rightkit-logs`; license/update/auth files → `rightkit-license`, `rightkit-tauri`, `rightkit-updates`; notification/desktop shell → `rightkit-shell`. Keep app-level execution/context/semantic ownership, auth/session mapping, mobile editor/UI, GitHub/Cloudflare connectors, & notification fallbacks.

#### Version/prerequisite gaps

Npm app pins trail local staged/source records: tauri `0.1.1` vs source `0.2.0` staged record; release `0.2.133` vs staged `0.2.147`; other npm pins are listed above and must be checked against staged records before handoff. Cargo pins trail current source for watch (`0.1.0` vs `0.3.1`), fs (`0.1.0` vs `0.3.3`), http (`0.1.0` vs `0.2.4`), oauth (`0.2.0` vs `0.3.0`), mcp (`0.5.1` vs `0.5.2`), secrets (`0.1.0` vs `0.1.2`), db (`0.2.1` vs `0.3.2`), browser (`0.2.0` vs `0.2.3`), ort (`0.2.0` vs `0.2.2`), sandbox (`0.2.4` vs `0.3.3`), process (`0.3.2` vs `0.3.3`), & ocr (`0.1.2` vs direct source `0.2.0`; generated INDEX records `0.1.2`, so release metadata needs reconciliation). iOS has generator/generated drift: project.yml lower-bound `from: 0.1.0`, generated project exact `0.1.1` (`CodeRight.xcodeproj/project.pbxproj:2368-2375`), source `0.3.8` & local release adopted baseline `0.1.1`; reconcile lifecycle state before selecting a newer tag.

#### Handoff checklist

- Replace each Cargo/npm pin only after corresponding package publication & manifest/hash record; resolve ocr source-vs-INDEX discrepancy first.
- Convert `apps/coderight-ios/project.yml:23-28` to exact RightKitSwift tag; verify License/Diagnostics imports compile in every iOS target.
- Review `context/package.json` hooks usage in scripts/fixtures; remove only if no script/runtime edge remains.
- Acceptance: clean standalone install resolves no path/Git/workspace specs; license/update/logs/shell flows work; auth/mcp/fs-watch/storage/ocr/ort integration passes app QA; iOS license UI/tests pass; Windows notification fallback remains intact.

#### Remaining local-functionality candidates (evidence-based)

| Local body | Matching RightKit capability | Disposition & acceptance gate |
|---|---|---|
| `apps/coderight-tauri/src/components/app/AppFrame.tsx:23-49,74-126,156-180`; `Sidebar.tsx:40-80,85-180`; `TitleBar.tsx:79-110`; `CommandPalette.tsx` | `@rightkit/app-shell`, `@rightkit/platform-ui` | Candidate after API fit. Shared frame/palette/window controls could replace generic shell; keep CodeRight session tree, provider panes, command actions & keyboard policy. Acceptance: palette focus/shortcut behavior, deep links, multi-window/session restore match. |
| `apps/coderight-tauri/src-tauri/src/os_shell/tray.rs:11-18,121-180`; `autostart.rs:21-119`; `window_state/mod.rs:16-24,110-180` | `rightkit-shell` | Candidate after parity. Shared tray/autostart/window primitives fit; keep app menu snapshot, platform fallbacks & persistence policy. Acceptance: tray actions, startup toggle, geometry bounds, crash/restart restore on macOS/Windows/Linux. |
| `engine/crates/tools/src/process_launch.rs:1-40`; `process_tree.rs:1-40`; `process_resource_authority.rs:1-40` | `rightkit-process` | Candidate after API gap. Shared ownership/termination can replace mechanics; keep CodeRight command allowlist, resource authority, daemon protocol & cancellation policy. Acceptance: descendant cleanup, timeout/restart, inherited handles & error mapping. |
| `engine/bins/coderight/src/api_fs_watch.rs:40`; `engine/bins/coderight/src/github_app/file_store.rs:32-49,66-94`; `apps/coderight-tauri/src/lib/transcriptSearch.ts` | `rightkit-fs`, `rightkit-watch`, `rightkit-search` | Existing fs/watch adoption is source-backed. Search is only candidate where backend has FTS/query contract; transcript/session fields & remote engine routing stay app-owned. Acceptance: same query semantics, ranking, lock ownership & watch debounce. |
| `apps/coderight-tauri/src/components/settings/SettingsModal.tsx`; `src-tauri/src/consent_dialog.rs` | `@rightkit/settings-ui`, `rightkit-settings` | Candidate after adapter fit. Generic sections/storage can move; consent, provider/API-key policy & dangerous-action confirmation stay local. Acceptance: migration/corruption recovery, secret references, accessibility & section search. |

These are local bodies with concrete shared matches, not package requirements; no migration is justified by declaration absence alone.

### HeardRight

Root for every path in this section: `/Volumes/D/claude/heardright`.

#### Actual declarations & observed use

| Target | Exact declaration | Observed use |
|---|---|---|
| Desktop/web | `tauri-app-next/package.json:157-166`: app-shell `0.1.0`, legal `0.3.1`, legal-ui `0.1.3`, license `0.1.8`, logs `0.1.4`, onboarding `0.1.0`, settings-ui `0.1.0`, tauri `0.1.1`, theme `0.1.0`, updates `0.2.5`; dev qa-chrome `0.1.3`, release `0.2.135` (`:179-180`) | Imports/use: app-shell in Sidebar/PageShell/TitleBar/App; onboarding CSS + legal-ui `src/pages/onboarding/OnboardingImpl.tsx:12,14`; settings-ui `src/pages/Settings.tsx:9`, `src/lib/rightkitSettingsBackend.ts:1`; theme `src/lib/theme.ts:1`; logs `src/lib/logs.ts:1`; updates `src/lib/autoUpdates.ts:1`; tauri `src/lib/license/core.ts:12`; scripts use legal/release/qa-chrome. |
| Desktop Rust | `tauri-app-next/src-tauri/Cargo.toml:37,39,41,74`: license `0.1.3`, logs `0.2.0`, tauri `0.1.1`, audio `0.2.0`, no defaults, feature `capture` | license/logs/tauri; onboarding mic/devices `src-tauri/src/onboarding_mic.rs:52,79-160`; audio runtime. |
| Workspace Rust | `heardright-app-runtime/Cargo.toml:15` process `0.3.2`; `heardright-engine/Cargo.toml:43,45` speech-client `0.3.0`, audio `0.2.0`/`capture`; `heardright-model-store/Cargo.toml:11` models `0.2.6`; `heardright-parakeet-core/Cargo.toml:12` asr `0.0.2`/no defaults; `heardright-runtime/Cargo.toml:13-16` speech-client `0.3.0`, service `0.1.0`, audio `0.2.0`/`capture`; telemetry `Cargo.toml:12-13` crash `0.2.0`, logs `0.2.0`; `heardright_core/Cargo.toml:23` speech-client `0.3.0` | ASR `heardright-parakeet-core/src/lib.rs:22`; models/hash `heardright-model-store/src/model_fetch.rs:177`; speech-client in core/runtime; service atomic files `heardright-runtime/src/speech_service/files.rs:19`; process lifecycle `heardright-app-runtime/src/engine_supervisor/lifecycle.rs:43`; telemetry logs/JSONL/crash `heardright-telemetry/src/lib.rs:5`, `crash_dump.rs:39,55`. |
| Apple/native | `ios-native/project.yml:16-19` RightKitSwift exact `0.3.2`; first app products `:131-154`: UI, Purchases, CloudSync, Store, ImportQueue, LiveActivity, WatchBridge, Speech, License, Intents, AppleCredentials, Overlay; more target declarations `:199-212,256-261,292-295,330-333` | Actual imports cover UI (`RecordingPillView.swift`, `ContentViewHomeControls.swift`), Purchases (`StoreKitProController.swift`), CloudSync (`TranscriptCloudSync.swift`), Store (`RecordingOwnershipLease.swift`, transcript stores), ImportQueue (share extension), LiveActivity, WatchBridge, Speech (recording/ASR files), License, Intents, AppleCredentials. No `RightKitOverlay` import found; declaration is candidate review. |

#### Candidate review & local duplication

`ios-native/project.yml:154` declares `RightKitOverlay`; source import search found no `RightKitOverlay`, so confirm target/linker use before retaining. Rust `condition_for_asr`, Zephyr wake logic, one detok path, model activation transaction, audio route/calibration, hotkeys/tray, Tauri/native recording UI, Watch/Live Activity/Intent orchestration remain HeardRight-owned. `heardright-runtime`/`heardright-engine` own ASR/provider policy over shared speech-client/audio; `heardright-model-store` owns product model policy over shared models; telemetry owns product event schema over shared logs/crash.

#### Version/prerequisite gaps

Web pins are behind current staged npm records: app-shell `0.1.0` vs `0.3.0`, legal-ui `0.1.3` vs `0.2.0`, license `0.1.8` current, logs `0.1.4` current, onboarding `0.1.0` vs `0.3.0`, settings-ui `0.1.0` vs `0.3.0`, tauri `0.1.1` vs `0.2.0`, theme `0.1.0` vs `0.2.1`, release `0.2.135` vs `0.2.147`; legal/updates/qa-chrome match locally staged records. Cargo speech-client `0.3.0` vs source `0.3.2`, process `0.3.2` vs `0.3.3`, models `0.2.6` vs `0.2.7`, service `0.1.0` vs `0.1.3`; audio/asr/crash/logs match source. Swift `0.3.2` trails local source `0.3.8`; publication/status must be verified before changing pin.

#### Handoff checklist

- Review Overlay declaration against target linker/source usage; remove only after target-level check.
- Publish/record compatible app-shell, legal-ui, onboarding, settings-ui, tauri, theme, speech-client, process, models, service, & Swift versions.
- Keep ASR/Whisper policy, Zephyr/`condition_for_asr`, detok, audio routing, model transactions, hotkeys/tray, recording UI, Watch/Live Activity/Intents.
- Acceptance: onboarding mic capture, warm capture, detok, ASR model activation, update/license/settings/logging, crash JSONL, Watch/Live Activity/Intent/share extension, & exact Swift package resolution pass app QA.

#### Remaining local-functionality candidates (evidence-based)

| Local body | Matching RightKit capability | Disposition & acceptance gate |
|---|---|---|
| `tauri-app-next/src-tauri/src/hotkey.rs:4-60,103-193`; `src-tauri/src/fn_hotkey/tap.rs:356-381` | `rightkit-hotkeys` | Candidate after API gap. Shared registration/Fn tap mechanics may replace native plumbing; keep PTT hold/toggle semantics, 300 ms debounce, rollback, accessibility permission flow & event-to-ASR policy. Acceptance: Fn+Space/chord behavior, no duplicate registration, macOS/Windows parity. |
| `tauri-app-next/src-tauri/src/pill.rs:1-8`; `src-tauri/src/pill/state.rs:36-66,134-160,317-330`; `src/pill/PillOverlay.tsx` | `rightkit-overlay` | Candidate for geometry/state primitives only. RightKit overlay does not replace HeardRight native window creation, click-through, rendering or `pill://event` bridge. Acceptance: topmost/hover/visibility, monitor changes & accessibility behavior remain identical. |
| `tauri-app-next/src-tauri/src/settings.rs:20-130`; `src/pages/Settings.tsx:8-16,23-75`; `src/lib/rightkitSettingsBackend.ts:1` | `rightkit-settings`, `@rightkit/settings-ui` | UI is already adopted; persistence mechanics are candidate after adapter fit. Shared migrations/atomic writes/corruption recovery can replace generic file code, while allowlisted `SettingsBlob`, runtime sidecar sync, secrets & onboarding policy stay local. Acceptance: corrupt-file backup, migration, concurrent pill/engine reads & settings search. |
| `tauri-app-next/src-tauri/src/hotkey_setup.rs`; tray setup named in `src-tauri/Cargo.toml:110-121`; generated tray assets `src-tauri/scripts/gen_tray_icons.py:1-17` | `rightkit-shell` | Candidate after native-menu parity. Shared tray/menu/autostart/window primitives may replace generic shell; keep four-state recording icons, helper identity/heartbeat, click actions & platform policy. Acceptance: menu-bar/tray lifecycle, icon state, singleton behavior & restart recovery. |

Local ASR/provider choice, Zephyr wake, detok, model activation, audio route/calibration, Watch/Live Activity/Intent orchestration remain product capabilities even where shared speech/audio primitives match.

### ViewRight

Root for every path in this section: `/Volumes/D/claude/viewright`.

#### Actual declarations & observed use

| Target | Exact declaration | Observed use |
|---|---|---|
| Desktop/web | `package.json:97-104`: doc-render `0.1.3`, legal-ui `0.1.3`, license `0.1.8`, logs `0.1.4`, media-ui `0.2.6`, pdf-ui `0.1.2`, tauri `0.1.1`, updates `0.2.5`; dev legal `0.3.1`, qa-chrome `0.1.2`, release `0.2.135` (`:148`) | doc-render in `src/documents/markdown/*`; media-ui `src/documents/ImageDocument.tsx:1,4,8-9`; pdf-ui in `src/documents/pdf/*`; legal-ui `src/app/AppRoot.tsx:2`; tauri `src/state/tauriBridge.ts:9`; logs `src/state/diagnostics.ts:5`; license/updates `src/state/updateLicense.ts:15,24`; scripts use legal/qa/release. |
| Desktop Rust | `src-tauri/Cargo.toml:58,60,62-65,68,70,73`: db `0.2.1`/`bundled`; watch `0.3.1`; shell `0.1.3`/no defaults/`wv2-0_38`; logs `0.1.0`; license `0.1.3`; tauri `0.1.1`; secrets `0.1.1`; http `0.2.1`; ocr `0.1.2`/no defaults/`native` | db/logs `src-tauri/src/logs.rs:1-2,183-193`; watch `watcher.rs:8-9`; shell `lib.rs:228`; tauri/license/secrets/http `commands/license.rs:11-13,380,478-486,764-804`; ocr `ocr_backend.rs:1,8,14-49` plus worker/commands. |

#### Candidate review & local duplication

All declared ViewRight npm/Cargo packages had direct source or script use in this pass. Local wrappers map to matching APIs: `src-tauri/src/persistence.rs:5` app schema/transactions over rightkit-db; `logs.rs` IPC/event adapter over rightkit-logs; `watcher.rs` debounce/filter policy over rightkit-watch; `commands/license.rs` app entitlement endpoints over rightkit-license/tauri/secrets/http; `ocr_backend.rs`, `commands/ocr.rs`, `ocr_worker.rs` app payload/isolation/timeout around rightkit-ocr. Keep vendor/docx-editor fidelity, markdown semantics, PDF engine/AppKit/native behavior, encoding/chardetng, file associations, licensing adapter, & product UI.

#### Version/prerequisite gaps

Npm pins behind local staged records: doc-render `0.1.3` vs `0.1.4`, legal-ui `0.1.3` vs `0.2.0`, media-ui `0.2.6` vs `0.3.0`, tauri `0.1.1` vs `0.2.0`, qa-chrome `0.1.2` vs `0.1.3`, release `0.2.135` vs `0.2.147`; license/logs/pdf-ui/updates/legal match records. Cargo pins behind current source: db `0.2.1` vs `0.3.2`, shell `0.1.3` vs `0.1.4`, logs `0.1.0` vs `0.2.0`, secrets `0.1.1` vs `0.1.2`, http `0.2.1` vs `0.2.4`, ocr `0.1.2` vs direct manifest `0.2.0`; generated INDEX still records ocr `0.1.2`, requiring release metadata reconciliation. watch/license match; tauri `0.1.1` trails direct source `0.1.2`.

#### Handoff checklist

- Reconcile ocr source manifest vs generated INDEX before selecting pin.
- Publish/record doc-render, legal-ui, media-ui, tauri, qa-chrome, release, db, shell, logs, secrets, http, ocr, & tauri updates.
- Preserve docx editor, markdown/PDF fidelity, native PDF, schema/transactions, watcher filters, OCR isolation, file associations, & license adapter.
- Acceptance: markdown/image/PDF/docx open/render/edit/export, watcher recovery, OCR native path, license/update/logging, SQL/schema migration, Windows shell, & standalone install resolve published specs.

#### Remaining local-functionality candidates (evidence-based)

| Local body | Matching RightKit capability | Disposition & acceptance gate |
|---|---|---|
| `src-tauri/crates/pdf/src/inspect.rs:1-31,53-119`; `compose.rs:1-29,102-125,631`; `save.rs:401-430`; `src-tauri/src/commands/pdf_adapter.rs:103-180,204,337` | `rightkit-pdf` | Strong migration candidate after API parity. Local engine already performs inspect/compose/staged atomic save, but adapter carries ViewRight source graph, font/layout, SHA and Tauri types; RightKit must expose equivalent contracts first. Acceptance: page tree inheritance, text replacement/addition, OCR correction, font subset, source identity recheck, staged save/commit & failure recovery. |
| `src/documents/pdf/PdfViewer.tsx:886-1163`; `PdfInlineTextEditor.tsx`; `pdfSave.ts:110-250`; `pdfBenchTransport.ts:68-85` | `@rightkit/pdf-ui`, `rightkit-pdf` | Keep ViewRight editor/render/probe orchestration. `pdf-ui` is read-only viewer, `rightkit-pdf` has no rendering/layout engine; migrate only shared viewer/probe helpers when edit-aware API exists. Acceptance: pdf.js worker/range probe, text overlay alignment, undo/redo, OCR and render-before-commit remain unchanged. |
| `src-tauri/src/site_root.rs:15,81-92,260-270,378`; `src/documents/html/htmlInline.ts:279-340`; CSP media rules `src/documents/html/HtmlViewer.tsx:249` | `rightkit-playback`, `rightkit-media` | No local FFmpeg/playback/probe implementation was found. These files stream HTML audio/video & MIME assets through ViewRight's origin/CSP; RightKit FFmpeg playback/media crates are not automatic requirements. Revisit only if app adds verified FFmpeg conversion or native playback. |
| `src/documents/ImageDocument.tsx:1-19`; `src/app/useAutoSave.ts:3`; `src/styles/index.css:908` | `@rightkit/media-ui` | Already adopted image viewer/editor; retain ViewRight save adapter and visual tokens. Acceptance: image edit snapshot/autosave, format limits & path broker behavior. |

The PDF crate is duplicated mechanism with a clear shared match, but app fidelity/source graph constraints make “after API gap” the safe disposition; no package addition follows from media MIME handling alone.

### MailRight

Root for every path in this section: `/Volumes/D/claude/mailright`.

#### Actual declarations & observed use

| Target | Exact declaration | Observed use |
|---|---|---|
| Desktop/web | `package.json:67-70`: license `0.1.8`, logs `0.1.4`, tauri `0.1.1`, updates `0.2.5`; dev legal `0.3.1`, release `0.2.135` (`:93-94`) | license/updates `src/lib/license.ts:29-36`, `src/lib/updateAuth.ts:1-2`; logs `src/lib/telemetry.ts:22`; tauri `src/lib/ipc/secure.ts:1`; scripts legal/release. |
| Desktop Rust | `src-tauri/Cargo.toml:58,88-90,104,106,108,110-113`: db `0.2.1`/no defaults/`sqlcipher`; license `0.1.3`; logs `0.2.0`; tauri `0.1.1`; embed `0.2.1`; cache `0.1.0`; secrets `0.1.0`; oauth `0.2.0`/no defaults; http `0.1.0`; optional shell `0.1.2`/no defaults/`wv2-0_38` | db `src-tauri/src/db.rs:28`; embed `src-tauri/src/db/semantic/embedder.rs:14`; cache `attach_cache.rs:16`; secrets `credential_store.rs:11,155-157`; oauth/http `oauth.rs:6-7`, `google.rs:25-27`; logs `logs.rs:1`; shell desktop/tauri-run `desktop.rs:23,210-322`, `tauri_run.rs:17-30,295-307`; license/tauri `commands/license.rs:40-235`; web imports above. |

#### Candidate review & local duplication

No declared MailRight package lacked direct source/script use in this pass. `src-tauri/src/db.rs` owns Gmail schema/query/SQLCipher transactions over rightkit-db; `db/semantic/*` owns sqlite-vec semantic index over rightkit-embed; `credential_store.rs` owns MailRight key namespace over rightkit-secrets; `oauth.rs`/`google.rs` own Gmail scopes/PKCE/endpoints over rightkit-oauth/http; logs/license/tauri files adapt IPC and entitlement records. Keep Gmail MIME/image sanitizer, remote-image protocol, composer/Tiptap, Gmail labels/search, semantic indexing, SQLCipher key/WAL invariant, & desktop shell.

#### Version/prerequisite gaps

Npm license/logs/updates/legal match local staged/source records; tauri `0.1.1` trails staged `0.2.0`; release `0.2.135` trails staged `0.2.147`. Cargo db `0.2.1` vs `0.3.2`, tauri `0.1.1` vs `0.1.2`, embed `0.2.1` vs `0.3.1`, secrets `0.1.0` vs `0.1.2`, oauth `0.2.0` vs `0.3.0`, http `0.1.0` vs `0.2.4`, shell `0.1.2` vs `0.1.4`; cache/logs/license match.

#### Handoff checklist

- Publish/record tauri, release, db, embed, secrets, oauth, http, shell versions; preserve SQLCipher feature & shell optionality.
- Exercise Gmail OAuth/scopes/PKCE, credential namespace, SQLCipher open/WAL checkpoint, label transaction, cache, semantic search, MIME/image sandbox, composer, update/license/logging.
- Acceptance: standalone install resolves published specs; no credential/token leaks through logs; Gmail sync/retry/offline behavior and Windows/macOS shell pass app QA.

#### Remaining local-functionality candidates (evidence-based)

| Local body | Matching RightKit capability | Disposition & acceptance gate |
|---|---|---|
| `src-tauri/src/commands/settings.rs:1-14,24-38,40-70`; `src/views/SettingsView.tsx:1-18,23-120` | `rightkit-settings`, `@rightkit/settings-ui` | Candidate after adapter fit. Shared versioned persistence, migrations, atomic writes & standard frame can reduce generic code; keep account/multi-mailbox sections, privacy-redacted diagnostics, Gmail policy & secret references. Acceptance: migrations/corruption recovery, redaction, section search & key rotation. |
| `src-tauri/src/db/search.rs:1-16,21-38,39-70` | `rightkit-search` | Candidate after schema mapping. Parser/FTS predicate/snippet primitives match; retain Gmail operators, account/label/category scope, candidate planner & ranking. Acceptance: identical Gmail query semantics, FTS planner, snippets/highlights & SQLCipher behavior. |
| `src-tauri/src/attachments.rs:1-27,53-92,96-180`; `src-tauri/src/gmail/mime.rs:1-46,48-115`; `src-tauri/src/webimg.rs:1-16,27-32,54-64,80-178` | `rightkit-http` (bounded fetch only); no generic attachment/MIME owner | Keep app-specific attachment tree, Gmail MIME/RFC composition, CID/remote-image policy, cache keying & tracker/size gates. A bounded shared HTTP adapter is a candidate after credential/cancellation/error mapping; it does not replace Gmail policy. Acceptance: MIME round-trip, hostile URL blocking, byte caps, cache isolation & redaction. |
| `src/components/AttachmentPdfPreview.tsx:1-24,26-98,100-164`; `AttachmentPreviewModal.tsx:48-99,142-180`; `views/ReaderMessage.tsx:55-77` | `@rightkit/pdf-ui`, `rightkit-pdf` | Candidate only for standard PDF viewer pieces. Local password flow, 30-page cap, spreadsheet/text/image preview and download-only safety remain MailRight-owned; `rightkit-pdf` is not a renderer. Acceptance: encrypted PDF unlock, bounded rendering, unsafe spreadsheet handling & attachment identity. |
| `src/lib/html.ts:3-16,40-69`; `src/views/ReaderMessage.tsx:17,91,204`; `src-tauri/src/mail_settings.rs:1-15,21-125` | `@rightkit/doc-render` | Keep email HTML sanitization/CID rewrite/sandbox & Gmail vacation/forwarding endpoints. `doc-render` sanitizes Markdown/HTML for reader documents, but cannot replace MailRight's untrusted email iframe policy or Gmail settings API. Acceptance: no script/event handler escape, CID/remote image behavior & compose/read parity. |

No local MailRight body proves a generic media-player requirement; `rightkit-media` would apply only if attachment playback/conversion is introduced.

### ScrapeRight

Root for every path in this section: `/Volumes/D/claude/scraperight`.

#### Actual declarations & observed use

| Target | Exact declaration | Observed use |
|---|---|---|
| Desktop/web | `package.json:78-88`: app-shell `0.2.0`, legal-ui `0.1.3`, license `0.1.6`, logs `0.1.3`, media-ui `0.2.5`, onboarding `0.2.2`, platform-ui `0.1.2`, settings-ui `0.2.7`, tauri `0.1.0`, theme `0.2.0`, updates `0.2.5`; dev legal `0.3.0`, qa-chrome `0.1.0`, release `0.2.133` (`:101-103`) | app-shell/media-ui `src/main.tsx:4,6,12`, `src/lib/theme.ts:1`, `views/DetailReader.tsx:2`; settings-ui `views/Settings.tsx:1,9`, `lib/settingsBackend.ts`; tauri `lib/license.ts`; license/updates `lib/updateAuth.ts`; logs `lib/logs.ts`; onboarding `SetupGate`; scripts legal/release/qa. `platform-ui`, `legal-ui`, `theme` have no direct package import in `src`/`scripts`; `src/generated/brand.css:1` records theme generator provenance. |
| Desktop Rust | `src-tauri/Cargo.toml:27,55,59-62`: framed-sidecar `0.1.0`, fs `0.3.0`, ort `0.2.0`/no defaults, logs `0.1.0`, license `0.1.3`, tauri `0.1.1`, secrets `0.1.0`; workspace engine `Cargo.toml:17-19,57` same framed/fs/ort/secrets; model-store models `0.2.6`; ocr ocr `0.1.2`/`native`, ort `0.2.0`/`session`; ort_common/speech ort `0.2.0`; protocol license optional; store db `0.2.1` | sidecar `crates/engine/src/main_stdio.rs:2`, `src-tauri/src/engine_supervisor.rs:48`; fs/license/secrets/tauri `src-tauri/src/license.rs:60-71,144-193,242-287,314-352,442-521`; logs `src-tauri/src/diagnostics.rs:1,119-145`; db `crates/store/src/lib.rs:38-39`; models `crates/model-store/src/discover_compat.rs:5-6`; ocr/ort `crates/ocr/src/lib.rs:48-57,93-97`, ort_common; main ort `src-tauri/src/main.rs:195-196`. |
| Apple/native | `ios-native/project.yml:19-22` RightKitSwift exact `0.3.7`; products `:97-112`: AppleCredentials, Artifacts, Diagnostics, ImportQueue, Purchases, CloudSync, Store, License; share extension `:124-127` AppleCredentials/ImportQueue. `macos-native/ScrapeRightAppleCredentials/Package.swift:11,17`: exact `0.3.7`, AppleCredentials | iOS imports Diagnostics, ImportQueue, Artifacts, License, Purchases, CloudSync, AppleCredentials in `ScrapeRight/` & share sources; macOS `main.swift:77` AppleCredentials. Every declared native product had observed source use. |

#### Candidate review & local duplication

`@rightkit/platform-ui`, `@rightkit/legal-ui`, & `@rightkit/theme` have no direct source import found, but this is not stale-dependency evidence: `packages/app-shell/package.json:33-35` declares platform-ui as app-shell peer; `packages/onboarding/package.json:26-30` declares legal-ui as onboarding peer; `packages/settings-ui/package.json:27-34` declares theme as settings-ui peer; `src/generated/brand.css:1` records theme-generator provenance. Verify peer resolution & generation scripts before any removal. Rust `src-tauri/src/engine_supervisor.rs` owns sidecar lifecycle/restart/queue around framed-sidecar; `src-tauri/src/logging.rs:29-99` owns tracing-appender setup around rightkit-logs; license owns app entitlement bridge; model-store compatibility wrappers are explicit (`crates/model-store/src/lib.rs:38-49`); OCR owns ONNX/fallback routing around rightkit-ocr; speech/audio own Parakeet/Whisper/ytdlp modalities. Keep cloud-sync schema/iOS adapters, installer/update policy, model compatibility, extraction modalities, OCR fallback, & platform-specific key/entitlement glue.

#### Version/prerequisite gaps

All npm pins except updates trail staged records: app-shell `0.2.0` vs `0.3.0`, legal-ui `0.1.3` vs `0.2.0`, license `0.1.6` vs `0.1.8`, logs `0.1.3` vs `0.1.4`, media-ui `0.2.5` vs `0.3.0`, onboarding `0.2.2` vs `0.3.0`, platform-ui `0.1.2` vs `0.2.0`, settings-ui `0.2.7` vs `0.3.0`, tauri `0.1.0` vs `0.2.0`, theme `0.2.0` vs `0.2.1`, legal `0.3.0` vs `0.3.1`, qa-chrome `0.1.0` vs `0.1.3`, release `0.2.133` vs `0.2.147`. Cargo fs `0.3.0` vs `0.3.3`, ort `0.2.0` vs `0.2.2`, logs `0.1.0` vs `0.2.0`, tauri `0.1.1` vs `0.1.2`, secrets `0.1.0` vs `0.1.2`, models `0.2.6` vs `0.2.7`, db `0.2.1` vs `0.3.2`; ocr app pin `0.1.2` conflicts with direct source `0.2.0` while generated INDEX says `0.1.2`, requiring release metadata reconciliation. Framed-sidecar/license match. iOS/macOS Swift exact `0.3.7` trails source `0.3.8`; local adopted record is older `0.1.1`.

#### Handoff checklist

- Reconcile ocr source-vs-INDEX & publish/record all selected npm/Cargo/Swift versions before edits.
- Review no-direct-import web declarations against transitive package contracts & generated theme workflow.
- Preserve sidecar supervisor, model compatibility, OCR/ONNX fallback, Parakeet/Whisper/ytdlp, cloud-sync schema, entitlement bridge, & installer policy.
- Acceptance: scrape/import/share, sidecar restart/framing, model install/hash/compatibility, OCR fallback/native path, license/diagnostics/logging, iOS/macOS credential/share flows, & standalone install resolve published immutable specs.

#### Remaining local-functionality candidates (evidence-based)

| Local body | Matching RightKit capability | Disposition & acceptance gate |
|---|---|---|
| `src-tauri/src/engine_supervisor.rs:1-30,32-54,67-129,145-209,542-565` | `rightkit-process` + `rightkit-framed-sidecar` | Candidate after API parity. Shared owned tree, bounded termination, restart/backoff & health can replace mechanics; keep framed protocol, 16 MiB line cap, pending-RPC drain, replay snapshots & ScrapeRight lifecycle state. Acceptance: descendant cleanup (including yt-dlp/FFmpeg), timeout, restart window/backoff, frame errors & replay. |
| `src-tauri/src/playback.rs:1-14,16-123,126-159,161-217` | `rightkit-media` + `rightkit-playback` | Strong migration candidate after publication/API fit. Shared verified FFmpeg/ffprobe, args, process ownership, cancellation & playback registry match local mechanics; keep transcript-row lookup, app cache scope, media-kind policy & asset protocol allowlist. Acceptance: verified binary/catalog, audio/video compatibility copy, cancellation, cache fingerprint & no broad asset exposure. |
| `crates/engine/src/extraction_audio.rs:5-13,91-152,257-325,508-571`; `crates/audio/src/process.rs` | `rightkit-media` + `rightkit-audio` | Migrate binary resolution/probe, owned FFmpeg execution, WAV I/O & generic chains after API fit; keep yt-dlp/cookie policy, trim limits, output schema, duration/codec fallbacks & job orchestration. Acceptance: 16 kHz mono contract, bounded child tree, codec probe, cancellation & provider policy. |
| `crates/speech/src/asr_window.rs:1-29,498-572`; `crates/engine/src/asr_route.rs`; `crates/engine/src/whisper.rs` | `rightkit-asr` | Candidate after decoder parity. Shared ONNX/CoreML/Whisper decode can replace generic runtime; keep model/artifact identity, Parakeet/Whisper entitlement routing, long-audio windows, detok, provider fallback & transcript schema. Acceptance: exact timestamps/text, cancellation/progress, macOS CoreML & Windows ONNX behavior. |
| `crates/engine/src/net.rs:1-53`; `crates/engine/src/extraction_audio.rs:701-709`; `crates/engine/src/browser/mod.rs:1-16,39-62`, `crates/engine/src/browser/agent.rs:1-27` | `rightkit-http`, `rightkit-browser` | HTTP timeout/retry/body bounds are candidates after API fit. Current browser adapter runs bundled agent-browser subprocess; source states former CDP backend was removed. Adopting RightKit browser would require protocol/backend parity, not a direct swap. Keep ScrapeRight SSRF/public-IP policy, redirect decisions, cookie-from-browser consent, agent-browser profile/stealth settings, extraction selectors & login-wall behavior. Acceptance: private/link-local/NAT64 rejection, redirect policy, profile isolation, trusted input & cancellation. |

These mappings identify reusable mechanics only; ScrapeRight's ingestion, extraction, model and platform policy stay app-owned.

### Cross-app handoff gates

1. Treat `rightkit-versions.json` as local release bookkeeping only; publication, immutable URL/hash, & target-qualified records must be established by release workflow.
2. Reconcile source/staged release intent before selecting consumer pins: OCR source `0.2.0` vs generated INDEX/staged `0.1.2`; Swift source `0.3.8` (unpublished; published tags are `0.1.0`, `0.1.1`, `0.3.0`-`0.3.7`) vs staged `0.3.1`. Swift adopted baseline `0.1.1` is separate lifecycle state. CodeRight's generated `/Volumes/D/claude/coderight/apps/coderight-ios/CodeRight.xcodeproj/project.pbxproj:2368-2375` resolves exact `0.1.1`, while project.yml retains lower-bound generator input `0.1.0`; repair that generator drift.
3. Keep app-owned policy/schema/orchestration identified above; adoption evidence supports replacing duplicated primitives only after API/feature parity, migration, & acceptance checks pass.
4. For every app, verify no Git/path/file/workspace dependency remains, exact features are preserved (`sqlcipher`, `capture`, `native`, `session`, `wv2-0_38`, `bundled`), & each native target resolves exact Swift tag.


## Newer products: pins, implementations & handoff checks

Scope: read-only inventory of current manifests, first-party source & RightKit source. Prior audits, reports & plans were ignored. No builds, tests, installs, network access, commits or source mutations were performed. Generated caches, lockfiles, `target/` & vendored trees were excluded from usage claims.

RightKit publishes Rust crates, npm packages & `RightKitSwift`; apps consume published versions, never sibling/path/Git specs: `/Volumes/D/claude/rightkit/README.md:3-7`, `/Volumes/D/claude/rightkit/README.md:29`. Package ownership is listed in `/Volumes/D/claude/rightkit/README.md:11-27`. `rightkit-versions.json` separates adopted top-level records (`:4-13`, `:257-272`, `:321-325`) from staged records (`:15-37`, `:274-319`, `:327-331`); staged is a readiness candidate, not proof of publication. This inventory marks exact manifest mismatches; a matching staged number still needs verified publication/coherent dependency set before handoff.

### CutRight — `/Volumes/D/claude/cutright`

Targets include root Cargo workspace, `apps/cutright` Vite/Tauri web target, `apps/cutright/src-tauri`, engine crates under `crates/`, `cutright-ui-e2e`, macOS & Windows package scripts: `/Volumes/D/claude/cutright/apps/cutright/package.json:23-32`.

Current npm pins: `legal 0.3.1`, `release 0.2.133`: `/Volumes/D/claude/cutright/apps/cutright/package.json:39-40`; `app-shell 0.2.0`, `legal-ui 0.1.3`, `license 0.1.8`, `logs 0.1.4`, `media-ui 0.2.0`, `onboarding 0.2.0`, `platform-ui 0.1.1`, `settings-ui 0.2.7`, `shell 0.1.0`, `theme 0.2.0`, `updates 0.2.4`: `/Volumes/D/claude/cutright/apps/cutright/package.json:50-61`. Matches against staged npm: `legal`, `license`, `logs`, `shell`; mismatches: `release`, `app-shell`, `legal-ui`, `media-ui`, `onboarding`, `platform-ui`, `settings-ui`, `theme`, `updates`: `/Volumes/D/claude/rightkit/packages/release/rightkit-versions.json:22-37`. Cargo pins: `rightkit-process 0.1.0` declared at `/Volumes/D/claude/cutright/Cargo.toml:51`; `audio 0.2.0` at `/Volumes/D/claude/cutright/crates/cutright-audio/Cargo.toml:20`; `fs 0.3.0` at `/Volumes/D/claude/cutright/crates/cutright-project/Cargo.toml:21`; `models 0.2.1` at `/Volumes/D/claude/cutright/crates/cutright-model-store/Cargo.toml:11`; `speech-client 0.3.0` & `ort 0.1.4` at `/Volumes/D/claude/cutright/crates/cutright-speech/Cargo.toml:11`, `:27`; `ort 0.1.4` at `/Volumes/D/claude/cutright/crates/cutright-reframe/Cargo.toml:20`; `jobs 0.2.1` at `/Volumes/D/claude/cutright/crates/cutright-engine/Cargo.toml:18`; `secrets 0.1.0` at `/Volumes/D/claude/cutright/crates/cutright-shorts/Cargo.toml:27`; `qa 0.2.12` at `/Volumes/D/claude/cutright/crates/cutright-ui-e2e/Cargo.toml:11`; `control 0.1.9` with `tauri-plugin` at `/Volumes/D/claude/cutright/apps/cutright/src-tauri/Cargo.toml:20`. Only Cargo `audio 0.2.0` matches staged `:274-319`; all other listed Cargo pins mismatch staged records. Matching staged numbers still require verified publication & coherent transitive set.

Actual shared use: app-shell shell/provider/toast, media-ui provider/styles, legal-ui styles at `/Volumes/D/claude/cutright/apps/cutright/src/main.tsx:3-10`; onboarding at `/Volumes/D/claude/cutright/apps/cutright/src/views/OnboardingGate.tsx:1-2`; settings-ui at `/Volumes/D/claude/cutright/apps/cutright/src/views/SettingsView.tsx:14-16`; media-ui views at `/Volumes/D/claude/cutright/apps/cutright/src/views/AssetsView.tsx:21-22`; release CLI is used by build/sign/install scripts at `/Volumes/D/claude/cutright/apps/cutright/scripts/build-engine.mjs:10` & `/Volumes/D/claude/cutright/apps/cutright/scripts/sign-macos-bundle.mjs:10`. Rust use: fs atomic functions at `/Volumes/D/claude/cutright/crates/cutright-project/src/fsutil.rs:8-20`; audio at `/Volumes/D/claude/cutright/crates/cutright-audio/src/lib.rs:21`; ORT runtime/session at `/Volumes/D/claude/cutright/crates/cutright-speech/src/onnx_tdt.rs:29-30`, `:69`, `/Volumes/D/claude/cutright/crates/cutright-reframe/src/detect.rs:6-7`; jobs at `/Volumes/D/claude/cutright/crates/cutright-engine/src/serve.rs:18`, `:128`; models adapter at `/Volumes/D/claude/cutright/crates/cutright-model-store/src/lib.rs:2-14`; secrets at `/Volumes/D/claude/cutright/crates/cutright-shorts/src/llm.rs:239-245`; control at `/Volumes/D/claude/cutright/apps/cutright/src-tauri/src/lib.rs:181`. Product bodies also own transcript schema/validation at `/Volumes/D/claude/cutright/crates/cutright-transcript/src/lib.rs:1-14`, speech word construction at `/Volumes/D/claude/cutright/crates/cutright-speech/src/words.rs:201-208`, transcript display/word mapping at `/Volumes/D/claude/cutright/apps/cutright/src/editor/TranscriptPanel.tsx:122-152`, native playback IPC at `/Volumes/D/claude/cutright/apps/cutright/src/lib/media.ts:510-543`, & audio timeline edits at `/Volumes/D/claude/cutright/apps/cutright/src/editor/Timeline.tsx:358-389`. RightKit `rightkit-audio` explicitly covers FFmpeg audio chains/WAV/measurement but not playback or transcription: `/Volumes/D/claude/rightkit/crates/rightkit-audio/README.md:3-20`; `rightkit-playback` prepares cached FFmpeg derivatives at `/Volumes/D/claude/rightkit/crates/rightkit-playback/README.md:1-15`. Therefore candidate reuse is bounded to audio processing or playback preparation; transcript schema/word timing & native playback IPC remain CutRight-specific.

Declared without first-party import found: `@rightkit/license`, `@rightkit/logs`, `@rightkit/platform-ui`, `@rightkit/shell`, `@rightkit/updates`; keep until dependency audit confirms generated/config-only intent. RightKit APIs exist for each concern: license/logs/platform/shell/updates are listed at `/Volumes/D/claude/rightkit/README.md:11-27`.

Local overlap & boundary: FCPXML writes `.partial`, flushes & renames at `/Volumes/D/claude/cutright/crates/cutright-fcpxml/src/lib.rs:586-590`, matching RightKit `atomic_write` at `/Volumes/D/claude/rightkit/crates/rightkit-fs/src/atomic.rs:51-62`; move file I/O primitive only, preserve FCPXML format/export policy. GenRight orchestration verifies bytes before rename at `/Volumes/D/claude/cutright/crates/cutright-genright/src/orchestrator.rs:653-715`; only generic atomic primitive is shared, verification policy remains app-specific. Engine supervisor owns restart/Windows flags at `/Volumes/D/claude/cutright/apps/cutright/src-tauri/src/engine_supervisor.rs:507-520`; RightKit `OwnedCommand`/`OwnedChild` are at `/Volumes/D/claude/rightkit/crates/rightkit-process/src/owned.rs:79-79`, `:219`, `:369`; retain supervisor protocol & restart policy. No RightKit path/Git dependency was found; internal app path dependencies remain app-owned.

Handoff checklist:

- Files: `apps/cutright/package.json`, `apps/cutright/pnpm-workspace.yaml`, root `Cargo.toml`, listed crate manifests, `src-tauri/Cargo.toml`, release scripts.
- Prerequisites: verify one coherent published npm/Cargo set; use `/Volumes/D/claude/rightkit/packages/release/rightkit-versions.json:22-37`, `:274-319` only as local readiness records; retain `rightkit-control` `tauri-plugin`, ORT runtime features & app-specific adapters.
- Acceptance: every declared RightKit pin resolves to chosen published record; no RightKit `path`, `git`, `file` or workspace spec; UI imports above remain functional; FCPXML hash/rename, engine restart & legal acceptance behavior remain unchanged; unused declarations are either wired with evidence or removed.

### GenRight — `/Volumes/D/claude/genright`

Targets include root web app, `src-tauri`, workspace crates `genright-*`, browser/e2e, macOS & Windows Tauri packaging: `/Volumes/D/claude/genright/package.json:1-14`, `/Volumes/D/claude/genright/src-tauri/Cargo.toml:1-10`.

Current npm pins: `app-shell 0.2.1`, `media-ui 0.2.6`, `onboarding 0.2.3`, `platform-ui 0.1.2`, `settings-ui 0.2.7`, `theme 0.2.1`: `/Volumes/D/claude/genright/package.json:56-62`; `release 0.2.135`: `/Volumes/D/claude/genright/package.json:75-76`. Only `theme 0.2.1` matches staged npm; `app-shell`, `media-ui`, `onboarding`, `platform-ui`, `settings-ui` & `release` mismatch staged `:22-37`. Cargo pins: settings `0.1.0`, process `0.3.0`, control `0.1.9` with `tauri-plugin` at `/Volumes/D/claude/genright/src-tauri/Cargo.toml:41-44`; QTS uses ORT `0.2.2`, TTS `0.1.0`, models `0.2.2`, qts `0.2.1`, qts-ggml/sys `0.1.0`, process `0.3.0` at `/Volumes/D/claude/genright/crates/genright-qts/Cargo.toml:29-39`; e2e uses QA/browser/process at `/Volumes/D/claude/genright/crates/genright-e2e/Cargo.toml:24-33`; studio uses media/models/fs at `/Volumes/D/claude/genright/crates/genright-studio/Cargo.toml:19-21`; gen uses db/jobs/secrets/ORT/process/fs at `/Volumes/D/claude/genright/crates/genright-gen/Cargo.toml:14-26`; local-image uses db/logs/models/jobs/process at `/Volumes/D/claude/genright/crates/genright-local-image/Cargo.toml:17-21`. Exact staged matches are settings `0.1.0`, ORT `0.2.2`, TTS `0.1.0`; listed process/control/models/qts/qts-ggml/sys/QA/browser/media/db/jobs/secrets/fs pins mismatch staged Cargo records. Staged records are readiness candidates, not verified publication: `/Volumes/D/claude/rightkit/packages/release/rightkit-versions.json:22-37`, `:274-319`.

Actual shared use: media-ui is imported by workflow/generation/motion/avatar/voice/gallery views, e.g. `/Volumes/D/claude/genright/src/views/PipelineWorkflowsPanel.tsx:10`, `/Volumes/D/claude/genright/src/views/Generate.tsx:10`, `/Volumes/D/claude/genright/src/views/Gallery.tsx:6-7`; onboarding/settings/platform/app-shell use is at `/Volumes/D/claude/genright/src/views/Onboarding.tsx:2-4`, `/Volumes/D/claude/genright/src/views/Settings.tsx:2-3`, `/Volumes/D/claude/genright/src/lib/platform.ts:2-15`, `/Volumes/D/claude/genright/src/components/DesktopWorkspace.tsx:2-3`; release scripts use CLI at `/Volumes/D/claude/genright/scripts/build-engine-for-tauri.mjs:4`. Rust use: models at `/Volumes/D/claude/genright/crates/genright-protocol/src/model_store.rs:8-44`; QA at `/Volumes/D/claude/genright/crates/genright-e2e/src/lib.rs:11-16`; media at `/Volumes/D/claude/genright/crates/genright-studio/src/ffmpeg.rs:26-80`; fs atomic writes at `/Volumes/D/claude/genright/crates/genright-studio/src/eyes.rs:82`; process/logs at `/Volumes/D/claude/genright/crates/genright-local-image/src/runtime.rs:13`, `:43-52`; secrets/jobs/db/ORT/fs at `/Volumes/D/claude/genright/crates/genright-gen/src/credentials.rs:27-47`, `/Volumes/D/claude/genright/crates/genright-gen/src/jobs.rs:9-40`, `/Volumes/D/claude/genright/crates/genright-gen/src/ledger.rs:6-7`, `/Volumes/D/claude/genright/crates/genright-gen/src/providers/local_lipsync.rs:41-42`; QTS/model/ORT/process/TTS at `/Volumes/D/claude/genright/crates/genright-qts/src/models.rs:69-70`, `:216-268`, `/Volumes/D/claude/genright/crates/genright-qts/src/client.rs:11`, `/Volumes/D/claude/genright/crates/genright-qts/src/lib.rs:16`.

Local overlap & boundary: `/Volumes/D/claude/genright/compat-lib/media-probe.mjs:30-42`, `:93-99` classifies product media paths/MIME & stats files; RightKit media owns verified FFmpeg/ffprobe resolution at `/Volumes/D/claude/rightkit/crates/rightkit-media/src/lib.rs:597-735`. GenRight local-image owns artifact Store rows, atomic publication & recovery at `/Volumes/D/claude/genright/crates/genright-local-image/src/artifacts.rs:101-112`, `:170-216`, `/Volumes/D/claude/genright/crates/genright-local-image/src/commands.rs:86-113`, `:270-301`; genright-gen owns provider/model catalog, job ledger, progress events & asset paths at `/Volumes/D/claude/genright/crates/genright-gen/src/api.rs:106-120`, `:179-200`, `:315-373`, while it already consumes RightKit db/jobs/fs/models. RightKit models provides hardware/profile/install primitives at `/Volumes/D/claude/rightkit/crates/rightkit-models/src/suite/install.rs:106-131`; RightKit db/jobs provide generic persistence/dispatch, but schema, idempotency, artifact manifests, provider catalog & recovery rules need body-level equivalence. GenRight also owns model residency/governor & QTS worker lifecycle at `/Volumes/D/claude/genright/crates/genright-studio/src/model_management/governor.rs:1-18`, `:79-97`, `/Volumes/D/claude/genright/crates/genright-qts/src/worker.rs:44-88`; only primitive/storage contract reuse is evidenced. GenRight's QTS modes, memory admission, media-console classification, recipes & workflow schema remain app-specific. No RightKit path/Git dependency was found.

Handoff checklist:

- Files: root `package.json`, `pnpm-workspace.yaml`, `src-tauri/Cargo.toml`, all `genright-*` manifests, release scripts & `compat-lib/media-probe.mjs` only if media boundary changes.
- Prerequisites: verify one coherent published npm/Cargo set; retain QTS `metal`/`vulkan`, ORT `session`/`half`, TTS client, QA browser & Tauri control features.
- Acceptance: all UI imports and Rust APIs resolve at selected pins; model install, JSONL logging, owned child lifecycle, jobs/ledger, QTS backends & media verification preserve behavior; product MIME/path classification remains separate; no path/Git RightKit specs.

### VoiceRight — `/Volumes/D/claude/voiceright`

Targets are Vite/Tauri web plus `src-tauri` native engine, with optional embedded QTS feature: `/Volumes/D/claude/voiceright/src-tauri/Cargo.toml:14-27`. Only RightKit pin is release CLI `0.2.109`: `/Volumes/D/claude/voiceright/package.json:48-50`; staged release is `0.2.147`, pending verified publication: `/Volumes/D/claude/rightkit/packages/release/rightkit-versions.json:32`.

No first-party `@rightkit/*` or `rightkit_*` API import was found. Native code directly owns ORT `2.0.0-rc.9` with `std`, `ndarray`, `tracing`, `load-dynamic`, `api-24`, plus local path QTS with `vulkan`: `/Volumes/D/claude/voiceright/src-tauri/Cargo.toml:15-16`; feature binds both at `:25-27`. Embedded synth/model load path is `/Volumes/D/claude/voiceright/src-tauri/src/engine/qts_embedded.rs:4-6`, `:72-120`; CLI process spawn is `/Volumes/D/claude/voiceright/src-tauri/src/engine/qts_cli.rs:25-29`. Web owns reference-audio staging/validation & generated speech playback at `/Volumes/D/claude/voiceright/src-tauri/src/commands.rs:61-68`, `/Volumes/D/claude/voiceright/src/App.tsx:131-134`, `:301`; settings, durable JSONL logs & shared model-store APIs have no first-party usage found. This overlaps RightKit QTS/QTS-GGML, ORT, TTS, process, models, logs, settings & media packages listed at `/Volumes/D/claude/rightkit/INDEX.md:39`, `:47-48`, `:55-57`, `:71`, `/Volumes/D/claude/rightkit/README.md:11-27`. Local path QTS and direct ORT are shared-boundary candidates; process wrapper, model artifact layout, voice prompt, synth selection, playback UI & embedded artifact policy need explicit equivalence proof before reuse.

Handoff checklist:

- Files: `package.json`, `src-tauri/Cargo.toml`, `src-tauri/src/engine/qts_embedded.rs`, `src-tauri/src/engine/qts_cli.rs`, `src-tauri/src/commands.rs`, local `eval/qts-upstream` boundary.
- Prerequisites: verify publication of one coherent `rightkit-qts`, `rightkit-qts-ggml`, `rightkit-ort`, `rightkit-tts`, `rightkit-models` & `rightkit-process` set; confirm Vulkan/Metal/CPU backend and model artifact compatibility before deleting local path QTS.
- Acceptance: release pin is changed only to verified published version; no local RightKit substitute remains for adopted APIs; embedded synth, voice prompt, model load, dynamic ORT, process launch & platform backend behavior match current product; local voice UX/artifact policy remains app-owned.

### WorkRight — `/Volumes/D/claude/workright`

Targets include protocol, crypto, local-store, capture-core, capture-windows, capture-macos, integrity, core-service, session-helper, test-vectors, backend/server, web `apps/agent-ui`, web `apps/dashboard` & Tauri agent UI: `/Volumes/D/claude/workright/Cargo.toml:3-15`, `/Volumes/D/claude/workright/apps/agent-ui/package.json:1-24`, `/Volumes/D/claude/workright/apps/dashboard/package.json:1-25`.

No RightKit npm/Cargo/Swift pin or first-party API import was found. `local-store` owns SQLite/WAL, SQLCipher keying & keyring-derived encryption at `/Volumes/D/claude/workright/crates/local-store/src/lib.rs:1-17`, `:156-186`; it persists event log, screenshot spool, outbox, policy cache, service state & consent at `:273-313`, `:337-405`, `:425-505`. RightKit db owns SQLite/SQLCipher bootstrap at `/Volumes/D/claude/rightkit/INDEX.md:33`, `/Volumes/D/claude/rightkit/crates/rightkit-db/src/lib.rs:119-210`, while RightKit secrets owns OS credentials at `/Volumes/D/claude/rightkit/INDEX.md:63`. WorkRight reads OS-keyring-derived material in core-service at `/Volumes/D/claude/workright/crates/core-service/src/main.rs:129`, `:207`; this is a candidate API match only: preserve WorkRight schema, key possession, retention & recovery policy. WorkRight has policy/config state but no standalone settings package: `/Volumes/D/claude/workright/crates/core-service/src/main.rs:131-163`, `/Volumes/D/claude/workright/crates/local-store/src/lib.rs:124-149`; RightKit settings provides versioned store/secret refs at `/Volumes/D/claude/rightkit/crates/rightkit-settings/src/lib.rs:1-9`, so equivalence is unknown. WorkRight logs service files at `/Volumes/D/claude/workright/crates/core-service/src/lib.rs:241-243` & logs helper handshake at `/Volumes/D/claude/workright/crates/session-helper/src/main.rs:1`; RightKit JSONL append/locking is `/Volumes/D/claude/rightkit/crates/rightkit-logs/src/store.rs:9-39`, so event schema/rotation equivalence is unknown. WorkRight owns multi-process service/session lifecycle & `sc.exe` command text at `/Volumes/D/claude/workright/crates/core-service/src/lib.rs:127-129`, `:204`; RightKit process owns containment/hidden Windows child defaults at `/Volumes/D/claude/rightkit/crates/rightkit-process/src/owned.rs:78-120`, so service install/privilege policy remains WorkRight-specific. `capture-macos` uses CoreGraphics at `/Volumes/D/claude/workright/crates/capture-macos/Cargo.toml:1-3`, `:14-28`; `capture-windows` uses Windows hooks at `/Volumes/D/claude/workright/crates/capture-windows/Cargo.toml:1-3`, `:14-28`. RightKit automation offers macOS AX/Windows UIA, screenshot & capture primitives at `/Volumes/D/claude/rightkit/crates/rightkit-automation/src/lib.rs:6-10`, `/Volumes/D/claude/rightkit/crates/rightkit-automation/src/engine.rs:538-545`, `/Volumes/D/claude/rightkit/crates/rightkit-automation/src/platform.rs:57`; disclosure, consent, capture policy & event model remain WorkRight-specific. No RightKit path/Git dependency exists.

Handoff checklist:

- Files: `crates/local-store`, `crates/core-service`, `crates/session-helper`, `capture-macos`, `capture-windows`, root workspace manifest & agent UI Tauri manifest.
- Prerequisites: resolve rustc compatibility with `rightkit-db`; verify published coherent versions before adding crates; map keyring/secret boundaries; compare settings, JSONL log & process contracts; prove automation capture semantics on macOS & Windows before replacing adapters.
- Acceptance: no schema/key/capture-policy drift; SQLCipher/WAL recovery, event/outbox/consent durability, OS permission behavior, service launch & child containment remain intact; each adopted crate is published/version-pinned with no path/Git spec; product crypto/integrity/service/server code remains separate.

### Pulse — `/Volumes/D/claude/pulse`

Targets include `core` Rust, `hub` web/Tauri, `hub/qa-e2e`, mac Swift package & Windows Rust target: `/Volumes/D/claude/pulse/package.json:17-24`, `/Volumes/D/claude/pulse/hub/src-tauri/Cargo.toml:1-42`, `/Volumes/D/claude/pulse/core/Cargo.toml:1-31`.

Current pins: root release `0.2.141`: `/Volumes/D/claude/pulse/package.json:23-24`; hub app-shell `0.2.1`, platform-ui `0.1.2`: `/Volumes/D/claude/pulse/hub/package.json:11-13`; Tauri control `0.1.11` with `tauri-plugin`/`wv2-0_39`, mac fsindex `0.1.0`: `/Volumes/D/claude/pulse/hub/src-tauri/Cargo.toml:21-27`, `:35-38`; QA `0.2.12`: `/Volumes/D/claude/pulse/hub/qa-e2e/Cargo.toml:7-14`. Against staged records, control `0.1.11` matches; release, app-shell, platform-ui & QA mismatch. `fsindex` has no staged comparison in requested records. Evidence: `/Volumes/D/claude/rightkit/packages/release/rightkit-versions.json:29-37`, `:274-319`.

Actual use: app-shell appears in hub App/main/views at `/Volumes/D/claude/pulse/hub/src/App.tsx:5`, `/Volumes/D/claude/pulse/hub/src/main.tsx:3`, `/Volumes/D/claude/pulse/hub/src/views/Cleanup.tsx:2`; Tauri control is embedded at `/Volumes/D/claude/pulse/hub/src-tauri/src/lib.rs:479`; fsindex is used by disk indexing at `/Volumes/D/claude/pulse/hub/src-tauri/src/disk_index.rs:1`, `:69`; QA is used by UI tests at `/Volumes/D/claude/pulse/hub/qa-e2e/tests/ui.rs:31-33`, `:1605-1612`. Platform-ui is declared but no first-party import was found. Pulse core intentionally uses direct rusqlite because its rustc floor is `1.90` while RightKit db requires `1.98`: `/Volumes/D/claude/pulse/core/Cargo.toml:24-31`; do not replace this without toolchain decision. mac package has no RightKit Swift dependency: `/Volumes/D/claude/pulse/mac/Package.swift:1-15`; Windows manifest has no RightKit crate: `/Volumes/D/claude/pulse/windows/Cargo.toml:1-29`.

Handoff checklist:

- Files: root/hub package manifests, hub Tauri manifest, QA manifest, `core/Cargo.toml`, `mac/Package.swift`, `windows/Cargo.toml`.
- Prerequisites: verify one coherent published npm/Cargo set; preserve `rightkit-control` target features & fsindex behavior; decide whether platform-ui is needed; resolve rustc floor before db adoption.
- Acceptance: hub shell, control, mac indexing & QA-native behavior remain intact; no unsupported core db swap; unused platform-ui is wired or removed; no path/Git RightKit specs.

### SwiftRight — `/Volumes/D/claude/swiftright`

Targets include BridgeGeoModal, BridgeSmoke, SliceApp, `SwiftRightBridge`, `SwiftRightServices`, `sr0-*`/`sr1-*` probes, Rust renderer, SliceApp macOS & Windows dirs: `/Volumes/D/claude/swiftright/examples/BridgeSmoke/Package.swift:13-23`, `/Volumes/D/claude/swiftright/examples/SliceApp/Package.swift:13-28`.

No RightKitSwift URL, package product or import exists in first-party Package.swift/source. SliceApp only pins release CLI `0.2.146`: `/Volumes/D/claude/swiftright/examples/SliceApp/package.json:5-9`; staged release is `0.2.147`: `/Volumes/D/claude/rightkit/packages/release/rightkit-versions.json:32`. RightKitSwift adopted record is URL `https://github.com/bogusyogi/rightkit-swift.git`, version `0.1.1`; staged record is `0.3.1`: `/Volumes/D/claude/rightkit/packages/release/rightkit-versions.json:321-331`, while source `VERSION` is `0.3.8`: `/Volumes/D/claude/rightkit/swift/RightKitSwift/VERSION:1`; record/source drift must be resolved before adoption. RightKitSwift products/targets are defined at `/Volumes/D/claude/rightkit/swift/RightKitSwift/Package.swift:7-67`.

SwiftRightBridge is intentionally local at `/Volumes/D/claude/swiftright/packages/SwiftRightBridge/Package.swift:15-33`; SwiftRightServices is Foundation-only and implements the framed-sidecar wire envelope at `/Volumes/D/claude/swiftright/packages/SwiftRightServices/Package.swift:4-23`. RightKit has a corresponding framed-sidecar record (`0.1.0`) at `/Volumes/D/claude/rightkit/packages/release/rightkit-versions.json:257-286`; protocol compatibility does not make local bridge/service code replaceable. Renderer ABI, bridge, service actor, probes & SliceApp behavior remain app-specific. No RightKit path/Git dependency was found.

Handoff checklist:

- Files: `examples/SliceApp/package.json`, all first-party Package.swift files, `SwiftRightBridge`, `SwiftRightServices`, renderer/probe manifests.
- Prerequisites: reconcile RightKitSwift record `0.3.1` with source `0.3.8`; choose immutable URL/tag; map each Apple target to exact RightKitSwift products; decide whether framed-sidecar adoption is wire-only or package-level.
- Acceptance: release pin is changed only to verified published version; Apple targets compile against exact Swift tag when adopted; Windows conditional targets retain behavior; local bridge/service/probe/renderer ABI and app-specific UI remain intact; no unpinned Git/path RightKit dependency.

### Cross-product handoff order

1. Record chosen published npm/Cargo/Swift versions in each app manifest & lockfile policy; source of truth is `/Volumes/D/claude/rightkit/packages/release/rightkit-versions.json:15-37`, `:274-331`.
2. Remove obsolete app pins only after import/use evidence above passes; keep app-specific policy, schemas, UX, provider adapters, capture consent, engine protocols, renderer ABI & artifact rules in app repositories.
3. For each adopted package, acceptance requires published resolution, no `path`/`git`/`file`/workspace RightKit spec, target-qualified native behavior, & unchanged app-specific behavior. This inventory intentionally does not authorize builds/tests or publication.


## Tooling, web/mobile products, retained & absent checkouts

Scope: current source only, bounded local scan on 2026-10-10. No build, test, install, network, commit, or source edits run. Paths below are absolute. “Keep local” means product semantics stay in app; shared capability should consume published RightKit.

### Version evidence used for comparison

`/Volumes/D/claude/rightkit/packages/release/rightkit-versions.json:4-13` is adopted npm baseline, not proof of registry publication. `stagedNpm` at `/Volumes/D/claude/rightkit/packages/release/rightkit-versions.json:15-37` is staged work; `registry.npm` is recorded registry inventory at `/Volumes/D/claude/rightkit/packages/release/rightkit-versions.json:377-400`; this report makes no npm publication claim. Relevant recorded values: baseline `@rightkit/release 0.2.47`; staged `@rightkit/release 0.2.147`, `@rightkit/ax 0.2.4`, `@rightkit/hooks 0.1.2`, `@rightkit/legal 0.3.1`, `@rightkit/app-shell 0.3.0`, `@rightkit/platform-ui 0.2.0`, `@rightkit/qa 0.4.0`.

Swift sections record URL plus adopted exact version `0.1.1` & staged version `0.3.1` at `/Volumes/D/claude/rightkit/packages/release/rightkit-versions.json:321-331`. README’s `0.3.8` example is explicitly illustrative at `/Volumes/D/claude/rightkit/swift/RightKitSwift/README.md:181-190`; it is not verified as published here. Handoffs below were written against recorded adopted `0.1.1`. **Version truth (merged; tag listing from the static pass via `gh api`, not re-run):** tags on `bogusyogi/rightkit-swift` are `0.1.0`, `0.1.1` and `0.3.0` through `0.3.7`; source `0.3.8` is unpublished; the feed is stale at adopted `0.1.1` / staged `0.3.1`. Tag availability for `0.1.1` and `0.3.x` is therefore established, and the earlier caveat about confirming tag existence applies only to `0.3.8`.

### Named products

#### Legion — desktop CLI/backend/release tooling

Evidence:

- Exact npm pins: `/Volumes/D/claude/legion/package.json:104-110` has runtime `@rightkit/hooks 0.1.2`; dev `@rightkit/ax 0.2.4`, `@rightkit/git 0.2.33`, `@rightkit/release 0.2.147`.
- External release bridge invokes RightKit at `/Volumes/D/claude/legion/engine/xtask/src/rightkit_release_bridge.rs:66-97`.
- Local duplicate AX behavior is explicit: `/Volumes/D/claude/legion/engine/xtask/src/portable_core.rs:1-7` calls itself Rust port of `@rightkit/ax/plugin/portable-core.mjs`.
- Runtime metadata lags package pin: `/Volumes/D/claude/legion/engine/bins/legion/src/cli.rs:173-177` hard-codes AX `0.2.1` plus source commit, versus package `0.2.4`.

Keep local Legion policy, Rust orchestration, & release composition. Handoff: reconcile hard-coded AX metadata with manifest `0.2.4`; confirm `@rightkit/hooks 0.1.2` is accepted by `/Volumes/D/claude/legion/pnpm-workspace.yaml:2-5` (allowlist contains older hooks); retain bridge calls; do not grow another AX implementation. Desktop-only package additions are not required.

#### RustRight — backend/native process broker

Evidence:

- Cargo declarations: `/Volumes/D/claude/rustright/Cargo.toml:22,37-39` has exact `rightkit-ownership = "=0.2.0"`, but caret ranges `rightkit-fs = "0.3.2"` & `rightkit-process = "0.3.1"`, plus explicit Windows feature list. Lock resolves `rightkit-fs 0.3.2` & `rightkit-process 0.3.1` at `/Volumes/D/claude/rustright/Cargo.lock:837-873`.
- Actual ownership imports are used by broker admission at `/Volumes/D/claude/rustright/crates/rustright-broker/src/job_service/assemble.rs:26-30`.
- Product-owned broker/scheduler/cache crates remain local in `/Volumes/D/claude/rustright/Cargo.toml:18-26`.

Keep local broker, scheduler, state, cache, service, install, & parity behavior. Handoff: exact-pin `rightkit-fs` & `rightkit-process` to satisfy RightKit contract, or document approved exception; keep ownership exact; verify package updates against current Windows feature surface. No web, mobile, or desktop UI dependency should be forced.

#### SellRight — website/backend + iOS mobile

Evidence:

- Backend exact pin `@rightkit/legal 0.3.1` at `/Volumes/D/claude/sellright/packages/api/package.json:27-31`.
- Actual backend imports are in `/Volumes/D/claude/sellright/packages/api/src/legal/manifests.ts:19-24` & `/Volumes/D/claude/sellright/packages/api/src/legal/acceptance.ts:1-8`.
- iOS uses RightKitSwift by URL but range: `/Volumes/D/claude/sellright/ios/project.yml:16-20` says URL plus `from: 0.1.0`; target consumes `RightKitDiagnostics` at `:27-30`.

Keep storefront, commerce, & legal acceptance adapter local. Handoff: replace mobile range with an exact existing Swift tag (`0.1.1` is the recorded adopted baseline; tags through `0.3.7` exist, and the static pass recommends exact `0.3.7`); retain only Legal + Swift diagnostics adoption. Classify as web/backend/mobile; do not add desktop UI packages.

#### RightSites — website/backend + iOS mobile

Evidence:

- Backend exact `@rightkit/legal 0.3.1` at `/Volumes/D/claude/rightsites/packages/api/package.json:27-32`; source imports mirror SellRight at `/Volumes/D/claude/rightsites/packages/api/src/legal/manifests.ts:19-24`.
- iOS repeats URL range `from: 0.1.0` and `RightKitDiagnostics` at `/Volumes/D/claude/rightsites/ios/project.yml:16-30`.
- Workspace exception list is stale: `/Volumes/D/claude/rightsites/pnpm-workspace.yaml:54-56` lists old release/legal versions, while package consumes legal `0.3.1`.

Keep site catalog, storefront, theming, & acceptance adapter local. Handoff: replace Swift range with an exact existing tag (`0.1.1` recorded baseline; tags through `0.3.7` exist, the static pass recommends `0.3.7`); reconcile/remove stale workspace exceptions; keep Legal + diagnostics only. No desktop dependency.

#### PostRight — docs-only checkout, no current app source

Bounded source scan found no `package.json`, `Cargo.toml`, Swift project, or app imports under `/Volumes/D/claude/postright`; only docs plus ignore metadata are present (`/Volumes/D/claude/postright/.gitignore:1-5`). No exact pin or actual import can be established from current source. Keep verdict: adoption unmaterialized; reassess when implementation checkout exists. No desktop, web, backend, or mobile RightKit dependency should be inferred from docs.

#### Xtool — native desktop + backend/headless MCP bridge

`/Volumes/D/claude/xtool/package.json:31-54` has no `@rightkit/*` dependency. Xtool is Node-hosted, with local generic infrastructure:

- Logging: `/Volumes/D/claude/xtool/src/main/logger.ts:1-24,63-90` owns an in-memory ring, redacted file mirror, & agent attribution. `@rightkit/logs` is a webview client that batches through injected Tauri `invoke` commands at `/Volumes/D/claude/rightkit/packages/logs/README.md:1-20` & `/Volumes/D/claude/rightkit/packages/logs/src/index.ts:17-45,152-176`. No compatible Xtool Tauri/native command seam is present; keep Xtool logger local.
- Durable state: `/Volumes/D/claude/xtool/src/main/durable.ts:1-18,94-124` atomically renames debounced JSON snapshots. `rightkit-fs` provides Rust descriptor-pinned `StateDir`, `fsync` writes, locks, & owner-only paths at `/Volumes/D/claude/rightkit/crates/rightkit-fs/README.md:3-9,22-31`. API semantics overlap, but Xtool has no Rust host boundary; keep Node durable store local unless native binding work is intentionally opened.
- Secrets: Xtool Keychain calls `/usr/bin/security` & AES-GCM at `/Volumes/D/claude/xtool/src/main/secrets-keychain.ts:1-15,31-80`; Windows DPAPI uses a PowerShell child at `/Volumes/D/claude/xtool/src/main/secrets-dpapi.ts:1-22,40-54`. `rightkit-secrets` exposes Rust `Store`/`Namespace`, macOS Keychain, Windows Credential Manager, an Apple helper, & Linux fail-closed behavior at `/Volumes/D/claude/rightkit/crates/rightkit-secrets/README.md:1-21` & `/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/lib.rs:1-34`. Xtool’s Node codecs, legacy service identity, & cross-platform fallback do not share this API; keep local, with migration requiring native bridge plus data migration.
- Process execution: Xtool shell-free `runCommand`/`launchCommand` uses Node `spawn`, explicit env filtering, timeouts, & detached POSIX groups at `/Volumes/D/claude/xtool/src/main/exec.ts:1-10,67-74,545-591`. `rightkit-process` owns Rust child-tree lifetime, restart tracking, health thresholds, Unix groups, & Windows Job Objects at `/Volumes/D/claude/rightkit/crates/rightkit-process/README.md:3-20,24-31`. This is partial lifecycle overlap; Xtool’s Node process policy & command protocol remain product-local, with no direct package substitution.
- Filesystem sandbox: `/Volumes/D/claude/xtool/src/main/sandbox.ts:1-17,171-197,291-312` canonicalizes approved roots, rejects links escaping roots, & maps virtual paths. `rightkit-fs` covers descriptor-relative state/locks rather than Xtool’s model-facing virtual-root protocol at `/Volumes/D/claude/rightkit/crates/rightkit-fs/README.md:3-9,33-38`; keep Xtool sandbox local.
- Desktop helper: Windows UIA/SendInput is embedded PowerShell in `/Volumes/D/claude/xtool/src/main/computer/helper.ts:1-31,63-83`; Node selects Windows PowerShell or macOS Swift backend over one JSON protocol at `/Volumes/D/claude/xtool/src/main/computer/index.ts:173-179,321-341`, with Swift helper sources importing AppKit/ApplicationServices/ScreenCaptureKit at `/Volumes/D/claude/xtool/native/macos-desktop-helper/main.swift:1-10`. `rightkit-automation` is an in-process Rust AX/UIA ladder with policy, leases, receipts, & no Node runtime at `/Volumes/D/claude/rightkit/crates/rightkit-automation/README.md:1-25,27-36`. This is substantial native-helper overlap; possible binding/migration is larger project work, not required for current adoption.
- RightKitSwift: package offers Apple targets such as Diagnostics, AppleCredentials, UI, Store, & Intents at `/Volumes/D/claude/rightkit/swift/RightKitSwift/Package.swift:4-24`; Xtool has no Swift package dependency or `RightKitSwift` import. Its macOS helper is standalone AppKit/ScreenCaptureKit source, so no direct Swift target compatibility is evidenced. Keep helper local; reassess only if Xtool intentionally adopts an Apple package boundary.

Keep Xtool MCP, PTY, session, connector, permission, & native-helper protocol semantics local. Handoff checklist: no forced `@rightkit/logs`, Rust fs/process/secrets/automation, or RightKitSwift dependency; consider only targeted native-bridge projects after API/packaging decisions. Classify Xtool as native desktop + backend/headless infrastructure, not website or mobile.

### Additional first-party checkouts

#### Membrane — Tauri desktop, three duplicate worktrees

Canonical `/Volumes/D/claude/membrane` is absent. These are linked worktree copies, not three products: `/Volumes/D/worktrees/membrane-main-f602-baseline`, `/Volumes/D/worktrees/membrane-blueprint-findings`, `/Volumes/D/worktrees/membrane-live-diagnostics-phase1-repair`; root, hub manifest, & lock hashes match across all three (`96218bc…`, `126b6e…`, `fea592…`).

Evidence from baseline (same content in copies):

- Root pins `@rightkit/hooks 0.1.0`, `@rightkit/ax 0.1.1`, `@rightkit/git 0.2.0` at `/Volumes/D/worktrees/membrane-main-f602-baseline/package.json:29-35`.
- Hub pins `@rightkit/platform-ui 0.1.0` & `@rightkit/release 0.2.67` at `/Volumes/D/worktrees/membrane-main-f602-baseline/apps/membrane-hub/package.json:16-23`.
- Actual platform UI import is `/Volumes/D/worktrees/membrane-main-f602-baseline/apps/membrane-hub/src/popover.mjs:87`; release bridge imports `@rightkit/release/notary-auth.mjs` at `/Volumes/D/worktrees/membrane-main-f602-baseline/apps/membrane-hub/scripts/build-mac-release.mjs:17`.

Keep Membrane engine, Blueprint/Cortex, hub semantics, & Tauri wiring local. Handoff: restore/select one canonical checkout before change; move hooks/AX/git/platform-ui/release to verified package pins only after publication proof & compatibility review. No RightKit Rust UI/process dependency is evidenced; do not force one.

#### ScreenRight — archived desktop checkout only

Only `/Volumes/D/claude/archive/screenright-2026-10-08` exists; no current canonical checkout. Evidence:

- Archived manifest pins `@rightkit/release 0.2.66` at `/Volumes/D/claude/archive/screenright-2026-10-08/package.json:32-34`.
- Current archive test expects release `0.2.61`, conflicting with manifest, at `/Volumes/D/claude/archive/screenright-2026-10-08/test/release-pipeline.test.mjs:9-15`.
- Local diagnostics duplicate RightKit-like behavior at `/Volumes/D/claude/archive/screenright-2026-10-08/menu/RightKitDiagnostics.swift:3-7`; controller calls it at `:382,532,539,555`.
- Archive test explicitly rejects RightKit QA Cargo dependency at `/Volumes/D/claude/archive/screenright-2026-10-08/test/boundary.test.mjs:13`; `src-tauri/Cargo.toml:1-5` has no RightKit crate.

Keep archive untouched. If resurrected, handoff: reconcile release version/test drift; migrate local diagnostics to RightKit logs/Swift diagnostics; use exact published release policy. No current app adoption action.

#### LightCraft — native/browser Rust workflow app

`/Volumes/D/claude/workflows/lightcraft/README.md:1-8` describes pure-Rust photo/raw tooling for macOS, Windows, Linux, & browser WASM. Bounded source search found no RightKit manifest or import. Keep image/raw pipeline local; evaluate release/QA only when shipping integration exists. Browser target stays separate from desktop packages.

#### FreeCAD — third-party fork/reference checkout

`/Volumes/D/claude/workflows/freecad/README.md:1-8` identifies FreeCAD fork/upstream 3D modeler. No RightKit manifest/import found. Classify as third-party fork/reference, exclude from Right Suite adoption universe, keep local.

### Discovery, duplicates, exclusions

- First-party roots inspected: named `/Volumes/D/claude/{legion,rustright,sellright,rightsites,postright,xtool}`, plus `/Volumes/D/worktrees/membrane-*`, `/Volumes/D/claude/archive/screenright-2026-10-08`, `/Volumes/D/claude/workflows/{lightcraft,freecad}`. Pulse is covered by separate apps-new evidence.
- Membrane worktrees are duplicate content; absence of canonical `/Volumes/D/claude/membrane` blocks canonical handoff.
- Citadel source checkout was not found. Only asset `/Volumes/D/claude/orthic-labs-hero-art/citadel/hero-illustrate-v1.png`, stale submodule metadata `/Volumes/D/claude/.git/modules/citadel`, & docs-only plan directory exist; no adoption evidence.
- ScreenRight is archive-only. Duplicate SwiftRight/build artifacts under `/Volumes/D/sr0-build` are not independent products; use source checkout identity from machine inventory, not artifact copies.
- Excluded bounded matches: `/Volumes/D/claude/repos/membrane/*` research/reference clones; `/Volumes/D/claude/apple-skill-sources/*` third-party skill sources; `/Volumes/D/claude/vendor/*`; caches, `node_modules`, `target`, `dist`, `build`, generated reports, & docs-only plans. These are not app adoption evidence.


## Every RightKit package/crate: direct-consumer matrix

This matrix is manifest-level only. “None found” can mean transitive consumption, private/internal intent, helper invocation, future product work or no applicable app; it is not dead-code evidence. Three Membrane copies collapse to one product here.

| Package/crate | Source version | Direct declaration consumers |
|---|---|---|
| `@rightkit/agent-room-mcp` | `0.1.0` | None found in scoped npm/Cargo manifests |
| `@rightkit/app-shell` | `0.3.0` | cutright, genright, heardright, pulse, scraperight |
| `@rightkit/ax` | `0.2.4` | coderight, legion, membrane (retained) |
| `@rightkit/build-control-policy` | `0.1.0-phase1` | None found in scoped npm/Cargo manifests |
| `@rightkit/doc-render` | `0.1.4` | viewright |
| `@rightkit/git` | `0.2.33` | coderight, legion, membrane (retained) |
| `@rightkit/hooks` | `0.1.2` | coderight, legion, membrane (retained) |
| `@rightkit/i18n` | `0.1.0` | None found in scoped npm/Cargo manifests |
| `@rightkit/legal` | `0.3.1` | coderight, cutright, heardright, mailright, rightsites, scraperight, sellright, viewright |
| `@rightkit/legal-ui` | `0.2.0` | cutright, heardright, scraperight, viewright |
| `@rightkit/license` | `0.1.8` | coderight, cutright, heardright, mailright, scraperight, viewright |
| `@rightkit/logs` | `0.1.4` | coderight, cutright, heardright, mailright, scraperight, viewright |
| `@rightkit/media-ui` | `0.3.0` | cutright, genright, scraperight, viewright |
| `@rightkit/onboarding` | `0.3.0` | cutright, genright, heardright, scraperight |
| `@rightkit/pdf-ui` | `0.1.2` | viewright |
| `@rightkit/platform-ui` | `0.2.0` | cutright, genright, membrane (retained), pulse, scraperight |
| `@rightkit/qa` | `0.4.0` | None found in scoped npm/Cargo manifests |
| `@rightkit/qa-chrome` | `0.1.3` | heardright, scraperight, viewright |
| `@rightkit/release` | `0.2.147` | coderight, cutright, genright, heardright, legion, mailright, membrane (retained), pulse, scraperight, swiftright, viewright, voiceright |
| `@rightkit/settings-ui` | `0.3.0` | cutright, genright, heardright, scraperight |
| `@rightkit/shell` | `0.1.0` | coderight, cutright |
| `@rightkit/tauri` | `0.2.0` | coderight, heardright, mailright, scraperight, viewright |
| `@rightkit/theme` | `0.2.1` | cutright, genright, heardright, scraperight |
| `@rightkit/updates` | `0.2.5` | coderight, cutright, heardright, mailright, scraperight, viewright |
| `agent-room-core` | `0.3.0` | None found in scoped npm/Cargo manifests |
| `rightkit-agent-tools` | `0.1.2` | None found in scoped npm/Cargo manifests |
| `rightkit-align` | `0.0.0` | None found in scoped npm/Cargo manifests |
| `rightkit-asr` | `0.0.2` | heardright |
| `rightkit-audio` | `0.2.0` | cutright, heardright |
| `rightkit-automation` | `0.1.0` | None found in scoped npm/Cargo manifests |
| `rightkit-browser` | `0.2.3` | coderight, genright |
| `rightkit-cache` | `0.1.0` | coderight, mailright |
| `rightkit-channel-memory` | `0.1.0` | None found in scoped npm/Cargo manifests |
| `rightkit-compose` | `0.0.0` | None found in scoped npm/Cargo manifests |
| `rightkit-contracts` | `0.1.2` | None found in scoped npm/Cargo manifests |
| `rightkit-control` | `0.1.11` | cutright, genright, pulse |
| `rightkit-crash` | `0.2.0` | heardright |
| `rightkit-daemon` | `0.1.0` | None found in scoped npm/Cargo manifests |
| `rightkit-db` | `0.3.2` | coderight, genright, mailright, scraperight, viewright |
| `rightkit-embed` | `0.3.1` | coderight, mailright |
| `rightkit-framed-sidecar` | `0.1.0` | scraperight |
| `rightkit-fs` | `0.3.3` | coderight, cutright, genright, rustright, scraperight |
| `rightkit-fsindex` | `0.1.0` | pulse |
| `rightkit-hotkeys` | `0.1.0` | None found in scoped npm/Cargo manifests |
| `rightkit-http` | `0.2.4` | coderight, mailright, viewright |
| `rightkit-jobs` | `0.2.6` | cutright, genright |
| `rightkit-license` | `0.1.3` | coderight, heardright, mailright, scraperight, viewright |
| `rightkit-llm` | `0.1.0` | None found in scoped npm/Cargo manifests |
| `rightkit-logs` | `0.2.0` | coderight, genright, heardright, mailright, scraperight, viewright |
| `rightkit-mcp` | `0.5.2` | coderight |
| `rightkit-media` | `0.2.4` | genright |
| `rightkit-models` | `0.2.7` | cutright, genright, heardright, scraperight |
| `rightkit-oauth` | `0.3.0` | coderight, mailright |
| `rightkit-ocr` | `0.2.0` | coderight, scraperight, viewright |
| `rightkit-ort` | `0.2.2` | coderight, cutright, genright, scraperight |
| `rightkit-overlay` | `0.1.0` | None found in scoped npm/Cargo manifests |
| `rightkit-ownership` | `0.2.1` | rustright |
| `rightkit-pdf` | `0.2.0` | None found in scoped npm/Cargo manifests |
| `rightkit-playback` | `0.1.1` | None found in scoped npm/Cargo manifests |
| `rightkit-process` | `0.3.3` | coderight, cutright, genright, heardright, rustright |
| `rightkit-qa` | `0.2.14` | cutright, genright, pulse |
| `rightkit-qts` | `0.2.4` | genright |
| `rightkit-qts-ggml` | `0.1.1` | genright |
| `rightkit-qts-ggml-sys` | `0.1.1` | genright |
| `rightkit-record` | `0.1.1` | None found in scoped npm/Cargo manifests |
| `rightkit-review` | `0.1.0` | None found in scoped npm/Cargo manifests |
| `rightkit-sandbox` | `0.3.3` | coderight |
| `rightkit-scenario` | `0.0.0` | None found in scoped npm/Cargo manifests |
| `rightkit-search` | `0.2.0` | None found in scoped npm/Cargo manifests |
| `rightkit-secrets` | `0.1.2` | coderight, cutright, genright, mailright, scraperight, viewright |
| `rightkit-service` | `0.1.3` | heardright |
| `rightkit-settings` | `0.1.0` | genright |
| `rightkit-shell` | `0.1.4` | coderight, mailright, viewright |
| `rightkit-speech-client` | `0.3.2` | cutright, heardright |
| `rightkit-sync` | `0.1.0` | None found in scoped npm/Cargo manifests |
| `rightkit-tauri` | `0.1.2` | coderight, heardright, mailright, scraperight, viewright |
| `rightkit-transcript` | `0.1.0` | None found in scoped npm/Cargo manifests |
| `rightkit-tts` | `0.1.0` | genright |
| `rightkit-vad` | `0.0.2` | None found in scoped npm/Cargo manifests |
| `rightkit-watch` | `0.3.1` | coderight, viewright |

## Static-pass app inventory, handover lists, gaps & blockers (from static pass, unverified by build)

Source: the earlier static pass over every product repository on this Mac, merged here. It covered CodeRight, CutRight, GenRight, HeardRight, MailRight, PostRight, Pulse, ScrapeRight, SwiftRight, ViewRight, VoiceRight, WorkRight, plus SellRight, RightSites, xtool and workflows/lightcraft (16 repositories; excluded: `heardright-release-0.1.167`, legion, rustright, rightkit). Method there: manifests and pins against `packages/release/rightkit-versions.json`, grep of in-house code against the CAPABILITY-MAP owner list, and `right-release ownership --app` per repo. **The ownership scanner refused 11 of 16 repos with `protected-evidence` (dirty checkouts)**; where it refused, the pass emulated the fingerprint list by regex and the per-app file says so. LOC figures are approximate and nothing was built. Full per-app tables with path:line and the scanner JSON are in [apps-g1..g5.md](/Volumes/D/claude/rightkit/docs/audits/2026-10-10-audit-evidence/). B's per-app sections above stand where they overlap; B states few LOC figures, and no case was found where B and A give different numbers for the same item (see footnotes below the summary table for differences of counting basis).

### Summary table (from static pass, unverified by build)

| App | In-house duplicates | LOC to move | Stale pins | Block findings | Blocked on RightKit gaps |
|---|---|---|---|---|---|
| CodeRight | 18 | ~24,000 (~16,000 movable now) | 26 (8 breaking minors) [1] | scanner refused | sandbox tiers/PTY (~7,800 LOC); updater; overlay, llm unpublished |
| ScrapeRight | 12 | ~15,400 | 27 (db, logs, 7 UI pkgs breaking) [1] | 2 (`puppeteer-core`; `ort` rc.12 at HEAD) | sync unpublished (2,518 LOC) |
| HeardRight | 14 (+5 blocked) | ~11,100 (+16,300 blocked) | 14 (6 UI minors) | 0 (180 advise) | hotkeys, overlay, llm, sync unpublished; automation private; speech host side; KWS |
| ViewRight | 13 | ~10,700 | 12 of 20 | scanner refused | native PDFKit view, file associations, updater |
| CutRight | 15 | ~10,100 | 18 | 0 (emulated) | review, llm, contracts, compose unpublished; GPU compositor; audio output; CoreML Silero |
| Pulse | 13 | ~8,400 [3] | 4 of 6 [2] | 0 (73 advise) | Rust 1.90 to 1.98 first; native updater; Windows HTTP backend; Swift SQLite/hotkeys/AX |
| MailRight | 14 | ~6,700 | 9 of 14 | 0 (5 advise) | llm unpublished |
| GenRight | 13 | ~4,500 (~5,800 with root-stack files) | 18 | 0 (emulated) | contracts, align, review, channel-memory, record unpublished; legal pages |
| VoiceRight | 8 | ~1,510 | 1 | 0 | `qts` path dep to a missing `../eval/qts-upstream` (broken input) |
| SwiftRight | 2 (+1) | ~1,200 (+1,202 engine client) | 1 | scanner refused | no Rust C ABI; RightKitSwift Windows platform (A listed it as a gap; not needed, see below) |
| WorkRight | 5 | ~900 of 7,000 | 0 (no pins) | scanner refused | its own release doctor bans `@rightkit/release`; hotkeys, record unpublished; no OS-service registration |
| PostRight | 0 | 0 (plan-only, 7 files) | 0 | 0 | daemon, llm, RK0/5/6/8 unpublished; RK7; Linux secrets |
| RightSites (infra) | ~13 | ~2,000 | release <=0.2.92, legal <=0.2.0 | emulated | server-side license owner (~6,000 LOC); unknown provider `r2-rightsites` |
| SellRight | 7 | ~1,250 | RightKitSwift `from: 0.1.0` | 1 (`@playwright/test`) | `@rightkit/release` has no `exports` map; legal verifier |
| xtool | 7 areas | ~8,000 (Rust-owned, no npm swap) | none; pnpm 11.18 | not registered | no Node bindings for control/secrets |
| lightcraft | 5 | ~3,000 | none | not registered | broken `release-candidate.yml` (no package.json) |

Totals (A): roughly 110,000 LOC of in-app code duplicates a RightKit owner; about 70,000 of it can move today; the rest waits on the gaps below. Stale-pin count across apps: ~130 manifest lines.

Already done, contrary to the 10-08 inventory (A): CodeRight `coderight-cache`, `coderight-ocr`, Windows job objects, `D:/work`; HeardRight model-store to `rightkit-models`, capture to `rightkit-audio`, job to `rightkit-process`; MailRight `rightkit-cache`, `rightkit-secrets`; Pulse `rightkit-control` with the Tauri 2.12 feature set; **Pulse `rightkit-fsindex 0.1.0` (macOS target; B's Pulse section, lines 354-356, `hub/src-tauri/Cargo.toml:35-38`, used at `disk_index.rs:1,69`)**.

Footnotes:
- [1] Counting basis. A counts stale manifest lines per app; B's census above counts distinct declared RightKit names (for example CodeRight 28 names in 22 manifests, ScrapeRight 24 in 9). The two are not the same measure and are not reconciled.
- [2] Pulse pins. A's targets are qa 0.2.14, release 0.2.147, app-shell 0.3.0 and platform-ui 0.2.0; B observed declared pins release 0.2.141 (now 0.2.146 per the concurrent workspace delta), qa 0.2.12, app-shell 0.2.1 and platform-ui 0.1.2. B's observed pins stand; A's are bump targets.
- [3] A's ~8,400 LOC for Pulse includes a ~1,800 LOC hub scanner/watch item whose target, `rightkit-fsindex`, Pulse already uses; the figure is overstated by whatever part of that item `disk_index.rs` already covers (not re-measured).

### Per-app handover lists (from static pass, unverified by build)

Prioritized; hand each to the app's chat after its pin bump. Corrections applied during the merge are marked **[corrected]**.

**CodeRight** (engine/, context/engine/, apps/coderight-tauri, apps/coderight-web)
1. Bump all 26 pins in one pass; unify duplicate db/process/watch versions in the lockfiles (~300 LOC; breaking minors: watch, fs, oauth, http, db, sandbox, embed, @rightkit/tauri).
2. `kill_tree`, `process_tree`, `daemon_lifecycle`, `no_window` to `rightkit-process@0.3.3` (~1,335 LOC).
3. Tauri `window_state`, autostart, tray, windows, presence to `rightkit-shell@0.1.4` (~1,260 LOC).
4. Keyring token stores and three PKCE/OAuth copies to `rightkit-secrets@0.1.2`, `rightkit-oauth@0.3.0` (~2,600 LOC; ChatGPT/Codex flows need presets).
5. `license.rs` to `rightkit-license@0.1.3` (375); context-engine embed/fs/db to `rightkit-embed@0.3.1`, `-fs@0.3.3`, `-db@0.3.2` (~1,200, Membrane chat).
6. **[corrected]** iOS: `apps/coderight-ios/project.yml:23-28` pins RightKitSwift `from: 0.1.0` (a range) while the generated project resolves exact `0.1.1` (B: Apple item A08); convert to one exact existing tag and regenerate.
Parked: sandbox/PTY block (~7,800) until `rightkit-sandbox` has SandboxTier, deny_read, confined Windows PTY; boot probation and Rust updater (2,375) have no owner.

**ScrapeRight**
1. Land the in-flight RightKit migration (29 uncommitted edits) before anything else.
2. `puppeteer-core` (block) to `@rightkit/qa-chrome` + `@rightkit/qa`.
3. Reconcile 27 pins (db 0.2.1 to 0.3.2, logs 0.1.0 to 0.2.0 breaking).
4. `vendor/parakeet-rs` + speech crate to `rightkit-asr`, `rightkit-vad` (~9,600 LOC; same upstream).
5. Browser stack to `rightkit-browser@0.2.2` (~1,600). Cloud sync waits on `rightkit-sync`.

**HeardRight** (tauri-app-next/)
1. Bump 14 pins (UI packages are 0.x minors).
2. Telemetry outbox/uploader/writer to `rightkit-logs@0.2.0` (~2,460).
3. Parakeet primitives + Whisper CoreML helpers to `rightkit-asr` (~1,275).
4. `vad.rs` + onnx-asr ORT env/session to `rightkit-vad`, `rightkit-ort@0.2.2` (~1,080).
5. Credentials to `rightkit-secrets@0.1.2` (965).
Parked: speech-service host side (1,411; `rightkit-speech-client` is client-only), device-lease client (786), KWS engine (1,330, no owner), hotkeys/overlay/llm/sync (unpublished).

**ViewRight**
1. Bump 12 pins (db, logs, legal-ui, media-ui, @rightkit/tauri breaking).
2. `viewright-pdf` crate + `pdf_adapter.rs` to `rightkit-pdf@0.2.0` (~4,950; sources diverge 260-860 lines per file, prove parity first).
3. `path_broker`, `binary_stream`, `stream_http`, `site_root`, `clipboard`, `setup_recovery`, `startup_cache`, `traffic_lights` to `rightkit-shell@0.1.4` (~1,800).
4. `ocr_worker.rs` to the `rightkit-ocr` worker (331); persistence migration/quarantine to `rightkit-db` (~250).
5. Watcher debounce + license plumbing (~600); front-end shell/theme/settings packages (~2,800).
Parked: native PDFKit view (1,847), file associations (1,351), Rust updater (no owner).

**CutRight**
1. Bump 18 pins, `rightkit-ort` =0.1.4 to 0.2.2 first; drop the dead `rightkit-process =0.1.0` workspace pin (B: `Cargo.toml:51`).
2. `cutright-speech` Parakeet runtime/VAD/transcript to `rightkit-asr`, `-vad`, `-transcript` (~2,520).
3. ffmpeg runner/probe/loudness + project lock to `rightkit-media`, `-process`, `-fs` (~1,650).
4. Rest of `cutright-audio` to `rightkit-audio` (1,087).
5. Engine supervisor/discovery/settings/cache to `rightkit-service`, `-jobs`, `-shell`, `-settings`, `-cache` (~2,200).
Also: `mediaAdapters.ts:5` still says `rightkit-playback` is unpublished; wire `preparePlayback`. Parked: review/llm/contracts/compose (~1,850) until published; GPU compositor and audio output have no owner.

**Pulse** (mac/ Swift, core/, windows/, hub/)
1. Raise the Rust toolchain 1.90 to 1.98 (every RightKit crate needs it; gates all Rust adoption; B: core keeps direct rusqlite for this reason).
2. Bump 4 pins (qa 0.2.14, release 0.2.147, app-shell 0.3.0, platform-ui 0.2.0; see footnote [2]).
3. In-repo DMG/NSIS scripts to `@rightkit/release` (~660).
4. `core/src/ipc` to `rightkit-service` + `rightkit-framed-sidecar` (~1,700).
5. Pinned-fd state dir + locks to `rightkit-fs` (~900).
6. **[corrected]** The static pass listed "hub scanner/watch to `rightkit-fsindex` (~1,800)". Pulse **already uses `rightkit-fsindex 0.1.0`** (B 03:354-356). Re-scope this item to whatever hub scanner/watch code remains outside `disk_index.rs`, or drop it; not re-measured here.
Parked: native updater, Windows HTTP backend, Swift SQLite/hotkeys/AX, non-Tauri autostart (no owner). Delete or quarantine the dead `PulseMacPrototype` (8.3k LOC, not built in CI; also listed in 02).

**MailRight**
1. Bump 9 pins (db, embed, oauth, http, @rightkit/tauri breaking).
2. `db/search/*` to `rightkit-search@0.2.0` (1,409).
3. `webview_idle.rs` to `rightkit-shell` idle (336).
4. Direct reqwest in google/backoff/webimg/translate/oauth to `rightkit-http@0.2.4` (~600).
5. `mcp.rs` to `rightkit-mcp` ServerHost (232); front end to app-shell, theme, legal-ui, pdf-ui, doc-render, settings-ui (~2,900).
Parked: `ai.rs` (213) until `rightkit-llm` publishes. Scanner coverage gap: `workers/waitlist/package.json` unresolvable.

**GenRight**
1. Port off root `rightkit_models::Store` onto `rightkit_models::suite` (audit 4.3 blocker; check `suite` exports first).
2. Bump 18 pins; add `minimumReleaseAgeExclude` entries for 0.3.0 UI packages.
3. QTS client/worker/models/protocol to `rightkit-tts`, `rightkit-qts@0.2.4` (1,157; diverged, not a swap).
4. `ureq` in 13 files to `rightkit-http@0.2.4` (~700). Review per client against B's redirect, range and no-network findings before swapping.
5. ffmpeg path resolution to `rightkit-media` (~250; `local_lipsync.rs:157` honours `GENRIGHT_FFMPEG` in release, which RightKit refuses).
Parked: contracts/align/review/channel-memory/record until published; legal gate empty until legal pages exist.

**VoiceRight**
1. Fix `src-tauri/Cargo.toml:16` `qts` path dep (`../eval/qts-upstream` missing) to `rightkit-qts@0.2.4` (blocker 1 below).
2. TTS engine + voice flow to `rightkit-tts@0.1.0` (~1,100; confirm voice-clone support for `engine/mod.rs:94-143`).
3. `ort` rc.9 to rc.13 via `rightkit-ort@0.2.2`.
4. Model provisioning to `rightkit-models@0.2.7`; WAV I/O to `rightkit-audio`.
5. `@rightkit/release` 0.2.109 to 0.2.147; adopt app-shell, platform-ui.

**SwiftRight**
1. `@rightkit/release` 0.2.146 to 0.2.147.
2. Upstream the `SwiftRightServices` engine client (1,202) into RightKitSwift (the static pass tied this to a Windows platform at `Package.swift:9`; see the Windows-platform decision below, which concludes that entry is not needed).
3. `EngineClient.swift:200` direct `Process()` to `rightkit-process` via the planned C ABI (does not exist yet).
4. Build/verify scripts to `right-release swift-package`.
5. QA drive scripts to `rightkit-qa` (`rightkit-automation` unpublished).

**WorkRight** (last source commit 2026-08-01)
1. Remove the `@rightkit/release` ban in `scripts/release-doctor.mjs:75-83`; add right-release config and workflows (blocker 2 below).
2. `local-store` to `rightkit-db` + SQLCipher + `rightkit-secrets` (the DB is plaintext today).
3. Helper-to-core IPC to `rightkit-service` + `rightkit-framed-sidecar`.
4. Helper supervisor to `rightkit-process`.
5. Logging/crash to `rightkit-logs`, `rightkit-crash`. Capture phase 2 waits on hotkeys, record.

**PostRight** (plan-only): scaffold `postright-engine` on jobs 0.2.6, db 0.3.2, secrets 0.1.2, settings, logs, crash, process 0.3.3, http 0.2.4 (~500 LOC). Bluesky-through-daemon waits on RK10 + RK7; OAuth on RK1.

**SellRight** (expanded from [apps-g5.md](/Volumes/D/claude/rightkit/docs/audits/2026-10-10-audit-evidence/apps-g5.md))
1. Resolve scanner admission first (commit or stash `AGENTS.md`, `LICENSE`, `.agent/config.json`, then rerun); Adrian owns the commit.
2. Replace Playwright e2e + `@playwright/test` (block-tier) with `@rightkit/qa@0.4.0` + `@rightkit/qa-chrome@0.1.3` (~240 LOC plus `ci.yml:682`; blocker 5 below).
3. `acceptance.ts` equality checks to a `@rightkit/legal@0.3.1` server-claim verifier (~150 LOC; RightKit gap: add `verifyAcceptanceClaim`).
4. `update-tier.ts` + `runtime-artifact-manifest.ts` + vendored `license-v2.json` onto `@rightkit/release`/`updates`/`license` exports (~370 LOC; RightKit must add package `exports`).
5. **[corrected]** iOS `Keychain.swift` (45) and `OrderActivityController.swift` (37) to RightKitAppleCredentials / RightKitLiveActivity. The static pass said 0.1.1 is the latest published, but tags 0.3.0-0.3.7 exist (version truth below); pin one exact existing 0.3.x tag in `ios/project.yml:19` and `Package.resolved`.
6. Add `distribution.provider: "github-releases"` once it is confirmed SellRight releases through GitHub (it ships a Docker appliance).
7. `packages/admin/src/theme.tsx` + generated tokens to `@rightkit/theme@0.2.1` (~410 LOC; four-theme vs brand-file model needs a decision). 8. right-git CI adoption (~1,431 LOC of workflow) lacks a node/pnpm profile in `@rightkit/git@0.2.33`.

**RightSites** (expanded from apps-g5.md)
1. Fix `right-release.config.mjs:4` provider `r2-rightsites` (blocker 3 below).
2. Replace 5 `test-legal-publishing-contract.mjs` + `legal-contracts.ts` (417 LOC to ~60) with a shared `@rightkit/legal` / `legal-contract` check (RightKit gap: checkout-subset generator, `verify-published-bundle`).
3. `workers/license-gate` verifier (252+93+57) to an `@rightkit/license` WebCrypto verifier (~400 LOC; RightKit gap: no TS verifier; vector `test-vectors/license-v2.json` already shared).
4. Runtime-artifact and update-tier validation (192+183) onto `@rightkit/release`/`updates` (needs an `exports` map).
5. Decide ownership of the server-side license service (~6,000 LOC + StoreKit ~1,300): RightSites owns `licensing` (server) or a new `@rightkit/license-server`. Adrian decides.
6. Clean stale config (`pnpm-workspace.yaml:55-56`; a stale `oauth.endpoint.google` entry in the repo's ownership exceptions record, which needs owner approval and may not be edited by agents). 7. `orthiclabs/artifacts/scroll-test.mjs` to `@rightkit/qa` (453 LOC, low priority). 8. iOS is a byte-for-byte duplicate of SellRight's `ios/`; keep one.

**xtool** (expanded from apps-g5.md)
1. Decide scope: register xtool in the ownership repositories registry (Adrian-only) or record it as out-of-suite.
2. `native/macos-desktop-helper` AX code (1,656) + `computer/` (2,792) to a `rightkit-control`/`rightkit-automation` sidecar (blocked: automation unpublished; needs a Node-callable surface).
3. `secrets.ts` (363) via a `rightkit-secrets` helper over `rightkit-service` (RightKit gap: no Node binding).
4. `mcp/` server host (3,400) to `rightkit-mcp@0.5.2` via sidecar, or keep and add `@rightkit/ax@0.2.4` gates.
5. Align `packageManager` to 11.24.0 and add `engines` (2 LOC).

**lightcraft** (expanded from apps-g5.md)
1. Decide registration and the Node harness: add `package.json` + `.rightgit.json` + `release:admit` so `release-candidate.yml` can run, or drop the managed lane (blocker 4 below).
2. Retire `release.yml` / `windows-arm64.yml` / `packaging/*/sign*` for `right-release` native-host signing (~1,100 LOC; no cross-compile; no Linux/FreeBSD/web story in RightKit).
3. `crates/mcp` (1,439) to `rightkit-mcp@0.5.2`. 4. `catalog/lock.rs` + `write_atomic` (~200) to `rightkit-fs@0.3.3`.

### Swift targets without an equivalent row in B's target matrix (from static pass, unverified by build)

Method there: per target, package type, tools version, platforms, Swift language mode, UI/storage stacks, native boundary and tests; `swift package dump-package`, `gh api` tag/release listing; no build. B's matrix above covers CodeRight, HeardRight, ScrapeRight (app and credentials helper), SellRight, RightSites, SwiftRight and xtool, so only the two rows B lacks are imported.

| Target | Shape | Deps | Duplicates | Top handover items |
|---|---|---|---|---|
| RightKitSwift (`rightkit/swift/RightKitSwift`) | SwiftPM 6.0, iOS 17 / macOS 14, Swift 6 mode, 14 products, 11.1k src / 5.3k test LOC, no third-party deps; parity with Rust by shared vectors, no C ABI | 22 system frameworks, all used (2 heavy, both in Speech) | brand-named `RightKitOverlayKind` cases (`heardRightPill`, `cockpitNotch`) | 1. publish 0.3.8 and fix the feed (needs a fresh gate receipt and Adrian's publish request); 2. de-brand overlay kinds, keeping JSON raw values; 3. RightKitSpeech split is **DISPUTED** (see 02) |
| Pulse mac (`pulse/mac`) | notch app, 35.5k LOC, Swift 5 minimal concurrency, macOS 26, no tests, no RightKitSwift (B: `mac/Package.swift` has no RightKit dependency); dead `PulseMacPrototype` 8.3k LOC, macOS 13, 7 tests, not in CI | 21 needed, 4 replace, 2 heavy, 2 unused (in the dead prototype) | ~2,900 LOC (hotkeys 603, updater 542, settings + hub bridge 1,466, overlay ~270) | 1. delete or quarantine the prototype; 2. add tests, starting with the disk-image installer; 3. hotkeys to `rightkit-hotkeys` (needs the C ABI) |

Unverified there: no build or test run; test counts are greps (except RightKitSwift's receipt of 195 passed and SwiftRightServices' README 29/29, neither re-run; B's own executed macOS suite counted 201 cases); remote tag contents not diffed against local source; watchOS consumption of the Watch/LiveActivity products; Diagnostics API compatibility 0.1.0 to 0.3.7 for the iOS apps; which Pulse updater files ship.

**RightKitSwift version truth.** Git tags on `bogusyogi/rightkit-swift` are `0.1.0`, `0.1.1` and `0.3.0` through `0.3.7` (no 0.2.x, no 0.3.8, no GitHub Releases; listing from the static pass via `gh api`, not re-run). Source `VERSION` is `0.3.8` and is unpublished. The feed (`rightkit-versions.json:321-332`) records adopted 0.1.1 / staged 0.3.1 and is stale against every real pin: CodeRight generated project 0.1.1 (its `project.yml` also says `from: 0.1.0`), HeardRight 0.3.2, ScrapeRight 0.3.7, the template 0.3.0, SellRight/RightSites `from: 0.1.0` (resolves 0.1.0, violating the exact-pin rule). The last gate receipt covers 0.3.7 and names a commit not in this checkout.

Decisions surfaced by the static pass:
- **Windows platform for RightKitSwift: not needed.** SwiftPM has no Windows deployment entry and the sources depend on CryptoKit, Security, CloudKit, StoreKit and UIKit/AppKit. SwiftRight should depend on it `.when(platforms: [.macOS])` and use the Rust C ABI on Windows.
- **C ABI:** no Rust crate exports one today; it gates `rightkit-process`, `rightkit-hotkeys` and framed-sidecar adoption from Swift (SwiftRight, Pulse).
- **RightKitSpeech split:** DISPUTED, see 02.

### RightKit gaps the apps are waiting on (from static pass, unverified by build)

| Gap | Apps blocked | Action |
|---|---|---|
| Unpublished: hotkeys, overlay, llm, sync, record, daemon, review, contracts, align, channel-memory, scenario, compose; automation `publish = false` | HeardRight, CodeRight, MailRight, CutRight, GenRight, ScrapeRight, WorkRight, PostRight | publish decisions per crate (needs catalog owner + qualification) |
| `rightkit-sandbox`: no SandboxTier, deny_read, confined Windows PTY | CodeRight (~7,800 LOC) | design item |
| No Rust/native updater owner (`rightkit-shell` is Tauri-coupled) | CodeRight, ViewRight, Pulse | design item |
| Speech host side (`speech-client` is client-only); device-lease client; KWS | HeardRight | decide owner |
| `rightkit-ort`: two DirectML knobs; `rightkit-vad`: CoreML Silero | CutRight, HeardRight, ScrapeRight | small additive change |
| `rightkit-http`: Windows system proxy / cert store; `Retry-After` parses numeric only, capped 30 s | Pulse, PostRight | small additive change |
| `rightkit-service`: current-user only; no OS-service registration (launchd / SCM) | WorkRight, Pulse | design item |
| RightKitSwift: feed records 0.1.1 / staged 0.3.1 while tags reach 0.3.7 and source is 0.3.8 (unpublished); **no Rust C ABI for Swift consumers**; no notch overlay kind (a Windows platform entry is not needed) | SwiftRight, Pulse, HeardRight, ScrapeRight, SellRight, RightSites | publish 0.3.8 with a fresh gate receipt; correct the feed; decide the C ABI owner |
| Server-side license/entitlement + legal verifier; TypeScript/WebCrypto token verifier; `@rightkit/release` `exports` map; Node CI template in `@rightkit/git` | RightSites, SellRight | decide owner (RightApps vs RightKit) |
| Toolchain floor 1.98 | Pulse (1.90) | app-side bump |
| `rightkit-tts` voice-clone support unconfirmed | VoiceRight | verify |
| No owner: GPU compositor, audio output, native PDFKit view, file associations, boot probation, R2/S3 presigned URLs, platform capability rules, skill-execution machinery | CutRight, ViewRight, CodeRight, PostRight | explicit "stays in app" or new crate |

### Five concrete blockers (from static pass, unverified by build)

Each is a defect in an app's or a workflow's own input that stops adoption regardless of RightKit readiness. Evidence is in [apps-g3.md, apps-g4.md and apps-g5.md](/Volumes/D/claude/rightkit/docs/audits/2026-10-10-audit-evidence/).

| # | App | Blocker | Evidence | B cross-check |
|---|---|---|---|---|
| 1 | VoiceRight | `qts` is a path dependency `../eval/qts-upstream/crates/qts` and that directory does not exist, so the embedded build cannot resolve outside the author's machine | `src-tauri/Cargo.toml:16` | B notes the local path QTS and the `eval/qts-upstream` boundary but does not record that the directory is missing. |
| 2 | WorkRight | Its own release doctor fails the build on RightKit references, so `@rightkit/release` cannot be adopted; `docs/RELEASE.md:51` also requires the three-binary (service + helper + UI) install model to be represented in release tests (RightKit-side work) | `scripts/release-doctor.mjs:75-83` | B: WorkRight has no RightKit pin or import; the ban is not recorded. |
| 3 | RightSites | `right-release.config.mjs` names provider `r2-rightsites`, which `@rightkit/release` does not accept (`publish-update.mjs:47` requires `r2-rightapps`, `upload-release.mjs:55` requires `github-releases`); a release would fail closed. The fix is a one-line rename on one side; Adrian decides which. | `right-release.config.mjs:4` | Not examined by B. |
| 4 | lightcraft | `release-candidate.yml` (right-git managed, template 1.8.0) runs `pnpm install --frozen-lockfile`, `pnpm exec right-release ownership --app .` and `pnpm run release:admit`, but the repo has no `package.json`, lockfile, `.rightgit.json` or `release:admit`, so the lane cannot run; the repo is also not in the ownership repositories registry | `.github/workflows/release-candidate.yml:1-2,84-86` | B: LightCraft has no RightKit manifest or import; the lane defect is not recorded. |
| 5 | SellRight | `@playwright/test` is a block-tier fingerprint (`browser.npm.playwright-test`); replace with `@rightkit/qa` + `@rightkit/qa-chrome` and drop the CI browser install | `packages/storefront/package.json:45`, `ci.yml:682` | Not examined by B. |

## Exact handoff evidence

- [app-rightkit-dependencies.csv](/Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-10/evidence/app-rightkit-dependencies.csv)
- [app-rightkit-references.csv](/Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-10/evidence/app-rightkit-references.csv)
- [app-manifest-inventory.json](/Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-10/evidence/app-manifest-inventory.json)
- [machine-repositories.json](/Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-10/evidence/machine-repositories.json)
- [collect-inventory.py](/Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-10/evidence/collect-inventory.py)

Each app chat should receive its full section, exact declaration rows, selected verified package versions & acceptance checks. No messages have been sent to other chats. Start alignment only after selected RightKit package closure has release evidence & standalone-install proof.

## Coverage & verification status

| Dimension | Result |
|---|---|
| RightKit source inventory | All 24 npm package manifests & 56 workspace crate manifests enumerated; 26 npm manifests including workspace root/template, 66 Rust package manifests including 10 fixtures/examples/template packages. |
| Declared dependencies | 171 npm declarations & 437 Rust direct edges, retaining runtime/dev/build/peer/optional/target/feature scope. Swift & .NET helpers listed separately. |
| App census | 22 current/retained checkout roots, 260 npm/Cargo RightKit declarations, 3,109 lexical references across 20,718 hashed files; semantic review distinguishes imports, adapters, comments & generated assets. |
| Duplication scan | 1,042 first-party source/e2e files, 229,545 significant lines; exact 16-line windows identify candidates, followed by source-body review. No AST-complete clone claim. |
| Apple skill follow-up | Both Legion iOS & macOS skills applied; all 14 Swift products, helpers, template/spike & Apple consumers reviewed. macOS suite: 201 cases/3 skips/0 failures; iOS product build passed; StoreKit simulator: 8 pass; CloudKit helper built; recorder helper: 14 cases/1 skip/0 failures; contract parity: 2 pass. Old archive helper: 1 pass/2 failures. Exact logs & closure checks are linked below. |
| Other runtime/package qualification | No new Windows qualification, actual packed-registry-artifact installation or live registry verification accepted. Native Apple checks supplement source review, not app/device/distribution qualification. |
| Native Audit gate | **INCOMPLETE; quality gate UNPROVEN.** 78 providers returned results, only 16 marked complete; no reasoning receipt ingested. Graph unavailable; source byte limits hit; lens packets failed with `plan digest differs from the executed plan`. `legion verify` failed: `native application config is required`. |

Frozen Audit runner output is recorded separately, not treated as verified product findings. Direct source review & separately executed Apple checks supply inventories, recommendations & bounded native evidence. Use Apple closure checklist for remaining repairs, then qualify selected published versions & app integrations.

## Apple targets, helpers, exact pins & native integration handoffs

Read-only consumer source review of current target specs, product imports, entitlement/privacy files,
helper boundaries & repository instructions. Shared-package/helper native checks are recorded in
[Apple verification](/Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-10/evidence/apple-verification.md); consumer application builds & signing were not performed. `RightKitSwift` supports iOS 17 & macOS 14, with fourteen library products
([Package.swift](/Volumes/D/claude/rightkit/swift/RightKitSwift/Package.swift:7)).

### Target matrix

| Project / scheme / targets | Minimum, Swift mode, host | Exact RightKitSwift use | Entitlement, resource & privacy facts | Proof present in source |
| --- | --- | --- | --- | --- |
| CodeRight `apps/coderight-ios`; `CodeRight`, Widget, File Provider, unit/UI tests | All inherit iOS 26.5; app/Widget/File Provider set Swift 6. Native XcodeGen app with Rust static-library bridge ([app](/Volumes/D/claude/coderight/apps/coderight-ios/project.yml:12), [extensions](/Volumes/D/claude/coderight/apps/coderight-ios/project.yml:161)). | App uses **range** `from: 0.1.0`, declares `RightKitLicense`, `RightKitDiagnostics` & imports License ([project.yml](/Volumes/D/claude/coderight/apps/coderight-ios/project.yml:26)). Widget/File Provider declare no direct RightKitSwift product; each is embedded by app ([project.yml](/Volumes/D/claude/coderight/apps/coderight-ios/project.yml:159)). | App, Widget & File Provider each declare `group.app.coderight.ios` ([app entitlement](/Volumes/D/claude/coderight/apps/coderight-ios/CodeRight/CodeRight.entitlements:5), [extension settings](/Volumes/D/claude/coderight/apps/coderight-ios/project.yml:186)). File Provider uses that group as document group ([project.yml](/Volumes/D/claude/coderight/apps/coderight-ios/project.yml:193)); app declares camera, Face ID, microphone, speech-recognition & Live Activity usage ([project.yml](/Volumes/D/claude/coderight/apps/coderight-ios/project.yml:111)). No `PrivacyInfo.xcprivacy` source was found in this target. | Unit & UI targets exist ([project.yml](/Volumes/D/claude/coderight/apps/coderight-ios/project.yml:222)); neither ran. |
| HeardRight `ios-native`; `HeardRight`, Widgets, Watch, Watch Widgets, Share, UI/Sync tests | iOS 17, watchOS 11, Swift 5; Share alone sets strict concurrency `complete`; native XcodeGen app plus Rust/XCFramework dependencies ([project.yml](/Volumes/D/claude/heardright/ios-native/project.yml:1)). | Exact `0.3.2`; app declares UI, Purchases, CloudSync, Store, ImportQueue, LiveActivity, WatchBridge, Speech, License, Intents, AppleCredentials & Overlay; extensions select matching products ([project.yml](/Volumes/D/claude/heardright/ios-native/project.yml:10)). | App groups: `group.app.heardright.ios.native`, shared models; CloudKit container, APNs, Sign in with Apple, shared keychain. Extensions only retain app group ([entitlements](/Volumes/D/claude/heardright/ios-native/HeardRight/HeardRight.entitlements:5)). Main manifest records accessed APIs & functional collected-data categories ([PrivacyInfo](/Volumes/D/claude/heardright/ios-native/HeardRight/PrivacyInfo.xcprivacy:5)); extension/privacy manifests exist. Mic, background audio/processing/push & Live Activities are declared ([project.yml](/Volumes/D/claude/heardright/ios-native/project.yml:55)). | Source smoke checks assert Info.plist & entitlement shape ([entitlements smoke](/Volumes/D/claude/heardright/ios-native/Tests/entitlements_smoke.py:44)), but were not run. |
| ScrapeRight `ios-native`; `ScrapeRight`, Share, unit & UI test schemes | iOS 17, Swift 5; native XcodeGen app ([project.yml](/Volumes/D/claude/scraperight/ios-native/project.yml:1)). | Exact `0.3.7`; app declares AppleCredentials, Artifacts, Diagnostics, ImportQueue, Purchases, CloudSync, Store & License; Share declares AppleCredentials + ImportQueue ([project.yml](/Volumes/D/claude/scraperight/ios-native/project.yml:96)). Imports establish direct use of each app product except Store, which is declared but has no matching source import in reviewed files. | Main app has app/model groups, CloudKit, APNs development & shared keychain; Share has app group ([entitlements](/Volumes/D/claude/scraperight/ios-native/ScrapeRight/ScrapeRight.entitlements:5)). Photo-library usage, Files exposure & remote-notification background mode are declared ([project.yml](/Volumes/D/claude/scraperight/ios-native/project.yml:49)). Main privacy manifest declares UserDefaults reasons & no tracking ([PrivacyInfo](/Volumes/D/claude/scraperight/ios-native/ScrapeRight/PrivacyInfo.xcprivacy:5)). | Unit & UI schemes exist ([project.yml](/Volumes/D/claude/scraperight/ios-native/project.yml:189)); none ran. |
| ScrapeRight `macos-native/ScrapeRightAppleCredentials` executable | macOS 14, Swift language mode 5; Swift subprocess helper for Rust/Tauri Keychain boundary. | Exact `0.3.7`, `RightKitAppleCredentials` ([Package.swift](/Volumes/D/claude/scraperight/macos-native/ScrapeRightAppleCredentials/Package.swift:4)). | No app entitlement/resource manifest lives in this package. Helper narrows requests to JSON-line status/read/write/delete/copy commands & documents shared keychain identity ([main.swift](/Volumes/D/claude/scraperight/macos-native/ScrapeRightAppleCredentials/Sources/ScrapeRightAppleCredentials/main.swift:1)). | No helper packaging, signed-launch, or actual Keychain proof ran. |
| SellRight `ios`; `SellRightAdmin`, Widgets, unit-test scheme | iOS 17.2, Swift 5, native XcodeGen app ([project.yml](/Volumes/D/claude/sellright/ios/project.yml:1)). | **Range**, URL `from: 0.1.0`; declares & imports `RightKitDiagnostics` ([project.yml](/Volumes/D/claude/sellright/ios/project.yml:17)). | No app group; app declares associated-web-credentials & development APNs ([project.yml](/Volumes/D/claude/sellright/ios/project.yml:49)). No `PrivacyInfo.xcprivacy` source found. Widget has no RightKit dependency. | Unit-test scheme exists ([project.yml](/Volumes/D/claude/sellright/ios/project.yml:93)); unrun. |
| RightSites `ios`; current project name/targets are also `SellRightAdmin` & Widgets | iOS 17.2, Swift 5, XcodeGen spec ([project.yml](/Volumes/D/claude/rightsites/ios/project.yml:1)). | **Range**, URL `from: 0.1.0`; declares & imports `RightKitDiagnostics` ([project.yml](/Volumes/D/claude/rightsites/ios/project.yml:17)). | Same associated-domain & development-APNs facts as SellRight ([project.yml](/Volumes/D/claude/rightsites/ios/project.yml:49)); no app group or privacy manifest source found. | Same unit-test scheme declaration; unrun. |
| SwiftRight compatibility review: `SwiftRightBridge`, `SwiftRightServices`, SliceApp example | Both libraries set macOS 14. Bridge is Swift 5; Services has no manifest language-mode override. SwiftRight is planning/prototype work, not a released consumer ([README](/Volumes/D/claude/swiftright/README.md:3)). | No `RightKitSwift` dependency. Services implements the Rust `rightkit-framed-sidecar` stdio envelope; Bridge is C-ABI/renderer support ([Services](/Volumes/D/claude/swiftright/packages/SwiftRightServices/Package.swift:4), [Bridge](/Volumes/D/claude/swiftright/packages/SwiftRightBridge/Package.swift:4)). | SliceApp’s macOS sample has an explicit empty entitlement record ([SliceApp.entitlements](/Volumes/D/claude/swiftright/examples/SliceApp/Mac/SliceApp.entitlements:3)); no iOS target or RightKitApple entitlement/resource claim was found. | SwiftRight records protocol compatibility gaps, including no normative `ready`, no cancel frame & no shared Swift process ABI ([Services README](/Volumes/D/claude/swiftright/packages/SwiftRightServices/README.md:98)). No new adoption defect assigned. |
| Xtool compatibility review: macOS Swift helper + N-API addon | macOS helper/addon supports macOS 12.3; product’s app floor is documented as 13.0 ([binding.gyp](/Volumes/D/claude/xtool/native/macos-desktop-addon/binding.gyp:1), [platform.ts](/Volumes/D/claude/xtool/src/main/platform.ts:4)). | No `RightKitSwift` dependency. Native helper imports ScreenCaptureKit/AppKit/AX APIs ([main.swift](/Volumes/D/claude/xtool/native/macos-desktop-helper/main.swift:1)); desktop runtime loads dylib + N-API addon on a Worker, preserving neutral JSON protocol ([index.ts](/Volumes/D/claude/xtool/src/main/computer/index.ts:173)). | No native entitlement or privacy-manifest source found. Packaged runtime requires both `macos-desktop-addon.node` & `libcos-desktop.dylib` under resources ([index.ts](/Volumes/D/claude/xtool/src/main/computer/index.ts:479)). | Live Mac test is explicitly opt-in ([macos-computer-live.test.ts](/Volumes/D/claude/xtool/test/macos-computer-live.test.ts:10)); no compatibility claim from source review. |

### Template & spike

| Item | Current source fact | Review result |
| --- | --- | --- |
| `swift/ios-app-template` | iOS 17/watchOS 10 template uses Git URL plus `exactVersion: 0.3.0`; app, Share, Widgets & Watch targets model group/CloudKit, import queue, Live Activity, Watch & purchase wiring ([project.yml](/Volumes/D/claude/rightkit/swift/ios-app-template/project.yml:8)). Its pin-check script rejects local paths & every non-exact requirement ([check-package-pin.sh](/Volumes/D/claude/rightkit/swift/ios-app-template/scripts/check-package-pin.sh:2)). | Pin policy is correct. Its own comment says tag `0.3.0` was not published & fresh clone cannot resolve it ([project.yml](/Volumes/D/claude/rightkit/swift/ios-app-template/project.yml:16)); this is a source claim, not a live tag check by B. The merged tag listing includes `0.3.0`, so the comment is likely stale (static evidence only). Update template pin/comment together after confirming the tag. |
| `swift/scraperight-ios-spike` | iOS 17 fixture target intentionally uses sibling `../RightKitSwift`, imports Artifacts/Diagnostics/License & has a unit-test target ([project.yml](/Volumes/D/claude/rightkit/swift/scraperight-ios-spike/project.yml:1)). Test source exercises a loopback archive, checksum rejection/install & JSONL diagnostic write ([ScrapeRightSpikeTests.swift](/Volumes/D/claude/rightkit/swift/scraperight-ios-spike/Tests/ScrapeRightSpikeTests.swift:7)). | Proof-only spike, not consumer adoption; its local path makes it unsuitable as a published-consumer portability proof. |

### Concrete adoption / wiring findings

1. **Exact pin invariant breach — CodeRight.** Its RightKitSwift requirement is `from: 0.1.0`, not an exact tag ([project.yml](/Volumes/D/claude/coderight/apps/coderight-ios/project.yml:26)). Replace with known published tag after release evidence.
2. **Exact pin invariant breach — SellRight & RightSites.** Both current specs use `from: 0.1.0` ([SellRight](/Volumes/D/claude/sellright/ios/project.yml:17), [RightSites](/Volumes/D/claude/rightsites/ios/project.yml:17)). Pin each exact tag; decide whether RightSites is intentionally a duplicate before changing both.
3. **Template fresh-clone block, by source claim.** Template says its exact `0.3.0` tag is unpublished ([project.yml](/Volumes/D/claude/rightkit/swift/ios-app-template/project.yml:16)). Current shared source version says `0.3.8` ([VERSION](/Volumes/D/claude/rightkit/swift/RightKitSwift/VERSION:1)); B made no inference about remote tag availability; the merged tag listing (`0.3.0`-`0.3.7` exist) suggests the comment is stale. Align comment & template pin only against verified publication state.

### Helper boundary & closure

- `RightKitCloudKitHelper` is macOS 14 executable-only, with a local `../RightKitSwift` package dependency ([Package.swift](/Volumes/D/claude/rightkit/swift/RightKitCloudKitHelper/Package.swift:7)). This is an internal RightKit source relationship, not an app-consumer pin violation. It is a long-lived JSON-lines adapter between `rightkit-sync` & `RightKitCloudSync` ([README.md](/Volumes/D/claude/rightkit/swift/RightKitCloudKitHelper/README.md:3)); Rust keeps one child with an explicit timeout & stderr tail ([process.rs](/Volumes/D/claude/rightkit/crates/rightkit-sync/src/helper/process.rs:52)). A distributed helper must still carry self-contained same-tag sources or a released dependency.
- CloudKit real-device proof remains distinct & unrun: source requires an app-owned helper `.app`, provisioning profile, signing & per-app CloudKit state/race/offline/account-change evidence ([README.md](/Volumes/D/claude/rightkit/swift/RightKitCloudKitHelper/README.md:19)). No named consumer in scope proved that closure.
- `rightkit-record` Mac boundary is narrower: Rust launches/reaps signed Swift helper through `rightkit-process`, uses a framed stdio protocol, & the helper owns ScreenCaptureKit while core owns bundle data ([mod.rs](/Volumes/D/claude/rightkit/crates/rightkit-record/src/macos/mod.rs:1)). Runtime locates binary from `RIGHTKIT_RECORD_MACOS_HELPER` or beside executable ([mod.rs](/Volumes/D/claude/rightkit/crates/rightkit-record/src/macos/mod.rs:57)). Local script is ad-hoc only; it explicitly prohibits shipping that signature ([build.sh](/Volumes/D/claude/rightkit/crates/rightkit-record/helpers/macos/build.sh:8)). Signed bundle placement, TCC grant & live capture are Mac proof gaps, not source-proven packaging defects.

### Platform proof gaps

- **iOS apps:** no consumer application simulator/device build or launch ran. Shared-package iOS compile & StoreKit simulator tests passed separately. Source declarations do not prove SPM resolution, generated-project linkage, entitlement provisioning, CloudKit/APNs, extension exchange, StoreKit, or PrivacyInfo inclusion. HeardRight has focused source smoke checks, but source presence is not execution proof.
- **macOS:** no signed helper bundle, Keychain/iCloud account, CloudKit container, TCC grant, or ScreenCaptureKit capture ran. The subprocess protocols are source-backed; native identity & installed-bundle behavior are not.


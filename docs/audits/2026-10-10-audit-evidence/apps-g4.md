# Apps group 4: Pulse, SwiftRight, WorkRight (RightKit adoption audit)

Date: 2026-10-10. Read-only. Sources: `rightkit/INDEX.md`, `docs/CAPABILITY-MAP.md`, `packages/release/rightkit-versions.json` (registry section = latest published), `swift/RightKitSwift/Package.swift`, `docs/audits/2026-10-08-inventory-final.md:148-171` (Wave 3 donor-retirement table; none of its rows name these three apps).

Latest published (registry section) used for stale-pin checks: cargo `rightkit-control 0.1.11`, `rightkit-fs 0.3.3`, `rightkit-fsindex 0.1.0`, `rightkit-http 0.2.4`, `rightkit-mcp 0.5.2`, `rightkit-qa 0.2.14`, `rightkit-db 0.3.2`, `rightkit-process 0.3.3`, `rightkit-secrets 0.1.2`, `rightkit-service 0.1.3`, `rightkit-settings 0.1.0`, `rightkit-logs 0.2.0`, `rightkit-crash 0.2.0`, `rightkit-framed-sidecar 0.1.0`, `rightkit-watch 0.3.1`; npm `@rightkit/release 0.2.147`, `app-shell 0.3.0`, `platform-ui 0.2.0`, `shell 0.1.0`, `settings-ui 0.3.0`, `legal 0.3.1`, `legal-ui 0.2.0`, `git 0.2.33`; Swift `rightkit-swift` manifest `swift` section 0.1.1 (published), `stagedSwift` 0.3.1, source `swift/RightKitSwift/VERSION` 0.3.8. Not in registry (none recorded): `rightkit-hotkeys`, `rightkit-overlay`, `rightkit-llm`, `rightkit-automation`, `rightkit-record`, `rightkit-sync`, `rightkit-daemon` (INDEX.md flags hotkeys/overlay/llm as "MISMATCH public package has no release record").

Cross-cutting RightKit-side blockers found while auditing (apply to more than one app):

- Every Rust RightKit crate declares `rust-version = "1.98.0"` (e.g. `crates/rightkit-service/Cargo.toml`, `-http`, `-fs`, `-db`). Pulse core/windows pin 1.90 (`pulse/rust-toolchain.toml:2`, `pulse/Cargo.toml:8`, `pulse/windows/Cargo.toml:5`).
- `rightkit-shell` hard-depends on `tauri` (`crates/rightkit-shell/Cargo.toml:26`) and `rightkit-service` Windows builds on `windows 0.62` (`crates/rightkit-service/Cargo.toml:23`); no Tauri-free OS layer (tray, autostart, single-instance, launch-at-login) exists for native Win32/Swift/GPUI hosts.
- `RightKitSwift` is `platforms: [.iOS(.v17), .macOS(.v14)]` (`swift/RightKitSwift/Package.swift:9`): not consumable on Windows, so SwiftRight's Windows side and any Swift-on-Windows host cannot use it.
- `rightkit-service` is "current-user-only" (INDEX.md); no cross-user (LocalSystem service to user session) channel.
- `@rightkit/updates` and `@rightkit/settings-ui`/`@rightkit/app-shell` are Tauri/React only; there is no native (non-Tauri) update client.

---

## Pulse

### 1. Shape

Git: `/Volumes/D/claude/pulse`, 0 dirty files. Last commit 2026-10-10 11:47 (`e09052d7`); last source commit 2026-10-10; history starts 2026-10-05. Proprietary (docs/open-source-readiness.md:3). AGENTS.md: local static only, all compile/test in generated RightKit GitHub workflows.

| Unit | Manifest | Lang / platform | LOC (first-party unless noted) |
|---|---|---|---|
| `pulse-core` (lib + `pulse` CLI) | `core/Cargo.toml`, root workspace `Cargo.toml:1-3` | Rust, mac + Windows | 37,348 (`core/src`) |
| `pulse-windows-prototype` (Win32 notch) | `windows/Cargo.toml` (own `[workspace]`), deps: `windows =0.61.3` only | Rust, Windows x64 | 28,674 |
| `pulse-hub` (Tauri 2 hub) | `hub/src-tauri/Cargo.toml` (own workspace, lock-less) + `hub/package.json` | Rust 7,754 + React/TS 7,370 | 15,124 |
| `pulse-hub-qa` | `hub/qa-e2e/Cargo.toml` (own workspace) | Rust, mac + Windows | 2,156 (`tests/ui.rs`) |
| Mac notch app (Pulse.app) | `mac/Notch/project.yml` (XcodeGen, macOS 26.0, no SwiftPM deps) | Swift | 44,481 total under `mac/`; app `mac/Notch/Sources` ~35.6K (Providers 5,713; Notch 7,690; Features 3,592; Conveniences 2,692; Model 2,587; System 2,433; Sessions 2,288; Launcher 2,363; Settings 1,535; Sharing 1,270; Updater 542) |
| `PulseMacPrototype` | `mac/Package.swift` (swift-tools 5.9, macOS 13, zero deps) | Swift salvage | 8,321 Sources + 287 Tests |
| vendor donors | `vendor/petal` (13,064, Rust, MIT donor), `vendor/codenotch` (106,519: Swift 55,015 + Windows Tauri 15,276 + hook 140; pinned upstream, `upstream.lock.json`), `third_party/smartmontools` | donors, not product | excluded from move counts |
| Release scripts | `package.json`, `right-release.config.mjs`, `scripts/release/*` | Node | 661 (mac-payload 364, windows-payload 219, pulse.nsi 78) + 86 candidate + 13 dispatch-env |
| QA/probes scripts | `scripts/qa/*.mjs` (625), `scripts/probes/*.mjs` (887) | Node | 1,512 |

### 2. RightKit adoption today

| Pin (path:line) | Pinned | Latest published | Verdict |
|---|---|---|---|
| `hub/src-tauri/Cargo.toml:27` `rightkit-control` (optional, `qa-native`) `=0.1.11`, `default-features=false, features=["tauri-plugin","wv2-0_39"]` | 0.1.11 | 0.1.11 | current; matches the Tauri 2.12+ feature note (`wv2-0_39`, `default-features=false`) |
| `hub/src-tauri/Cargo.toml:38` `rightkit-fsindex` (mac only) `=0.1.0` | 0.1.0 | 0.1.0 | current (not in INDEX.md; registry-only) |
| `hub/qa-e2e/Cargo.toml:14` `rightkit-qa` `=0.2.12` | 0.2.12 | 0.2.14 | STALE (2 patches) |
| `package.json:24` `@rightkit/release` `0.2.141` (pnpm-lock.yaml:29) | 0.2.141 | 0.2.147 | STALE (6 patches) |
| `hub/package.json:12` `@rightkit/app-shell` `0.2.1` (lock `hub/pnpm-lock.yaml:57`) | 0.2.1 | 0.3.0 | STALE (0.x minor bump; CHANGELOG has no 0.3.0 entry, so breaking surface unknown) |
| `hub/package.json:13` `@rightkit/platform-ui` `0.1.2` | 0.1.2 | 0.2.0 | STALE (0.x minor bump) |
| RightKitSwift | none (`mac/Notch/project.yml` and `mac/Package.swift` have no package deps) | 0.1.1 published / 0.3.8 source | not adopted |
| `.github/workflows/ci.yml:70`, `release-candidate.yml:84` | ownership step wired (`--advisory-when-pending`) | n/a | ok |

Stale pins: **4** (rightkit-qa, @rightkit/release, @rightkit/app-shell, @rightkit/platform-ui). Breaking gaps: MSRV (all Rust crates 1.98 vs core/windows 1.90); app-shell/platform-ui 0.x minors (verify `--rk-*` class use: hub uses `.rk-*` and `.ck-*` selectors in `hub/src/interaction.css:14-27`). No `rightkit-control` feature mismatch found.

### 3. Still-to-move table (CAPABILITY-MAP capabilities)

Status key: ON = already on RightKit; DUP = in-house duplicate (LOC = file LOC, est. = estimated LOC touched); N/A; GAP = RightKit lacks it.

| Capability | Status | Pulse code | LOC | Target owner | Notes |
|---|---|---|---|---|---|
| desktop-control | ON | `hub/src-tauri/src/lib.rs:479` embedded Control plugin | n/a | rightkit-control 0.1.11 | |
| desktop-qa | ON | `hub/qa-e2e/tests/ui.rs:31-33` | 2,156 | rightkit-qa | stale pin |
| file-index | ON + DUP | on: `hub/src-tauri/src/disk_index.rs:69`. dup: `hub/src-tauri/src/scanner.rs` 1,524, `watch.rs` 279 (FSEvents), `core/src/platform/mac_bulk.rs` 223 (`getattrlistbulk`) | 2,026 (est. 1,800) | rightkit-fsindex 0.1.0 | macOS only. Scanner flags 11 raw fingerprints. Windows backend is a stub (`crates/rightkit-fsindex/README.md` Platform support) |
| local-services / IPC | DUP | `core/src/ipc/mod.rs` 348, `unix.rs` 842, `windows.rs` 861 (SDDL pipe `windows.rs:715`, `getpeereid` `unix.rs:755`) | 2,051 (est. 1,700) | rightkit-service 0.1.3 + rightkit-framed-sidecar 0.1.0 (length-prefix read `ipc/mod.rs:274`) | needs MSRV 1.98 |
| safe-filesystem | DUP | `core/src/store.rs` 1,885 (pinned-fd state dir), `bridge/store.rs` 1,004 (flock + rename), `hub/src-tauri/src/cache.rs` 104 (atomic versioned file), `core/src/state_migration.rs` 115; ~20 files with ad hoc `rename(` | ~3,100 (est. 900 touched) | rightkit-fs 0.3.3 | verify O_NOFOLLOW/`*at` parity first; persisted format must not change |
| http-client | DUP | `windows/src/http.rs` 364 (raw WinHTTP, system proxy and cert store) | 364 | rightkit-http 0.2.4 | GAP in RightKit: rustls-only `ureq` (`rightkit-http/Cargo.toml:28`), no WinHTTP/system-proxy/OS cert-store backend; Pulse-windows deliberately dependency-free (`windows/Cargo.toml`) |
| sqlite-storage | DUP | `core/Cargo.toml:26` rusqlite 0.32 (read-only Codex state, `core/src/bridge/deliver_codex.rs:355-382`); Swift `mac/Notch/Sources/Providers/SQLiteStore.swift` 70 | est. 60 + 70 | rightkit-db 0.3.2 (`open_read_only`, `lib.rs:217`) | comment at `core/Cargo.toml:24-25` records the MSRV block. GAP: no `immutable=1` fallback for foreign WAL DBs; no Swift SQLite owner |
| os-credential-store | DUP | Swift `Providers/KeychainItem.swift` 157, `KeychainPrompt.swift` 162, `CredentialCache.swift` 177, `ClaudeCredentials.swift` 280, `CodexCredentials.swift` 85 (14 `SecItem*` fingerprints) | 861 (est. 400) | RightKitSwift `RightKitAppleCredentials` | RightKitSwift pin must reach >= 0.3.8 (manifest says 0.1.1 published). GAP: attributes-only enumeration of foreign duplicate items (`KeychainItem.swift:1-25` rationale) is not in the target |
| desktop-overlay | DUP | Windows: `windows/src/layout.rs` 522, `lifecycle.rs` 279, `surface.rs` 251, `runtime.rs` 1,032 (part), 9 `WS_EX_NOACTIVATE` sites (`main.rs:471,937,1349`); Mac: 6 `ignoresMouseEvents` sites (`CornerPassage.swift:432`, `NotchWindowController.swift:409,1411`) | ~2,100 (est. 500) | rightkit-overlay (not published), RightKitOverlay (Swift) | Rust crate unpublished; Swift `RightKitOverlayKind` has only `.heardRightPill` (`swift/.../Overlay.swift:3`): GAP no notch kind |
| global-hotkeys | DUP | `windows/src/keys.rs` 396 (WH_KEYBOARD_LL + `SendInput` `:367`); Mac `Conveniences/EventTapHub.swift` 123, `Keyboard/FnCommand.swift` 89 | 608 (est. 300 Rust) | rightkit-hotkeys 0.1.0 (not published) | GAP: no Swift hotkey/event-tap owner; remap-with-swallow policy is product |
| desktop-automation | DUP | `mac/Notch/Sources/Conveniences/AXSupport.swift` 93 (+ FinderCutPaste, WindowMaximizer, DockClickMinimize; 14 `AXUIElement*` fingerprints) | 93 core + ~2,600 conveniences | rightkit-automation (private, staged 0.1.0) | Rust-only; no Swift owner. Conveniences are product features: keep policy, only AX primitives move |
| settings-persistence | DUP | `windows/src/settings.rs` 1,634 + `json.rs` 243 (hand parser by design); Mac `Settings/Preferences.swift` 980 (UserDefaults) | 2,857 (est. 900 Rust) | rightkit-settings 0.1.0 | needs serde (Pulse-windows has none); schema is shared with Mac pill (`settings.rs:1-15`), keep wire schema |
| structured-logging / crash | DUP + GAP | `windows/src/diag.rs` 179 (key=value stderr); `mac/Notch/Sources/App/Log.swift` 10; 13 `eprintln!` in core/hub; no crash capture anywhere | ~190 | rightkit-logs 0.2.0, rightkit-crash 0.2.0, RightKitDiagnostics (Swift) | crash reporting simply absent (not a RightKit gap) |
| owned-processes | DUP | `windows/src/hub.rs` 290 (spawn + restart at most once per 30 s); Mac `System/HubLauncher.swift` 251 | 541 (est. 250 Rust) | rightkit-process 0.3.3 | `core/src/process_control.rs` 361 is user-app Quit/Force Quit, not owned children: N/A |
| desktop-shell | DUP | `hub/src-tauri/src/win_bridge.rs` 303 (single-instance mutex + named events), `lib.rs:433-445` `show_in_dock`/`hide_to_notch`, `lib.rs:471-538` builder; `windows/src/autostart.rs` 285 (Run key) | ~900 (est. 400 hub) | rightkit-shell 0.1.4 + @rightkit/shell 0.1.0 (hub); autostart for non-Tauri notch = GAP | hub has no tray, no window-state; adoption must keep hide-to-notch. Tauri 2.12 needs `wv2-0_39` (same note as control) |
| update-policy | GAP | `windows/src/update.rs` 997 (GitHub feed, SHA-256, WinVerifyTrust), Mac `Updater/` 542 (ReleaseFeed 122, UpdateVerifier 100, UpdateDownload 100, UpdateInstaller 95, Relauncher 91, SemanticVersion 34), hub `Settings.tsx:109-140,356-382` | 1,539 | @rightkit/updates 0.2.5 is Tauri-only | RightKit lacks a native update client (feed, size+digest+signature check, relaunch) |
| release-tooling | DUP | `scripts/release/mac-payload.mjs` 364 (uses `appdmg` + `@electron/osx-sign`, `:297-298`), `windows-payload.mjs` 219 (makensis, `:182-196`), `windows/pulse.nsi` 78; `package.json:25-28` deps | 661 | @rightkit/release 0.2.147 (`dmg-build.mjs`, `nsis-payload.mjs`, `swift-mac-dmg.mjs`) | violates "no installer machinery in product repo" (AGENTS.md); scanner flags `appdmg` and `@electron/osx-sign` |
| app-shell-ui / platform-ui | ON (stale) + DUP | `hub/src/*` uses app-shell; `hub/src/api.ts:4` `isWindows` UA sniff | 1 site | @rightkit/platform-ui `getPlatform` | scanner finding |
| theme-ui | DUP (donor only) | `vendor/petal/src/sunburst.rs:511`, `vendor/codenotch/windows/codenotch/src/backdrop.rs:107` luminance math; hub `styles.css` 243 not token-driven | small | @rightkit/theme 0.2.1 | donor files: do not edit; hub styles est. 150 |
| settings-ui / i18n / legal / onboarding / license | N/A or later | Hub Settings.tsx 1,107 renders notch-published state (command bridge `windows/src/bridge.rs`), Mac has 13 locales in `Localizable.xcstrings` | n/a | @rightkit/settings-ui 0.3.0, @rightkit/i18n 0.1.0 | Pulse is proprietary but has no license server or legal gate; decide whether the Right Suite legal gate applies. Settings UI adoption blocked because state ownership is the notch |
| file-watching | DUP (donor+Swift) | `hub/.../watch.rs` (counted above), `windows/src/installer.rs` ReadDirectoryChangesW watcher (inside 1,720), Swift `Sessions/PiResponseMonitor.swift:55` FSEventStream | est. 150 | rightkit-watch 0.3.1 / fsindex | |
| model-installation, audio, asr, tts, ocr, pdf, media, embeddings, llm, mcp, oauth, browser-automation, jobs, cache, search, license, sync, daemon | N/A | Pulse reads Claude/Codex usage endpoints (`mac/Notch/Sources/Providers/Claude*`, `windows/src/usage.rs` 1,959) with its own tokens: product code, not an LLM client. `plugins/pulse-bridge` is a Claude skill, not MCP. `core/src/storage_browser.rs` 712 search runs on in-memory scan data (rightkit-search is a query-language compiler): low-value candidate | n/a | n/a | |
| LAN file share (LocalSend v2), region screenshot, zstd decoder, SMART drive health, duplicate finder, app uninstall | N/A / GAP | `core/src/localsend` 3.3K + `windows/src/send.rs` 2,107 + `send_sys.rs` 447 + hub `share.rs` 862 + Mac `NearbySharing.swift` 996; `windows/src/shot.rs` 1,679 (+viewshots 1,162); `zstd.rs` 730; `core/src/drive_health.rs` 637 | ~13K | none | No RightKit owner exists; product features. zstd decoder should be a third-party crate, not RightKit |

Totals (Pulse): **13 in-house-duplicate rows with a RightKit owner today** (IPC, safe-fs, fsindex-consolidation, http, sqlite, keychain, overlay, hotkeys, settings, logs, process, hub-shell, release-tooling); **est. 8.4K LOC touched** (largest single items: IPC 1.7K, fsindex 1.8K, fs 0.9K, settings 0.9K, release scripts 0.66K). 5 RightKit-gap rows (native updater, Swift SQLite, Swift hotkeys/AX, Windows http backend, non-Tauri autostart/shell).

### 4. Ownership scanner summary

Command run: `right-release ownership --app /Volumes/D/claude/pulse --json` -> `apps-g4-pulse-ownership.json` (exit 1: `ok:false`, error `ownership-coverage`, "4 ownership coverage gap(s)").

- Capability rows: 71 = 54 `unscanned` (no fingerprint hit), 14 `blocked` (fingerprint hits; status name only, every finding is advise tier), 3 `declared-dependency` (desktop-control, desktop-qa, app-shell-ui).
- Findings: 73 violations, **0 block tier, 73 advise tier**.
- By capability: desktop-automation 15 (14 `AXUIElement*` in `mac/Notch/Sources/Conveniences/AXSupport.swift`, 1 `SendInput` `windows/src/keys.rs:367`), desktop-overlay 15 (6 `ignoresMouseEvents`, 9 `WS_EX_NOACTIVATE`), os-credential-store 14 (`SecItem*` in `KeychainItem.swift` etc., owner RightKitSwift), file-index 11 (FSEventStream x8, getattrlistbulk x3), local-services 5 (SDDL x3, peer-credentials x2), sqlite-storage 2, framed-sidecar 2, release-tooling 2, theme-ui 2, and 1 each: http-client, desktop-shell, file-watching, platform-ui, update-policy. 6 of the 73 sit in `vendor/codenotch`/`vendor/petal` donors (rusqlite, ureq, notify, single-instance, updater, luminance).
- Coverage gaps (unscanned): `vendor/petal/Cargo.toml` git sources `gpui-ce`, `gpui_ce_platform`; `Cargo.lock` roots `hub/qa-e2e/Cargo.toml` and `hub/src-tauri/Cargo.toml` unresolved (lock-less own workspaces). These two hub workspaces are therefore not lock-verified by the scanner.

### 5. Handover list (Pulse chat)

1. Raise core/windows toolchain 1.90 -> 1.98 (`rust-toolchain.toml:2`, `Cargo.toml:8`, `windows/Cargo.toml:5`) -> unblocks every Rust RightKit crate; est. 10 LOC + lock regen; blocker: Rust 1.98 on mac/win CI runners (hub already on 1.98: `hub/rust-toolchain.toml:2`).
2. Bump stale pins -> `@rightkit/release@0.2.147` (`package.json:24`), `rightkit-qa =0.2.14` (`hub/qa-e2e/Cargo.toml:14`), `@rightkit/app-shell@0.3.0` + `@rightkit/platform-ui@0.2.0` (`hub/package.json:12-13`); est. 20 LOC; blocker: app-shell 0.3.0 changelog missing, run hub parity journey.
3. Delete in-repo DMG/NSIS machinery -> `@rightkit/release@0.2.147` (`dmg-build`, `nsis-payload`, `swift-mac-dmg`), drop `appdmg`/`@electron/osx-sign`; est. 660 LOC removed; blocker: right-release must express the multi-binary Helpers signing list (`right-release.config.mjs` sign.prePackageFiles) and per-OS Helpers (smartctl, PulseHelper, pulse-elevate).
4. Replace `core/src/ipc/{mod,unix,windows}.rs` -> `rightkit-service@0.1.3` + `rightkit-framed-sidecar@0.1.0`; est. 1,700 LOC; blocker: MSRV (item 1); Windows crate `=0.61.3` vs service `windows 0.62` (two copies); keep read-only allowlist semantics (`ipc/mod.rs:1-9`).
5. Replace pinned-fd state dir, locks and atomic writes (`core/src/store.rs`, `bridge/store.rs`, `hub/.../cache.rs`) -> `rightkit-fs@0.3.3`; est. 900 LOC; blocker: prove O_NOFOLLOW/`*at` parity and unchanged on-disk format.
6. Collapse hub scanner/watch + `mac_bulk.rs` onto -> `rightkit-fsindex@0.1.0` (macOS); est. 1,800 LOC; blocker: `allocated_unique`/clone semantics parity with `scanner.rs`; Windows backend is a stub, so `core/src/platform/win_native.rs` (596) stays.
7. Hub shell -> `rightkit-shell@0.1.4` + `@rightkit/shell@0.1.0` (single instance, show/hide); est. 400 LOC; blocker: keep hide-to-notch; use `wv2-0_39` when Tauri >= 2.12 (same rule as rightkit-control).
8. Windows notch small modules -> `rightkit-settings@0.1.0` (settings.rs+json.rs, est. 900), `rightkit-logs@0.2.0` (diag.rs, 190), `rightkit-process@0.3.3` (hub.rs, 250), `rightkit-http@0.2.4` (http.rs, 364); blocker: dependency-free policy (needs serde), no WinHTTP/system-proxy backend in rightkit-http.
9. Mac Swift -> `RightKitSwift` (`RightKitAppleCredentials`, `RightKitDiagnostics`, `RightKitOverlay`) once a tag >= 0.3.8 is published; est. 400-900 LOC; blocker: manifest pin is 0.1.1 / staged 0.3.1; no notch overlay kind; foreign-keychain enumeration missing.
10. Hotkeys/overlay on Windows -> `rightkit-hotkeys@0.1.0`, `rightkit-overlay@0.1.0`; est. 800 LOC; blocker: both unpublished (INDEX.md MISMATCH).
11. Raise as RightKit gaps: native non-Tauri updater, Windows-native HTTP backend, immutable-fallback read-only SQLite, Swift SQLite/hotkey/AX owners, Tauri-free shell/autostart.

---

## SwiftRight

### 1. Shape

Git: `/Volumes/D/claude/swiftright`; 1 dirty path (untracked `probes/sr0-s/fresh-on-bridge/out/matrix-08/`). Last commit 2026-10-10 11:36 (`290f129`), history since 2026-10-08. Private repo (`bogusyogi/swiftright`), status "architecture and implementation planning"; not a released SDK (`README.md:5`). Qualified targets: Windows x64 + macOS arm64 only (`docs/architecture.md` section 9, scope revision 1).

| Unit | Manifest | Lang / platform | LOC |
|---|---|---|---|
| `SwiftRightBridge` | `packages/SwiftRightBridge/Package.swift` (tools 6.0, macOS 14, Foundation only) | Swift; Windows impl behind `#if os(Windows)` | 1,904 src + 104 tests |
| `SwiftRightServices` (+ `FakeEngine`) | `packages/SwiftRightServices/Package.swift` | Swift, mac + Windows | 1,202 src (EngineClient 509, EngineWire 264, FakeEngine 162, Configuration 95, EngineProcess 70, EngineJSON 62, EngineError 40) + 499 tests |
| `swiftright-render` | `render/swiftright-render/Cargo.toml` v0.0.3, `gpui =0.2.2`, `accesskit =0.25.1`, own `[workspace]`, `publish=false` | Rust cdylib, Windows only | 5,663 (+770 ABI header) |
| Examples | `examples/SliceApp` (`package.json`, `Package.swift`, `right-release.config.mjs` 198, `rightkit-qa.toml`), `BridgeSmoke`, `BridgeGeoModal` | Swift + PowerShell/sh/Node | 7,270 |
| Probes | `probes/sr0-*`, `sr1-*` (many Package.swift/Cargo.toml) | research | 119,948 (of which `probes/sr0-s/scui-fork` 68,829 is a vendored upstream fork); excluded from move counts |
| Qualification | `qualification/run.py` 1,448 (+ `test_run.py` 1,444), `clean-machine/mac-clean-run.sh` 801 | Python/sh | 5,115 |

### 2. RightKit adoption today

| Pin (path:line) | Pinned | Latest | Verdict |
|---|---|---|---|
| `examples/SliceApp/package.json:8` `@rightkit/release` | 0.2.146 (lock lines 11-27) | 0.2.147 | STALE (1 patch) |
| `examples/SliceApp/right-release.config.mjs:4` comment | "written against 0.2.138" | | doc drift only |
| `examples/SliceApp/rightkit-qa.toml` | runner found on PATH, not pinned | rightkit-qa 0.2.14 | unpinned |
| any `rightkit-*` crate / RightKitSwift | none | | not adopted |

Stale pins: **1**. The SDK owns no consumer of RightKit crates yet: `docs/architecture.md:202-208` lists intended owners (rightkit-settings/db, rightkit-secrets + RightKitSwift, rightkit-process/framed-sidecar/service, rightkit-shell, rightkit-hotkeys, rightkit-logs/crash/qa/automation, @rightkit/release + updates) and says "not claims of working Swift adapters"; `architecture.md:214` says D records each missing Swift/C ABI adapter against its RightKit owner. The `swiftright-services` C-ABI library does not exist. `examples/SliceApp/swift-package.json` drives `right-release swift-package mac-app --sign` (command exists in `packages/release/swift-package-command.mjs`).

### 3. Still-to-move table

| Capability | Status | SwiftRight code | LOC | Target | Notes |
|---|---|---|---|---|---|
| EngineService | adapter, upstream candidate | `packages/SwiftRightServices` (Swift client of the `rightkit-framed-sidecar` wire, derived by hand from `wire.rs` / `length_prefixed.rs` line numbers, README:5-30) | 1,202 | RightKitSwift new `RightKitEngine` target; contract file under `rightkit/contracts/` | RightKitSwift has no engine client and no Windows platform (`Package.swift:9`). Drift risk: no shared JSON contract like `contracts/license-v2.json`. Cancellation not in wire (`EngineWire`/README:49) |
| owned-processes | gap in adapter | `EngineClient.swift:200` `Process()` (no job object / kill-tree / restart tracker) | ~70 (`EngineProcess.swift`) | rightkit-process 0.3.3 via C ABI (`architecture.md:204,214`) | scanner could not run; manual grep finds this single site |
| SecureStore, Settings, Hotkeys, Window/tray/launch, Diagnostics/crash | not built | none (architecture-only) | 0 | rightkit-secrets / RightKitAppleCredentials, rightkit-settings, rightkit-hotkeys, rightkit-shell, rightkit-logs, rightkit-crash | RightKit gaps: no Swift/Windows consumption path; rightkit-shell is Tauri-coupled (`rightkit-shell/Cargo.toml:26`), hotkeys unpublished |
| release-tooling | partly ON, partly DUP | on: `right-release.config.mjs`, `swift-package`, `install-app`. dup: `examples/SliceApp/scripts/` build-windows.ps1 90, clone-build-mac.sh 89, clone-build-win.ps1 74, prepackage-* 50, bump-version-win.ps1 22, verify-package-layout.ps1 129 | 454 | @rightkit/release swift-package / hardening | `make-local-appinstaller.mjs:25-26` imports RightKit `msix-appinstaller.mjs` by path (not via a package export). `interrupt-*` (385) and `swift-only-proof.mjs` (144) are qualification probes of right-release itself: keep |
| desktop-qa / automation | DUP | `examples/SliceApp/qa/drive-windows.ps1` 435, `drive-mac.sh` 248, `mac-probe.swift` 99 (real AX/UIA driving); `qualification/run.py` evidence events + hashes | 782 (+ est. 300 evidence layer) | rightkit-qa 0.2.14 scenarios, rightkit-automation (private, staged), @rightkit/qa 0.4.0 evidence | automation unpublished. Conformance trace compare itself is SDK-specific: keep |
| update-policy | GAP (by design) | none; `architecture.md:233` "this SDK does not create another updater" | 0 | @rightkit/updates 0.2.5 is Tauri-only | MSIX App Installer feed via right-release is the route; native Swift update UX has no owner |
| desktop-shell overlap | GAP | none yet | 0 | rightkit-shell / @rightkit/shell 0.1.0 | Tauri-coupled; renderer bridge JS is N/A to a Swift host. Needs Tauri-free OS layer |
| RightKitSwift overlap | judged | RightKitUI (glass surface), RightKitOverlay (pill), RightKitStore, etc. are iOS/macOS app-level | n/a | n/a | no overlap with SwiftRightBridge/render (UI runtime); Mac consumption possible, Windows impossible (platforms) |
| SDK core (Bridge, render, probes, conformance, BridgeGeoModal harness) | N/A | | ~8.3K + probes | none | product of the SDK itself. `render` (GPUI/AccessKit) has no RightKit owner |

Totals (SwiftRight): **2 in-house-duplicate rows** (SliceApp build/verify scripts ~454 LOC; QA drive scripts ~782 LOC) = **~1.2K LOC**, plus **1 upstream candidate** (SwiftRightServices 1,202 LOC) that should move into RightKitSwift. RightKit gaps: Windows-capable RightKitSwift, Swift engine contract, Tauri-free shell, native updater, Swift/C ABI adapters for every Rust owner.

### 4. Ownership scanner summary

Command: `right-release ownership --app /Volumes/D/claude/swiftright --json` -> `apps-g4-swiftright-ownership.json`. **Failed**: `ok:false`, error `protected-evidence`, "committed ownership: protected release checkout has tracked or untracked changes" (the single untracked `probes/sr0-s/fresh-on-bridge/out/matrix-08/`). 0 rows, 0 findings; no counts available. Grep substitute: only fingerprint-like hit is `Process()` at `packages/SwiftRightServices/Sources/SwiftRightServices/EngineClient.swift:200` (owned-processes, would be advise). Re-run after the untracked output directory is ignored or removed.

### 5. Handover list (SwiftRight chat)

1. Bump `examples/SliceApp/package.json:8` -> `@rightkit/release@0.2.147`; est. 3 LOC + lockfile; blocker: none.
2. Move `SwiftRightServices` wire client upstream -> RightKitSwift `RightKitEngine` + a `contracts/engine-wire-v1.json` parity test; est. 1,202 LOC moved; blocker: RightKitSwift has no Windows platform (`Package.swift:9`) and is Foundation/CryptoKit-only; needs RightKit decision on a Windows-capable Swift package.
3. Replace `EngineClient` direct `Process()` with ownership-safe child lifecycle -> `rightkit-process@0.3.3` through the planned `swiftright-services` C ABI; est. 150 LOC; blocker: C ABI library not built; engine kill-tree semantics.
4. Retire SliceApp host build/verify scripts -> `right-release swift-package` + hardening/verify (`build-windows.ps1`, `clone-build-*`, `prepackage-*`, `verify-package-layout.ps1`, `bump-version-win.ps1`); est. 454 LOC; blocker: `swift-package` is documented as "not yet qualified on native hosts" (`packages/release/CHANGELOG.md:38`); SliceApp PACKAGING.md gaps 3, 3b (entitlements, app staple).
5. Replace `qa/drive-*.{sh,ps1}` and `mac-probe.swift` -> `rightkit-qa@0.2.14` scenarios (+ `rightkit-automation` when published); est. 780 LOC; blocker: automation is private/unpublished.
6. Remove `make-local-appinstaller.mjs` path import of RightKit internals -> a public `@rightkit/release` export; est. 39 LOC; blocker: export not defined.
7. Raise gaps to RightKit owners: Tauri-free OS shell layer (tray/autostart/single-instance), Swift/C ABI adapters per owner, native update client, `rightkit-hotkeys` publication.

---

## WorkRight

### 1. Shape

Git: `/Volumes/D/claude/workright`, 7 dirty paths (modified `Cargo.toml`, `README.md`; untracked `.blueprint/`, `LICENSE`, `docs/architecture.md`, `docs/product.md`, `okf/`). Last commit 2026-10-10 11:23 (agent-rules doc), but last source commit **2026-08-01** (`883786d` "initial import"); 16 commits total. Proprietary, `publish = false`. Phase 1: skeletons with real schema/crypto; many flows missing (352 lines of `okf/gap-*.md`).

| Unit | Manifest | Lang / platform | LOC |
|---|---|---|---|
| `workright-protocol` | `crates/protocol` | Rust | 875 |
| `workright-crypto` (BLAKE3 domain hashing, Ed25519) | `crates/crypto`, blake3 path dep `vendor/blake3-1.5.5` (6,437 LOC vendored) | Rust | 329 |
| `workright-local-store` | `crates/local-store` (rusqlite `=0.40.1`) | Rust | 834 |
| `workright-capture-core` | `crates/capture-core` | Rust | 115 |
| `workright-capture-windows` | `crates/capture-windows` (`windows-sys =0.59.0`, feature `winhooks`, Phase 1 skeleton) | Rust, Windows | 461 |
| `workright-capture-macos` | `crates/capture-macos` (`core-graphics =0.23.2`, feature `coregraphics`) | Rust, macOS | 432 |
| `workright-integrity` | `crates/integrity` | Rust | 152 |
| `workright-core-service` | `crates/core-service` (tokio, tracing, service-install manifests) | Rust, OS service | 653 |
| `workright-session-helper` | `crates/session-helper` | Rust | 141 |
| `workright-test-vectors` | `crates/test-vectors` | Rust | 165 |
| `workright-server` | `backend/server` (axum 0.8.9, rusqlite stand-in for Postgres, hand-rolled SigV4 R2 presign) | Rust | 2,908 (ingest 991, db 688, control 617, r2 310) |
| `@workright/dashboard` | `apps/dashboard/package.json` (React 19, react-router, TanStack Query) | TS | 737 |
| `@workright/agent-ui` (Tauri tray UI) | `apps/agent-ui` (`src-tauri` has `tauri.conf.json` + capabilities, **no Cargo.toml, empty `src`**) | TS | 79 |
| Scripts | `scripts/install-core.{sh,ps1}` 107, `release-doctor.mjs` 116 | sh/ps1/Node | 223 |

Rust workspace total 7,065 LOC (`Cargo.toml:1-14`, rust-version 1.98.0 at `:20`, matches RightKit MSRV). No `.github/workflows`, no `right-release.config.mjs`, `tests/*` dirs empty.

### 2. RightKit adoption today

**No `rightkit-*`, `@rightkit/*` or RightKitSwift dependency exists.** Stale pins: **0** (nothing pinned). `docs/RELEASE.md:51-52` says WorkRight joins `right-release` "only after its three-binary installation model is represented by `@rightkit/release` tests" (the architecture plan lists exact intended pins). **Policy conflict:** `scripts/release-doctor.mjs:75-83` fails the build if any package.json mentions `@rightkit/release|rightkit-shared`, which blocks the adoption the architecture requires (`2026-07-21-workforce-monitoring-platform-architecture.md:930` "Add exact RightKit dependency pins"). Version alignments that already hold: `rusqlite =0.40.1` equals `rightkit-db` (`rightkit-db/Cargo.toml` rusqlite 0.40.1); `serde_json =1.0.150` equals RightKit's `1.0.150`. Coexisting duplicates if adopted: `sha2 =0.10.9` vs rightkit-fs 0.11, `thiserror =1.0.69` vs 2, `windows-sys =0.59.0` vs 0.61 (`Cargo.toml:48`).

### 3. Still-to-move table

| Capability | Status | WorkRight code | LOC | Target | Notes |
|---|---|---|---|---|---|
| sqlite-storage | DUP | `crates/local-store/src/lib.rs` (WAL/busy_timeout PRAGMAs `:182`; `sqlcipher` feature referenced `:9-14,171-180` but the Cargo.toml has no `[features]`, `_key` unused `:168`, so DB is plaintext: `okf/gap-sqlcipher-local-encryption.md`) | 834 (est. 300 touched) | rightkit-db 0.3.2 (`sqlcipher` feature) + rightkit-secrets 0.1.2 (DB-key provisioning) | schema and record types stay; `backend/server/src/db.rs` 688 is a Postgres-shaped stand-in: N/A |
| os-credential-store | not built | none (key "held by OS keyring" only in comments) | 0 | rightkit-secrets 0.1.2 | |
| owned-processes | DUP | `core-service/src/lib.rs:49-60` `HelperGeneration`, supervision limit `:36` (no actual supervisor) | est. 150 | rightkit-process 0.3.3 (health/restart trackers; architecture doc cites it) | `main.rs:27` supervisor not implemented |
| local-services / IPC | not built | `protocol` `WireEnvelope` `lib.rs:690-772` unused (`okf/gap-local-ipc-channel-(helper-<->-core).md`); `MAX_FRAME_BYTES` `protocol/src/lib.rs:22` | est. 100 touched | rightkit-framed-sidecar 0.1.0 + rightkit-service 0.1.3 | RightKit GAP: service channel is current-user-only, but the core runs as LocalSystem/LaunchDaemon and the helper in the user session (`core-service/src/lib.rs:122-134`): needs a cross-user allowlisted-peer mode |
| OS service install | GAP | `core-service/src/lib.rs:122-300` install manifests (`sc.exe`, launchd plists), `scripts/install-core.sh` 64, `install-core.ps1` 43 | ~290 | none (RightKit has no `sc.exe`/launchd registration; grep of `crates/` finds no `StartServiceCtrlDispatcher`/`LaunchDaemon`) | new RightKit capability or product-local |
| global-hotkeys / input hooks | DUP (future) | `crates/capture-windows` 461 (`SetWindowsHookExW`, `LLKHF_INJECTED` plan), `capture-macos` 432 (`CGEventTapCreate` plan), `capture-core` 115 | 1,008 (skeleton, est. 300 touched) | rightkit-hotkeys 0.1.0 (CGEventTap / WH_KEYBOARD_LL primitives, unpublished) | GAP: aggregate listen-only counters + injected-flag classification are not in hotkeys' API; AX/TCC/Screen Recording permission probes |
| screen-recording | GAP | planned screenshot capture (`okf/gap-screenshot-capture-+-presign-+-put-+-finalize.md`) | 0 | rightkit-record (private, "none recorded") | RightKit recorder is session-bundle oriented; policy (consent, redaction) overlaps; not adoptable until published |
| structured-logging / crash | DUP | `tracing_subscriber::fmt` in `core-service/src/main.rs:198`, `session-helper/src/main.rs:28`, `backend/server/src/main.rs:87`; no crash capture | 9 tracing sites (est. 60) | rightkit-logs 0.2.0 (+ monitoring redaction allowlist per architecture doc), rightkit-crash 0.2.0 | |
| integrity / hashing | GAP (product) | `crates/crypto` BLAKE3 domain hashing + Ed25519 sign/verify; `crates/integrity` 152 rule catalog | 481 | none (rightkit-license verifies Ed25519 tokens only; rightkit-fs digest is SHA-256) | frozen protocol constants: keep product-owned. Replace vendored `vendor/blake3-1.5.5` path dep with crates.io `blake3` (RightKit-independent) |
| daemon / device ingest | candidate | `backend/server/src/ingest.rs` 991 (enrol, signed batches, hash chain), `auth.rs` 58, `jobs.rs` 14 | 1,063 | rightkit-daemon (private, not published: device enrolment, signed requests, blake3 resumable uploads, CAS) | cannot consume (private); semantics differ (per-event Ed25519 chain) |
| R2 presign | GAP | `backend/server/src/r2.rs` 310 (hand-rolled AWS SigV4) | 310 | none (no S3/R2 presign crate in `crates/`; artifact-storage is JS in @rightkit/release) | candidate new RightKit capability |
| cloud-sync | candidate | `local-store` outbox + planned sync coordinator | est. 0 now | rightkit-sync 0.1.0 (staged, unpublished) | |
| release-tooling | GAP | no `right-release.config.mjs`, no workflows; `docs/RELEASE.md` | 0 | @rightkit/release 0.2.147, @rightkit/git 0.2.33 | `scripts/release-doctor.mjs:75-83` bans RightKit refs. Three-binary install model (service + helper + UI) not represented in release tests |
| desktop-shell | not built | `apps/agent-ui/src-tauri` has config but no Cargo.toml or Rust | 0 | rightkit-shell 0.1.4 + @rightkit/shell 0.1.0, @rightkit/tauri 0.2.0 | tray, autostart, single instance |
| legal-assent / legal-ui / onboarding-ui | not built | consent receipt flow (`okf/gap-consent-receipt-production…md`) | 0 | @rightkit/legal 0.3.1, @rightkit/legal-ui 0.2.0, @rightkit/onboarding 0.3.0 | architecture doc line 568, 789 already plans `@rightkit/legal` |
| app-shell-ui / theme / i18n / settings-ui | N/A or later | `apps/dashboard` is a web SPA (737 LOC) | 737 | @rightkit/app-shell 0.3.0 is a desktop-app frame | adoption only if the dashboard is wrapped as a desktop app |
| licensing | N/A | employee/device identity is not a license (architecture doc line 55) | n/a | n/a | |
| http-client, llm, mcp, oauth, updater, models, audio, ocr, pdf, media | N/A | none present (server is axum, no outbound clients) | n/a | n/a | |

Totals (WorkRight): **5 in-house-duplicate rows** (local-store, supervisor, logging, framing/IPC, capture hooks) = **est. ~0.9K LOC touched** of 7.0K Rust; most of the value is building the missing pieces (IPC, supervision, SQLCipher, hooks, release, consent) on RightKit instead of from scratch. RightKit gaps: cross-user service channel, OS service install, passive input counters, R2/S3 presign, published `rightkit-record`/`rightkit-hotkeys`/`rightkit-daemon`.

### 4. Ownership scanner summary

Command: `right-release ownership --app /Volumes/D/claude/workright --json` -> `apps-g4-workright-ownership.json`. **Failed**: `ok:false`, error `protected-evidence`, "protected release checkout has tracked or untracked changes" (7 dirty paths). 0 rows, 0 findings. Manual expectation from the fingerprint catalog (`rightkit-ownership.json`): `db.cargo.rusqlite` (advise, sqlite-storage) at `Cargo.toml:45`, `crates/local-store/Cargo.toml`, `backend/server/Cargo.toml`; no other cargo/npm fingerprints match. Re-run after committing/stashing the dirty paths.

### 5. Handover list (WorkRight chat)

1. Remove the RightKit ban in `scripts/release-doctor.mjs:75-83`, add `right-release.config.mjs` and `@rightkit/git@0.2.33` generated workflows -> `@rightkit/release@0.2.147`; est. 150 LOC; blocker: three-binary (service + helper + UI) install model must be modeled in release tests (RightKit-side work, `docs/RELEASE.md:51`).
2. Replace `crates/local-store` open path and add real SQLCipher -> `rightkit-db@0.3.2` (feature `sqlcipher`) + `rightkit-secrets@0.1.2` DB-key provisioning; est. 300 LOC; blocker: vendored-OpenSSL build time on CI; closes `okf/gap-sqlcipher-local-encryption.md`.
3. Build helper<->core IPC on `rightkit-service@0.1.3` + `rightkit-framed-sidecar@0.1.0` instead of new code (retire unused `WireEnvelope` framing, `protocol/src/lib.rs:690-772`); est. 100 LOC touched, ~600 new LOC avoided; blocker: RightKit GAP for LocalSystem service to user-session helper (current-user-only ACL).
4. Implement helper supervisor/restart on `rightkit-process@0.3.3` (replace `HelperGeneration` hand-roll); est. 150 LOC; blocker: Windows session-0 vs user-session spawn semantics.
5. Logging and crash -> `rightkit-logs@0.2.0` with monitoring-specific redaction allowlist + `rightkit-crash@0.2.0`; est. 100 LOC; blocker: product telemetry upload remains app policy.
6. Capture Phase 2 on `rightkit-hotkeys@0.1.0` (input hooks) and `rightkit-record` (screenshots) when published; est. 300 LOC touched; blocker: both unpublished, listen-only aggregate API missing.
7. Consent/legal and agent tray: `@rightkit/legal@0.3.1` + `@rightkit/legal-ui@0.2.0`, `rightkit-shell@0.1.4` + `@rightkit/shell@0.1.0` (create `apps/agent-ui/src-tauri/Cargo.toml`, currently absent); est. 250 LOC new; blocker: none known.
8. Raise as RightKit gaps: OS service install capability (SCM/launchd), R2/S3 presign crate, cross-user local-service mode, `rightkit-daemon` publication or a published device-enrolment primitive.

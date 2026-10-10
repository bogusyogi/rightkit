# Swift / Apple audit, 2026-10-10 (read-only)

Method: Legion macOS + iOS development skills, step 1 (identify host architecture, native boundary, package/project, schemes, deployment minimum, Swift mode, architectures, UI and storage stacks) and `references/architecture.md` (preserve the established shape; split a target only for an evidenced ownership, build-time, or reuse benefit; make dependencies explicit at the lifetime owner; document compatibility before changing a persistence format, concurrency boundary, or native bridge). The iOS and macOS `architecture.md` / `toolchain.md` files are byte-identical.

Host: Xcode 27.0 (27A266a), Swift 6.4 (swiftlang-6.4.0.34.1), arm64-apple-macosx27.0. Nothing was built, signed, installed or edited. Only `swift package dump-package` (manifest parse), `gh api` tag/release listings, Read/grep, `wc` and `git log` were used. Note: the interactive shell aliases `grep` to a gitignore-respecting ugrep; the audit used `/usr/bin/grep` for workspace-wide greps (the plain-grep one-liner in the task returns only `ios-app-template/project.yml`, because the aliased ugrep honors ignore files (reason not investigated); raw grep finds 25 files, listed in the appendix).

## 0. RightKitSwift version truth

| Source | Value | Evidence |
|---|---|---|
| Tags actually on `bogusyogi/rightkit-swift` (private repo, default branch `main`, last push 2026-10-07T07:51:53Z) | 0.1.0, 0.1.1, 0.3.0, 0.3.1, 0.3.2, 0.3.3, 0.3.4, 0.3.5, 0.3.6, 0.3.7 (newest = 0.3.7, commit 48e08799). **No 0.2.x tags, no 0.3.8.** GitHub Releases list is empty (tags only). | `gh api repos/bogusyogi/rightkit-swift/tags` |
| Source `VERSION` | **0.3.8** (unpublished: adds `probeStatus`/`deleteAnyScope` in RightKitAppleCredentials) | `rightkit/swift/RightKitSwift/VERSION:1`; commit b08ed658 (2026-10-08) |
| Source CHANGELOG | none exists; README sections "(0.3.5)", "(0.3.7)", "(0.3.8)" are the change record | `rightkit/swift/RightKitSwift/README.md` |
| Release feed `swift` (adopted) | 0.1.1 | `rightkit/packages/release/rightkit-versions.json:321-326` |
| Release feed `stagedSwift` | 0.3.1 | `rightkit-versions.json:327-332` |
| Last gate receipt | 0.3.7, receipt commit b0566e95..., 195 passed / 0 failed, macOS arm64, `swift test --no-parallel` | `rightkit/release-evidence/gate/swift-RightKitSwift.json:1-24` |
| What apps pin | CodeRight iOS generated pbxproj 0.1.1 (generator says `from: 0.1.0`); heardright-release-0.1.167 snapshot 0.1.1; HeardRight 0.3.2; ScrapeRight iOS + macOS helper 0.3.7; SellRight and RightSites `from: 0.1.0` (resolved 0.1.0); ios-app-template 0.3.0 | see section 5 |

Conclusion: the published truth is **0.3.7**. The feed is two lifecycle steps stale (adopted 0.1.1, staged 0.3.1) and does not record any pin that real apps use. Source is one version ahead (0.3.8, unpublished). The feed's "0.1.1" is the highest version CodeRight/old HeardRight pin, not the newest tag. Per `docs/RIGHTKIT-SHARED-PACKAGES.md:71` the `swift` section is edited by hand after tagging; nobody has since 0.1.1. Publication gate is also blocked (receipt names commit b0566e..., which is not in this checkout; gate compares to HEAD 08252a68 per `packages/release/publication-gate.mjs:32-49`, as the prior fresh audit recorded in `docs/audits/fresh-2026-10-10/evidence/swift-publication.md`). `git status` of `rightkit/swift` is clean.

## 1. Target A: RightKitSwift source and siblings

### 1.1 Shape

| Item | Value |
|---|---|
| Type | SwiftPM library package (git-URL distributed; no Swift registry). Canonical source `rightkit/swift/RightKitSwift`; `bogusyogi/rightkit-swift` is a tagged mirror (`README.md` "Source of truth") |
| Tools / language | swift-tools-version 6.0, no `swiftLanguageModes` override, so **Swift 6 language mode (strict concurrency) for all targets** (`Package.swift:1`, dump-package `swiftLanguageVersions: null`) |
| Platforms | iOS 17, macOS 14 only (`Package.swift:9`). No watchOS/visionOS/tvOS entry even though WatchBridge/LiveActivity exist (they guard with `canImport`). No Windows entry (see section 7) |
| Architectures | not constrained by manifest; receipt run is arm64 macOS only |
| Products | 14 libraries, 14 test targets, **0 third-party dependencies**, one linker setting (`-lz` on RightKitArtifacts) |
| UI / storage / boundary | UI: SwiftUI (RightKitUI), AppKit NSPanel (RightKitOverlay, `#if os(macOS)`). Storage: Keychain (License, AppleCredentials), UserDefaults (Purchases, CloudSync, License device state), App Group files + `flock` (Store), JSONL files (Diagnostics). Native boundary: none to Rust at runtime (no C ABI, no XPC, no `Process()`); cross-runtime parity is by shared vectors (`contracts/license-v2.json`) and a line-JSON helper protocol (CloudKitHelper) |
| Last commit touching sources | b08ed658 2026-10-08 (0.3.8); tree moved by 68fb6a29 2026-10-10 |

Per-target (Sources LOC / test LOC / test funcs; tests: grep of `func test`/`@Test`, receipt says 195 passing):

| Target | Src LOC (files) | Test LOC / funcs | Depends on | Platform imports |
|---|---:|---:|---|---|
| RightKitLicense | 1534 (3) | 1676 / 58 | none | Foundation, CryptoKit, Security, Combine, UIKit (guarded) |
| RightKitDiagnostics | 150 (2) | 92 / 5 | none | Foundation |
| RightKitArtifacts | 259 (1) | 182 / 7 | none (+`-lz`) | Foundation, CryptoKit, zlib |
| RightKitPurchases | 874 (7) | 308 / 8 | RightKitLicense | StoreKit, Security, Combine, UIKit (guarded) |
| RightKitCloudSync | 2186 (9) | 1036 / 36 | none | CloudKit, Network, CryptoKit |
| RightKitStore | 506 (5) | 219 / 9 | none | Darwin |
| RightKitImportQueue | 432 (4) | 101 / 4 | RightKitStore | UniformTypeIdentifiers |
| RightKitLiveActivity | 177 (2) | 45 / 2 | none | ActivityKit (`@preconcurrency`, guarded) |
| RightKitWatchBridge | 247 (2) | 82 / 3 | none | WatchConnectivity (guarded) |
| RightKitSpeech | 3915 (23) + 103 KB `whisper_mel_128.bin` | 1165 / 47 | none | CoreML, AVFoundation, Speech, Accelerate |
| RightKitIntents | 139 (1) | 63 / 4 | none | CoreFoundation, AVFAudio (guarded) |
| RightKitAppleCredentials | 373 (1) | 211 / 10 | none | Security, LocalAuthentication |
| RightKitOverlay | 166 (1) | 102 / 5 | none | AppKit (`#if os(macOS)`) |
| RightKitUI | 143 (1) | 41 / 3 | none | SwiftUI |
| **Total** | **11,101 (62)** | **5,323 / ~201** | | |

Siblings under `rightkit/swift/`:

| Path | Shape | LOC |
|---|---|---:|
| `RightKitCloudKitHelper` | executable SwiftPM, tools 6.0, macOS 14, one product `rightkit-cloudkit-helper`; **`.package(path: "../RightKitSwift")`** (`Package.swift:13-15`); line-JSON `rightkit-sync-helper/1` over stdin/stdout for Rust `rightkit-sync`; 3 files (Session 298, Zone 285, main 55); no tests (Rust `tests/helper_e2e.rs` drives it); entitlements template only | 638 |
| `ios-app-template` | XcodeGen app + share ext + widgets + watch app + XCUITest only; iOS 17 / watchOS 10; `exactVersion: 0.3.0` (`project.yml:16`); 9 RightKitSwift products consumed; no Swift 5/6 setting declared (Xcode default); guard script `scripts/check-package-pin.sh` | 224 |
| `scraperight-ios-spike` | XcodeGen app + 1 XCTest file; **`path: ../RightKitSwift`** (`project.yml:12`) contradicting its own header and the exact-pin rule; products Artifacts, Diagnostics, License | 120 |
| `contracts/` | `license-v2.json` + Node parity test | n/a |
| `scripts/archive-package.mjs` | archive-source helper + test | n/a |
| `README.md` / `MAC-EXTRACTION-CHECKLIST.md` | extraction contract and Mac gate | n/a |

### 1.2 Dependencies

SwiftPM: none. System frameworks (all consumed): Foundation, CryptoKit, Security, LocalAuthentication, zlib (SDK `-lz`), CloudKit, Network, UniformTypeIdentifiers, Combine, StoreKit, UIKit, AppKit, SwiftUI, ActivityKit, WatchConnectivity, CoreML, AVFoundation, AVFAudio, Speech, Accelerate, CoreFoundation, Darwin.

Verdict: 22 frameworks, **needed 20, heavy 2 (CoreML, Accelerate, both only inside RightKitSpeech), unused 0, replace 0**. Package pin state: declared ranges n/a, no `Package.resolved` (library).

### 1.3 RightKit adoption and duplication

RightKitSwift is the Apple-side counterpart of Rust owners; it does not call them. Cross-runtime parity (not code duplicates):

| Swift target | Rust/TS owner | LOC | Drift guard |
|---|---|---:|---|
| RightKitLicense (verifier, activation, lease queue) | `rightkit-license`, `@rightkit/license` | 1534 | shared `contracts/license-v2.json` (`contracts.test.mjs`) |
| RightKitDiagnostics (JSONL, redactor) | `rightkit-logs` (`collect.rs:93-186`) | 150 | none (schema may diverge) |
| RightKitSpeech (Parakeet/Whisper CoreML, VAD) | `rightkit-asr`, `rightkit-vad`, `rightkit-audio` | 3915 | none; "parity port of Rust" comments (`ParakeetRuntime.swift:141,175`) |
| RightKitAppleCredentials | `rightkit-secrets` (macOS Keychain) | 373 | none |
| RightKitOverlay (state/placement) | `rightkit-overlay` | 166 | none |
| RightKitCloudSync (+ helper) | `rightkit-sync` | 2186 + 638 | helper e2e |

RightKit gaps (what apps need that RightKitSwift lacks): hotkeys/event taps/AX (Rust `rightkit-hotkeys` has Carbon + CGEventTap but no C ABI), updater (no Swift updater; only `@rightkit/updates` npm), SQLite, process lifecycle (`rightkit-process`), settings (`rightkit-settings`), crash reporting, foreign-Keychain read, Windows. **No Rust crate exports a C ABI** (`extern "C"` hits are all inbound FFI; zero `cdylib`/`no_mangle` exports in `rightkit/crates`).

### 1.4 Module-boundary findings

1. **Brand leakage into shared surface.** `RightKitOverlayKind` cases `heardRightPill` / `cockpitNotch` (`Overlay.swift:4-5,143,147`) contradict the extraction rule "No `heardright`... may enter the package" (`swift/README.md` Extraction rules). Also source comments cite HeardRight/CodeRight/ScrapeRight in RightKitSpeech, Diagnostics, CloudSync (provenance, harmless). Rename must keep JSON raw values (kind is `Codable`, shared with React pills).
2. **Mixed charter.** `Package.swift:4-5` says the package "never" owns app UI, but it ships RightKitUI (SwiftUI) and RightKitOverlay (NSPanel). README opening line still says "iOS apps" while Overlay is macOS-only and CloudKitHelper is macOS-only.
3. **RightKitSpeech is a monolith with one importer** (see section 7): 3915 LOC and 103 KB resource, of which about 674 LOC (AppleSpeechEngine 84, Dictation 39, LiveDictationSession 265, VoiceActivityDetector 70, SpeechError 56, SpeechProviders 47, AudioConditioning 59, AudioFileLoader 54) are light and wanted by ScrapeRight/CodeRight copies, and about 3241 LOC is Core ML Parakeet/Whisper used only by HeardRight.
4. **RightKitIntents** (139 LOC) is a recording-driver protocol + Darwin-notification bridge (`RightKitRecordingAction`: start/stopAndCopy/cancel/toggle/copyLatestTranscript) that is HeardRight product shape; README rule says App Intents are product behavior. Keep only if a second recording app exists.
5. **Concurrency:** package is Swift 6 mode, 28 `@unchecked Sendable` declarations (grep; clusters: Speech 9, CloudSync 4, Store/ImportQueue 5, License 4, Purchases 3, Credentials 3 incl. the `#if` twin, WatchBridge 1). The Speech engines and the two `SecurityAppleCredentialBackend` `#if` twins are the highest-risk (mutable Core ML state / Keychain handles). Consumers in Swift 5 mode (SellRight, ScrapeRight, HeardRight project.yml `SWIFT_VERSION 5.0`) see these as plain; Swift 6 consumers (CodeRight) get checked.
6. **iOS-only tests on macOS host:** StoreKit tests are skipped by `swift test` and need the dedicated scheme (README "Real StoreKit tests"); `RightKitSwift-Package` scheme cannot run tests on iOS because other targets use `Process`. Acceptable but means "195 passed" is macOS-only.
7. **Package.swift lists products for features guarded by `canImport`** (watch/ActivityKit) on iOS/macOS only platforms; no watchOS platform entry means HeardRight's watch app consumes `RightKitWatchBridge` through the platform's implicit minimum; verify before relying on it (unverified, no watch build run).
8. **Persistence format compat notes:** License store keys and Keychain service strings are injected by apps (safe); `RightKitDiagnostics` JSONL file name/format is app-chosen (SellRight uses `diagnostics.jsonl` in Application Support); CloudSync scope hash `sha256(container|zone|userRecordName)` is shared with the Rust helper and **must not change** (`CloudKitSyncTransport.swift:190`); `adoptLegacyLedger` exists for pre-RightKit ledgers.
9. **Docs drift:** `docs/RIGHTKIT-SHARED-PACKAGES.md:155` lists `0.3.0` and 9 products; `MAC-EXTRACTION-CHECKLIST.md` still speaks of a "Swift package registry" (steps 5-6) whereas distribution is git tag; `ios-app-template/project.yml:12-16` comment says tag 0.3.0 "NOT yet published" but the tag exists; `scraperight/macos-native/README.md` says helper declares macOS 13 but `Package.swift:7` is macOS 14.

### 1.5 Handover list (RightKitSwift chat)

1. Publish 0.3.8 (tag via `right-release publish swift`) after a fresh receipt at current HEAD; then edit `rightkit-versions.json` `swift` to the tag actually adopted (0.3.7 now, 0.3.8 after) and `stagedSwift`; est. 6 JSON lines + tag; blocker: publication-gate receipt commit mismatch, Adrian's explicit publish request.
2. Fix stale docs (`RIGHTKIT-SHARED-PACKAGES.md:155,196`, checklist registry wording, template comment `project.yml:12-16`, macos-native README macOS 13); est. 40 doc lines; blocker: none.
3. Make `scraperight-ios-spike/project.yml:12` an exact URL tag pin (it is the "no copied source from the registry" proof); est. 4 lines; blocker: needs a published tag (0.3.7 works).
4. Neutralize `RightKitOverlayKind` names with deprecated aliases, raw values unchanged; est. 15 LOC; blocker: HeardRight (and Cockpit/Pulse) source compat.
5. Split RightKitSpeech into `RightKitSpeech` (Apple speech, dictation, VAD, providers; about 674 LOC) and `RightKitSpeechCoreML` (about 3241 LOC + bin); est. 0 net LOC, Package.swift +6 lines, file moves; blocker: HeardRight must re-point product names.
6. Add `CHANGELOG.md` to `RightKitSwift/` (README sections are today's only record); est. 80 lines; blocker: none.
7. Decide RightKitIntents ownership (keep neutral or move to HeardRight); est. 139 LOC moved; blocker: second consumer decision.
8. Add a Swift-side drift test for Diagnostics redaction vs `rightkit-logs` (shared vector like license-v2); est. 120 LOC + JSON vector; blocker: Rust schema owner.

## 2. Target B: SwiftRight SDK

Repo `/Volumes/D/claude/swiftright` (own git, last commit d4d2ca4 2026-10-10; 26 Swift manifests incl. SR0 probes, 7 in scope). Architecture v0.5 (`docs/architecture.md`, 421 lines). **None of the in-scope packages depend on RightKitSwift or any SwiftPM third-party package**; all dependencies are `path:`.

### 2.1 Shape

| Package | Tools / platforms / Swift mode | Targets (LOC) | UI | Storage | Native boundary | Tests | Last commit |
|---|---|---|---|---|---|---|---|
| `packages/SwiftRightBridge` | 6.0 / macOS 14 / **`swiftLanguageModes: [.v5]`** (`Package.swift:34`) | SwiftRightBridge 1113 (Bridge 487, Marshal 251, Types 307, Loader 68), CSwiftRight system lib (770-line `swiftright.h`, copy of `render/swiftright-abi/include/swiftright.h`, checked by `scripts/check-bridge-header.sh`), CDispatchPrivate C shim (1 line + header) | none (window/tree primitives) | none | **C ABI v0.6 to Rust `swiftright_render.dll` (GPUI 0.2.2 + AccessKit), loaded at runtime by `LoadLibraryW`/`GetProcAddress`, Windows only**; Mac `Bridge.load` throws `.unsupportedPlatform`. Uses private libdispatch SPI (`_dispatch_main_queue_callback_4CF`) | 1 file / 8 XCTest (104 LOC) | 33987c2 2026-10-10 |
| `packages/SwiftRightServices` | 6.0 / macOS 14 / **Swift 6 mode** (README) | SwiftRightServices 1040 (EngineClient 509, EngineWire 264, EngineConfiguration 95, EngineProcess 70, EngineJSON 62, EngineError 40), FakeEngine exe 162 | none | none | **Foundation `Process` + stdin/stdout pipes to Rust engines speaking the `rightkit-framed-sidecar` envelope** (JSON lines or 4-byte length prefix) | 4 files / 31 tests (29 + 2 real-engine opt-in via `SWR_REAL_ENGINE`), 499 LOC; Mac 29/29, Windows 29/29 per README | f6e6828 2026-10-10 |
| `examples/BridgeGeoModal` | 6.0 / macOS 14 / v5 | TextGeom06 753, Modal06 629 | Win32 via bridge | none | Bridge ABI 0.6 | harness-driven (PowerShell UIA), 0 XCTest | 840f9ec 2026-10-10 |
| `examples/SliceApp` | 6.0 / macOS 14 / v5 | SliceApp 973 (BridgeBackend 566, EditorModel 126, EditorScreen 60, RowStore 61, Windows glue) + qa 99 | **SwiftUI on macOS; SwiftRightRuntime + Bridge on Windows** (`condition: .when(platforms: [.windows])`) | one JSON file `rows.json` schema 1 in Application Support / `%LOCALAPPDATA%` (`RowStore.swift:6-22`) | Bridge on Windows | none (qa scripts, `rightkit-qa.toml`) | d4d2ca4 2026-10-10 |
| `examples/BridgeSmoke` | 6.0 / macOS 14 / v5 | BridgeSmoke 728 | none | none | Bridge ABI | harness | 840f9ec |
| `probes/sr1-win-backend` (SR1Gallery) | 6.0 / macOS 14 / v5 | SR1Gallery 1946 | SwiftRightRuntime on Windows | none | Bridge | self-test | 6b953b5 2026-10-09 |
| `probes/sr1-pulse-probe` | 6.0 / macOS 14 / v5 for PulseProbe target | SwiftRightUI shim 3 + stubs 347 + copied Pulse files (out/src, uncommitted by design) | SwiftUI source compiled against runtime; expected to fail | none | none | none | 076a56f 2026-10-09 |

Not in the requested list but the load-bearing dependency of 4 of the 7: `probes/sr0-s/fresh-runtime` (module `SwiftRightRuntime`, 7663 LOC, **Swift 6 mode**, 84 tests, product of probe SR0-S). `render/swiftright-render` is Rust: cdylib `swiftright_render` v0.0.3, `publish = false`, Windows-only deps `gpui =0.2.2`, `accesskit =0.25.1`, `accesskit_windows =0.35.1`, `image =0.25.10`, `raw-window-handle =0.6.2` (`Cargo.toml:14-25`).

### 2.2 Dependencies

SwiftPM third-party: 0. Internal path deps: Bridge consumed by 4 packages; fresh-runtime by 3. System/toolchain imports: Foundation (needed), WinSDK (Windows only, needed), Darwin/Glibc (Services, needed), SwiftUI/AppKit/CoreGraphics/Observation (SliceApp macOS path, needed), XCTest (tests). Verdict counts across B: **needed 9, unused 0, replace 2, heavy 0**. The 2 "replace" verdicts:
- `SwiftRightServices/EngineProcess.swift` (`Process`, 70 LOC) and restart/stderr-tail logic inside `EngineClient.swift` replace-with `rightkit-process` (OwnedChild tree kill, RestartPolicy `restart.rs:11`, HealthPolicy, StderrTail `stderr_tail.rs:13`, env allowlist `owned.rs:346`) via a future C ABI. Gaps 5, 6, 8 in `packages/SwiftRightServices/README.md` ("Gaps to report to RightKit owners").
- `SliceApp/Shared/RowStore.swift` (61 LOC JSON persistence) replace-with `rightkit-settings` / `rightkit-db` via C ABI (architecture §7 table), but an example, so low priority.

### 2.3 RightKit adoption and duplication

SwiftRight consumes RightKit at the **protocol** level (framed-sidecar wire), at the **tooling** level (`@rightkit/release` 0.2.146 in `examples/SliceApp/package.json`, `rightkit-qa.toml`, `right-release.config.mjs`) and not at the code level. Architecture §7 says "Mac consumes Apple-qualified RightKitSwift where available; other Rust-owned capabilities use the thin `swiftright-services` C ABI library" (`architecture.md:214`); neither dependency exists yet (`services/swiftright-services` directory does not exist; section 7 table is "intended owners, not claims").

| In-target code | LOC | RightKit owner | Status |
|---|---:|---|---|
| `EngineWire.swift` (envelope, framing) | 264 | `rightkit-framed-sidecar` (`wire.rs`, `lib.rs`, `length_prefixed.rs`) | deliberate independent Swift peer; wire derived read-only; 10 gaps listed by the author |
| `EngineProcess.swift` + restart/stderr logic in `EngineClient.swift` | ~70 + ~120 | `rightkit-process` | duplicate; Windows grandchildren can survive shutdown (no job object) |
| `EngineConfiguration`, `EngineError`, `EngineJSON` | 197 | `rightkit-service` / `rightkit-speech-client` (typed errors) | adapter surface, keep |
| `RowStore.swift` | 61 | `rightkit-settings` | example only |
| FakeEngine | 162 | `rightkit-jobs` `engine_fixture` | test peer, keep |
| Bridge, Marshal, Types, Loader | 1113 | none (SwiftRight-owned) | n/a |

Duplicates total (candidates to retire after a C ABI exists): about 520 LOC (EngineWire 264 + process/restart about 190 + RowStore 61).

RightKit gaps SwiftRight needs: (a) C ABI for `rightkit-process`/`-secrets`/`-settings`/`-db`/`-hotkeys` (none exported); (b) `ready` frame and cancel frame in framed-sidecar (README gaps 2, 4); (c) no Swift client for `rightkit-service` discovery; (d) a Windows-capable SecureStore (RightKitAppleCredentials is Apple-only; `rightkit-secrets` has the Windows side); (e) Swift/MSIX packaging support in `right-release` is already wired through `swift-package.json` + SliceApp config (gaps noted in `right-release.config.mjs` header).

### 2.4 Module-boundary findings

1. **Mixed language modes.** Bridge, BridgeGeoModal, BridgeSmoke, SliceApp, SR1Gallery are **Swift 5 mode** under tools 6.0; Services and fresh-runtime are **Swift 6**. The bridge's public API is non-Sendable `final class Bridge` / `BridgeWindow` with C function tables and `unsafeBitCast` (`Loader.swift:44,55`); a Swift 6 consumer (runtime in v6) must cross it via `@MainActor`/`MainActor.assumeIsolated` (SliceApp does: `SliceAppEntry.swift:31`). Decide per module: Bridge should become Swift 6 with explicit `@MainActor` (the dispatch drain must run on the bound initial thread, `CDispatchPrivate.h:13-15`) before a second consumer appears.
2. **Runtime lives under `probes/`.** SliceApp and SR1Gallery depend on `../../probes/sr0-s/fresh-runtime`; architecture §4 names `SwiftRightCore`/`SwiftRightUI` packages which do not exist yet. Promote `SwiftRightRuntime` into `packages/` before SR2 so examples do not depend on probe directories.
3. **Private SPI.** `CDispatchPrivate` binds `_dispatch_get_main_queue_handle_4CF` / `_dispatch_main_queue_callback_4CF` by ordinal (`CDispatchPrivate.h`); toolchain upgrades (Swift 6.4 pinned) can break it. Already gated by the SR0 tuple pin; keep that pin.
4. **Header duplication** is guarded by `scripts/check-bridge-header.sh` (copy not symlink because RightKit ownership preflight rejects committed symlinks); consider a CI/qualification step that runs it (not verified to run).
5. **Services has no consumer** in-tree besides tests; its public API is stable enough to publish but "No published Swift version" (gap 10).
6. **Persistence format:** SliceApp `rows.json` schema 1 with `RowFile{schema,rows}` and `unsupportedSchema` fail-closed (`RowStore.swift:24-37`); upgrade/rollback builds must both read it (PACKAGING.md). Keep.
7. **Composition roots are clean**: macOS `SliceAppMain` builds `RowStore` then `EditorModel(store:)` (`SliceAppEntry.swift:6-14`); Windows entry calls `sliceAppWindowsMain` on the initial thread. No global singletons found in the sampled sources (grep for UserDefaults/Keychain/Process in packages: none).

### 2.5 Handover list (SwiftRight chat)

1. Swift-side consumption of RightKitSwift on Apple only: add `.package(url: ".../rightkit-swift.git", exact: "0.3.7")` guarded with `condition: .when(platforms: [.macOS])` for Diagnostics/AppleCredentials adapters when SR2 needs them; est. 25 LOC manifest + adapter; blocker: SR2 scope, RightKitSwift macOS 14 matches SwiftRight macOS 14 (compatible).
2. Replace EngineProcess + restart/health/stderr-tail → `rightkit-process@0.3.3` via C ABI; est. -190 LOC +90 LOC binding; blocker: no C ABI exported by any RightKit crate; needs `swiftright-services` cdylib + owner approval for ABI surface.
3. Replace EngineWire constants/semantic drift risk → shared wire vector test against `rightkit-framed-sidecar@0.1.0`; est. 150 LOC test + JSON fixture; blocker: framed-sidecar owner to publish vectors (gaps 1, 2, 4).
4. Promote `fresh-runtime` to `packages/SwiftRightRuntime`; est. 0 LOC (move); blocker: SR gate ordering.
5. Move Bridge to Swift 6 mode with `@MainActor`-isolated `Bridge`; est. 150 LOC edits; blocker: re-qualify SR0 tuple (receipts under `qualification/`).
6. Replace `RowStore.swift` → `rightkit-settings@0.1.0` through C ABI once available; est. -61 LOC; blocker: C ABI.
7. Publish SwiftRightServices as an exact-tag Swift package (gap 10); est. release config only; blocker: SwiftRight release policy.

## 3. Target C: Pulse Mac (`/Volumes/D/claude/pulse/mac`)

Two independent Swift codebases in one directory.

### 3.1 Shape

| Item | PulseMacPrototype (SwiftPM) | Notch (XcodeGen app "Pulse", Codenotch fork) |
|---|---|---|
| Manifest | `Package.swift`, tools **5.9**, macOS **13**, no language-mode setting (Swift 5) | `Notch/project.yml`; `SWIFT_VERSION 5.0`, `SWIFT_STRICT_CONCURRENCY: minimal`; deployment **macOS 26.0**, hardened runtime on, sandbox off, Xcode 27 |
| Targets / LOC | PulseMacPrototypeCore 8201 (12 files), PulseProbe exe 120, tests 287 / **7 tests** (3 files) | Pulse app 35,504 LOC (155 files, `Sources/`), PulseFinder Finder Sync ext 117, PulseHelper privileged tool 213, pulse-elevate tool 38, Shared 14; vendored C: `zstddeclib.c` via bridging header |
| UI | none (probe CLI) | SwiftUI + AppKit (`NSPanel` notch, 51 files import AppKit, 33 SwiftUI), LSUIElement, 13 localizations |
| Storage | n/a | UserDefaults (9 files, `Preferences.swift` 980 LOC), JSON files `notch-state.json`, `hub-commands/`, `drive-health.json` under `~/Library/Application Support/Pulse/`, SQLite3 read of Codex DB (`SQLiteStore.swift`, `CodexLocalProvider.swift`), Keychain read of Claude Code OAuth item (`KeychainItem.swift` 157, `ClaudeCredentials.swift` 280, `KeychainPrompt.swift` 162) |
| Native boundary | none to Rust | **Process()** 14 files (hub = Tauri child `pulse-hub` via `Foundation.Process`, `HubLauncher.swift:251 LOC`), Darwin notifications + JSON files to the Rust/Tauri hub (`HubBridge.swift` 486), `SMAppService.daemon` privileged helper + `pulse-elevate` client for the Rust core (`PrivilegedHelper.swift`, `Helper/`), CGEventTap (1 file), Carbon `RegisterEventHotKey` (2 files) |
| Tests | 7 | **none** (`FORK.md` "Codenotch's unit tests not carried over") |
| CI | not built in CI (README: depended on removed dashboard host `ScanRequest`, `ProcessScanRunner`) | CI-only on `xcode-27` runner (`scripts/gate.sh`) |
| Last commit (`mac/`) | 7edb5a58 2026-10-10 |

### 3.2 Dependencies

SwiftPM: **none** in either (Sparkle and SwiftNIO deliberately removed; Sparkle appears only in comments `Preferences.swift:939`, `SessionCompletionWatcher.swift:36`, `project.yml:13`). Frameworks (Core): Foundation, Darwin, AppKit, UniformTypeIdentifiers, IOKit(.ps), ImageIO, CoreServices, CoreGraphics, AVFoundation, CoreFoundation. Frameworks (Notch): Foundation 93 files, AppKit 51, SwiftUI 33, Combine 15, ApplicationServices 12, Darwin 11, Security 5, Carbon.HIToolbox 5, os 4, CoreGraphics 4, ServiceManagement 3, IOKit 3, UserNotifications 2, SQLite3 2, CoreServices 2, SystemConfiguration 1, CryptoKit 1, AVFoundation 1.

Verdicts (28 imports + 1 vendored lib): **needed 21, replace-with 4, heavy 2, unused 2**:
- replace-with: Carbon.HIToolbox hotkeys -> `rightkit-hotkeys` (needs C ABI); `ServiceManagement`/SMAppService helper has no RightKit owner (keep, so counted needed); `import os` Loggers (2) -> `RightKitDiagnostics` for file diagnostics; UserDefaults `Preferences` -> no direct RightKitSwift product (Rust `rightkit-settings` needs ABI); Updater stack -> `@rightkit/updates` policy (no Swift product).
- heavy: vendored `zstddeclib.c` (Claude Desktop cache decode, justified in `Sources/Vendor/zstd/README.md`); `SystemConfiguration`/IOKit private HID sensor client (private API, `System/SystemSensors.swift`).
- unused: `PulseMacPrototypeCore` module as a whole is dead for the shipping app (not built in CI; zero references from Notch), including `NativeApplicationUpdates` 736, `NativeHomebrewUpdates` 410 (counted as 2 unused frameworks: AVFoundation/ImageIO are only used by `MediaCompression`, 697 LOC).

### 3.3 RightKit adoption and duplication

RightKitSwift consumed: **none** (no `packages:` in `Notch/project.yml`). Duplicates in the shipping Notch app:

| In-target code | LOC | Target owner |
|---|---:|---|
| `Updater/*` (ReleaseFeed 122, Download 100, Verifier 100, Installer 95, Relauncher 91, SemanticVersion 34) | 542 | `@rightkit/updates` policy + `right-release` feed; no Swift product exists (gap) |
| Hotkeys: `LauncherHotKey` 140, `Windows/WindowHotKeys` 161, `Windows/WindowShortcut` 90, `EventTapHub` 123, `Keyboard/FnCommand` 89 | 603 | `rightkit-hotkeys` (Carbon + CGEventTap + fn tap + chord parsing); needs C ABI/Swift binding |
| Notch panel/placement: `NotchPanel` 94, `NotchPlacement` 78, `NotchHostingView` 101, parts of `NotchGeometry` 470 | ~270 | `RightKitOverlay` (`RightKitOverlayPanel`, `.cockpitNotch` kind, work-area clamp, hit test) and `rightkit-overlay`; NotchPanel has extra right-click/drag handling so only the base panel replaces |
| `Preferences` 980 + `HubBridge` 486 (settings sync to Tauri hub via JSON + Darwin notifications) | 1466 | `rightkit-settings` (versioned JSON document per namespace); needs binding |
| `HubLauncher` 251 + 14 `Process()` sites | ~400 | `rightkit-process` (child ownership/tree termination), `rightkit-shell` presence/broker |
| Keychain foreign-item read (`KeychainItem` 157, `KeychainPrompt` 162, `ClaudeCredentials` 280) | 599 | partially `RightKitAppleCredentials`; reading another app's item is a gap (probe API only supports own service) |
| `SQLiteStore` | ~100 | `rightkit-db` is Rust; no Swift owner |
| `Providers/ClaudeAccountWatcher`, `System/PulsePermissions` 277, `DriveHealth` | n/a | product, no owner |

Prototype core duplicates: `NativeFilenameIndex` 1001 vs `rightkit-fsindex` (`publish = false`, working tree dirty per git status); `NativeCleanup` 1220 (and `NativeStorageServices` 763, `NativeDuplicateRevalidation` 524) overlap Pulse's Rust core plan/apply (README: "bypasses core plan/apply checks and must not ship as-is"); `MediaCompression` 697 and `NativeApplication*`/Homebrew 3,000+ have no RightKit owner.

Estimated duplicate LOC in shipping app: **2,900 (hotkeys 603 + updater 542 + settings/bridge 1466 + overlay ~270)**; plus 2,225 LOC of dead prototype that duplicate/overlap owners.

### 3.4 Module-boundary findings

1. **God-objects:** `NotchWindowController` 2,190 LOC, `NotchViewModel` 1,343, `TooltipCard` 1,089, `ClaudeOAuthProvider` 1,011, `NearbySharing` 996, `UsageStore` 989, `AppDelegate` 960 (composition root plus first-run/login-item/bridge wiring). One app target with 155 files and no local package boundaries; splitting candidates by evidenced ownership: `Conveniences/` (2,692 LOC, 3 features + hotkeys), `Launcher/` (2,363), `Providers/` (5,713). Not recommended before tests exist.
2. **Zero tests for 35k LOC** while the dead prototype has 7. Priority risk, especially the disk-image installer (`DiskImageInstaller` 592 + `DiskImageWork` 527, destructive `ditto`/rename flows).
3. **Concurrency:** Swift 5 mode, `SWIFT_STRICT_CONCURRENCY minimal`, 86 `@MainActor` uses across 52 files, 12 `@unchecked Sendable` in 10 files, own `EventTapHub` thread, IOKit sampling "off the main actor". Moving to Swift 6 mode needs the Preferences (UserDefaults-backed `ObservableObject`) and event-tap handler boundaries to be isolated first.
4. **Deployment mismatch:** prototype macOS 13 vs RightKitSwift macOS 14 (blocks any RightKitSwift adoption in that package); Notch macOS 26 is compatible.
5. **Persistence compat:** `notch-state.json` + `hub-commands/` + Darwin notification names are a wire contract with the Tauri hub (`hub/src-tauri/src/permissions.rs`, `hub/src/views/Settings.tsx` per FORK.md); `drive-health.json` and UserDefaults keys (`conv*`, `launcherConfig`, `windowHotkeys`) are user data; any move to `rightkit-settings` needs a one-time reader of the old keys.
6. **Inert updater stub** documented in FORK.md ("Sparkle removed; Updater is an inert stub") while `Updater/` has 542 LOC of real code: FORK.md is stale on this point (unverified which is in the build; `UpdateInstaller` etc. exist in tree).

### 3.5 Handover list (Pulse chat)

1. Delete or quarantine `PulseMacPrototype` (8,321 LOC incl. probe) after confirming nothing salvaged is pending; est. -8.3k LOC; blocker: Pulse plan "salvage review" (`docs/plan.md`) owner decision.
2. Replace hotkeys (`LauncherHotKey`, `WindowHotKeys`, `WindowShortcut`, `EventTapHub`, `FnCommand`) -> `rightkit-hotkeys@0.x` via C ABI; est. -603 +120 LOC; blocker: no C ABI/binding; crate publication per feed.
3. Replace `NotchPanel`+`NotchPlacement` base class -> `RightKitOverlay@0.3.7` (`RightKitOverlayPanel(kind: .cockpitNotch)`, `RightKitOverlayWorkArea.clamping`); est. -170 +30 LOC; blocker: RightKitSwift products need macOS 14+ (ok), Pulse must add `packages:` exact pin and keep right-click/drag subclass.
4. Replace updater stack -> RightKit updates path (`@rightkit/updates` + `right-release` feed) with a Swift client; est. -542 +150 LOC; blocker: no Swift updater product in RightKitSwift (gap to be filed with RightKit owner).
5. Add tests for disk-image installer, Preferences migration and HubBridge JSON before any refactor; est. +600 LOC; blocker: Notch has no test target (`project.yml`).
6. Replace Preferences/HubBridge JSON -> `rightkit-settings@0.1.0` through binding; est. -1,466 +300 LOC; blocker: binding and migration of existing keys.
7. Replace `Process()` hub launch/helper spawn -> `rightkit-process@0.3.3`/`rightkit-shell`; est. -250 +80 LOC; blocker: binding.

## 4. Target D: ScrapeRightAppleCredentials helper

| Item | Value |
|---|---|
| Shape | SwiftPM executable package, tools 6.0, macOS 14 (`Package.swift:7`), **`swiftLanguageModes: [.v5]`** (`:22`), single file `Sources/ScrapeRightAppleCredentials/main.swift` 352 LOC, no tests, last commit 13978b3 2026-10-04 |
| Native boundary | spawned **per request** by Rust/Tauri (`src-tauri/src/pro_bridge.rs`, `helper_process.rs` path+spawn+bounded wait), one JSON line on stdin, one JSON line on stdout; runs inside a signed nested login-item `.app` carrying `keychain-access-groups` + provisioning profile (`macos-native/README.md`) |
| UI / storage | none; synchronizable iCloud Keychain item (service `app.scraperight.shared.credentials`, accounts `pro.bridge`, `sync.consent`, group `6KLGD3LLKF.app.scraperight.shared`) |
| Dependencies | `rightkit-swift` exact **0.3.7** (`Package.swift:11`), `Package.resolved` revision 48e08799 version 0.3.7 (matches declared exact pin; v3 file). Products used: `RightKitAppleCredentials` only. Imports: Foundation (needed), Security (direct, partially replaceable), RightKitAppleCredentials (needed). Verdicts: **needed 2, replace-with 1 (Security direct calls), unused 0, heavy 0** (resolution pulls the whole 14-product repo checkout though only one product builds) |
| Adoption | Uses `SecurityAppleCredentialBackend.read/write/delete/legacyReadAnyScope` (`main.swift:237-270`). The comment at `main.swift:174-175` says 0.3.7 "cannot express this helper's exact status probe or broad deletion", and 0.3.8 adds exactly `probeStatus` (`CredentialProbe`) and `deleteAnyScope` (`[CredentialDeletionOutcome]`). Duplicate in-target code: `baseQuery` (12), `probeStatus` (about 23) and `deleteEverywhere` (about 15 incl. `SecItemDelete` with `kSecAttrSynchronizableAny`) = **about 50 LOC** (lines 181-222, 268-282) |
| Boundary findings | Per-request process is a deliberate design (no daemon). Swift 5 mode is fine for a 352-line script but a 6.0 manifest should say so on purpose. Request/response structs (`Request`, `StatusResponse`, ...) are an ad hoc line-JSON protocol parallel to the helper protocol in `RightKitCloudKitHelper` (`rightkit-sync-helper/1`) and `rightkit-framed-sidecar`; consider whether a one-shot request/response envelope belongs in RightKit. README claims helper declares macOS 13 (stale; manifest says 14). Persistence compat: Keychain service/accounts/access group are frozen to match iOS `ios-native/Shared/AppleSyncedCredentialStore.swift`; do not change |

Handover (ScrapeRight chat):
1. Replace `probeStatus` + `deleteEverywhere` + `baseQuery` -> `RightKitAppleCredentials@0.3.8` `probeStatus`/`deleteAnyScope` (publish first); est. -50 +20 LOC; blocker: 0.3.8 is unpublished (latest tag 0.3.7); also decide status mapping (`CredentialProbe.rawStatus`) and keep `kSecAttrSynchronizableAny` semantics.
2. Fix `macos-native/README.md` macOS 13 claim and the "SwiftPM compatibility check blocks compilation" paragraph; est. 10 doc lines; blocker: none.
3. After 0.3.8 pin, move `swiftLanguageModes` to default 6 or document v5 intentionally; est. 2 lines + warnings; blocker: strict-concurrency fixes for `readLine` top-level code.

## 5. Target E: iOS admin apps and other RightKitSwift pinners

Raw `grep -rn` (unignored) of `rightkit-swift` in Package.swift/project.yml/Package.resolved/project.pbxproj finds: SellRight, ScrapeRight iOS and macOS helper, RightSites, CodeRight iOS, HeardRight iOS (+2 E2E packages), the `heardright-release-0.1.167` snapshot, and the template (full list in appendix).

### 5.1 SellRight admin (`/Volumes/D/claude/sellright/ios`) and RightSites admin (`/Volumes/D/claude/rightsites/ios`)

| Item | Value |
|---|---|
| Shape | XcodeGen (`project.yml`) iOS app `SellRightAdmin` + widget extension `SellRightWidgets` + unit tests; **iOS 17.2**, `SWIFT_VERSION: "5.0"` (`project.yml:13`), SwiftUI (13 files) + a single UIKit import (`PushManager`), Observation, ActivityKit/WidgetKit Live Activity; targets: app 2,465 LOC (20 files), widgets 151, Shared 72, Tests 162; 10 XCTest funcs in `ModelsTests`/`ThemeTests`; last commit (both repos share history) 21f3daa 2026-07-19 |
| Storage | `UserDefaults` for serverURL/storeSlug/theme (`AppSession.swift:20,28,61,122`, `Theme.swift:159-166`), Keychain admin token (`Keychain.swift`, 45 LOC, service `app.sellright.ios.admin`, `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`), JSONL diagnostics `diagnostics.jsonl` |
| Native boundary | none (REST `/v1/admin/*` over URLSession, APNs push, ActivityKit push-to-start) |
| RightKitSwift | `url: https://github.com/bogusyogi/rightkit-swift`, **`from: 0.1.0`** (`project.yml:17-19`), resolved **0.1.0** (revision 7a15b150; `Package.resolved:5-11`); product `RightKitDiagnostics` only (`Core/Diagnostics.swift`). `from:` violates the exact-tag rule (`docs/RIGHTKIT-SHARED-PACKAGES.md:85`, template guard `check-package-pin.sh`) and would resolve to 0.3.7 on a fresh resolve (caret range `<1.0.0`) |
| RightSites | `rightsites/ios` is a **verbatim fork**: `diff -rq` shows zero source differences, `project.yml` identical (same bundle ids `app.sellright.ios.admin`, team 6KLGD3LLKF, README, "SellRightAdmin" names); it lacks a committed `.xcodeproj`, `Info.plist`, entitlements (generated) |
| Dependencies | SwiftPM 1 (rightkit-swift, **needed**); frameworks: SwiftUI, Foundation, ActivityKit, UserNotifications, UIKit, Security, Observation, WidgetKit. Verdicts: **needed 8, replace-with 1 (Security direct SecItem in Keychain.swift), unused 0, heavy 0** |

RightKit adoption and duplicates:

| In-target code | LOC | Owner / verdict |
|---|---:|---|
| `Core/Keychain.swift` | 45 | `RightKitAppleCredentials` `.device` scope (RightKit uses `AfterFirstUnlockThisDeviceOnly` for device scope, `RightKitAppleCredentials.swift:237`); caveat: RightKit store writes service `org.rightkit.shared.credentials` by default and uses data-protection keychain + optional access group; migration must read the legacy `app.sellright.ios.admin` item once |
| `Core/OrderActivityController.swift` + `Shared/OrderActivityAttributes.swift` + widgets | 37 + 28 + 151 | `RightKitLiveActivity` `ActivityLifecycle`/`LiveActivityHost` offers session binding, terminal restart, adoption and watchdog; SellRight's flow is server push-to-start + local updates, so adoption is optional and a fit check is needed |
| `Core/PushManager.swift` | 185 | no RightKitSwift owner (APNs registration); gap |
| `Core/APIClient.swift`, `Models.swift`, `App/Theme.swift` | 111, 329, 208 | product |
| `Shared/Money.swift` | 44 | product |

Handover (SellRight chat and RightSites chat, same list):
1. Move pin from `from: 0.1.0` to `exactVersion: 0.3.7` (then regenerate `Package.resolved`; only RightKitDiagnostics API used); est. 3 lines; blocker: `RightKitDiagnostics` API compat 0.1.0 -> 0.3.7 (not built here, unverified).
2. Replace `Keychain.swift` -> `RightKitAppleCredentials@0.3.7` `.device` scope with one-time legacy read; est. -45 +35 LOC; blocker: Keychain migration must not log out the operator; account/service names differ; sellright must add the product to `project.yml:28-31`.
3. Evaluate `OrderActivityController` -> `RightKitLiveActivity@0.3.7`; est. -37 +30 LOC; blocker: server-started activity (push-to-start) is not a lifecycle the package starts.
4. RightSites: de-brand the fork (bundle id, display name, App Group/keychain service, URL scheme, README) and decide whether it stays a copy or a shared Swift package with SellRight; est. 60 lines; blocker: product decision; today two repos ship an identical app with the same bundle id.
5. Raise `SWIFT_VERSION` to 6 after audit of `@MainActor` use (10 sites); est. 40 LOC; blocker: none.

### 5.2 Other iOS apps pinning RightKitSwift (shape + adoption only; owned by their app chats)

| App | Project / Swift / iOS | LOC (files) | Pin | RightKitSwift products | Notes (grep-level; not read in depth) |
|---|---|---|---|---|---|
| CodeRight iOS `coderight/apps/coderight-ios` | XcodeGen, `SWIFT_VERSION 6.0`, iOS 26.5; also TCA 1.26.1, MarkdownUI, local WasmKit | 71,177 (323) | generator `from: 0.1.0` (`project.yml:26-28`); generated pbxproj/Package.resolved **0.1.1** | License, Diagnostics | Local `License/LicenseKeychain.swift` 60, `Storage/KeychainStore.swift` 222, `LocalRuntime/*SecretStore` (MCP 149, Session 330, Secret 197), `DeviceStateStore` 409 use SecItem directly (~1,370 LOC) -> `RightKitAppleCredentials`; local `Dictation/SpeechDictation.swift` 244 + `Dictation.swift` 42 -> `RightKitSpeech` Dictation/LiveDictationSession; pin drift generator vs pbxproj |
| HeardRight iOS `heardright/ios-native` (+ `Tests/RightKitMobileE2E`, `Tests/RightKitSpeechE2E`) | XcodeGen, `SWIFT_VERSION 5.0`, `SWIFT_STRICT_CONCURRENCY complete` (project.yml:323), iOS 17 / watchOS 11 | 49,706 (324) | exact **0.3.2** | Speech, License, Purchases, CloudSync, Store, ImportQueue, LiveActivity, WatchBridge, Intents, AppleCredentials, Overlay, UI (12 of 14; Diagnostics and Artifacts not used) | Already adapter-style (`LicenseEntitlement.swift` typealiases RightKitLicense types; `TranscriptCloudSync.swift` header says RightKit owns transport). Remaining local: `DogfoodTraceStore.swift` 149 (JSONL) -> RightKitDiagnostics (Diagnostics not imported); `AccountSessionStore.swift` 552 SecItem; 0.3.2 -> 0.3.7 bump pending |
| ScrapeRight iOS `scraperight/ios-native` | XcodeGen, `SWIFT_VERSION 5.0`, iOS 17; + YouTubeKit `from: 0.4.8` | 45,741 (348) | exact **0.3.7** | AppleCredentials, Artifacts, CloudSync, Diagnostics, ImportQueue, License, Purchases, Store | Local `Extract/AppleSpeechTranscriber.swift` 48 + `ASREngine.swift` 51 + `MediaTranscriber.swift` 111 -> `RightKitSpeech` Apple engine; `ScrapeRight/Diagnostics.swift` 18 shim |
| `heardright-release-0.1.167/ios-native` | frozen release snapshot | 47,162 | exact 0.1.1 | n/a | archival copy; do not edit; counts toward stale-pin inventory |
| `rightkit/swift/ios-app-template` | see 1.1 | 224 | exact 0.3.0 | 9 products | pin stale vs 0.3.7 |

## 6. Cross-cutting answers

### 6.1 RightKitSpeech: stay or move to HeardRight

Importers (raw grep over Swift, project.yml, Package.swift, excluding RightKit tree, `.cache`, `tmp/` scratch copies): **only HeardRight** (`heardright/ios-native/project.yml:146`; `HeardRight/RustSpeechProviders.swift`, `LocalASRRuntimeStore.swift`, `OnDeviceTranscriber.swift`, `SharedRecordingCoordinator.swift`, `WarmAudioCapture.swift`; E2E `Tests/RightKitSpeechE2E/Package.swift:10`). ScrapeRight and CodeRight still carry local copies (section 5.2) of the Apple-speech/dictation pieces the package was extracted from (`AppleSpeechEngine.swift:5-6`, `Dictation.swift:3`, `LiveDictationSession.swift:6`).

Recommendation: **do not move the whole target; split it.** Keep a light `RightKitSpeech` (Apple speech, dictation, VAD, providers; about 674 LOC) in RightKitSwift because two other apps carry copies and would adopt it; move the Core ML Parakeet/Whisper runtime (about 3,241 LOC + 103 KB bin, one consumer, `@unchecked Sendable` heavy, duplicates Rust `rightkit-asr`/`-vad` algorithms with "parity port" comments) to a HeardRight-local package or a separate `RightKitSpeechCoreML` product that only HeardRight pins. If the adoption by ScrapeRight/CodeRight is abandoned, move everything to HeardRight (`swift/README.md` extraction rules already say Core ML/ASR are product behavior). Evidence for stay-or-split is the single importer plus the README rule; the decision is a product-ownership call for Adrian.

### 6.2 Does RightKitSwift need a Windows platform entry for SwiftRight?

**No.** (1) SwiftPM `platforms:` only sets Apple deployment minimums; there is no Windows deployment entry, and an omitted platform is not excluded from building. (2) SwiftRight's own architecture says Mac consumes Apple-qualified RightKitSwift and Windows uses the Rust-backed `swiftright-services` C ABI (`architecture.md:214`; `docs/RIGHTKIT-SHARED-PACKAGES.md:541` says Swift packages are Apple-only, macOS receipts only). (3) The sources cannot compile on Windows anyway: CryptoKit (License, Artifacts, CloudSync), Security, Combine, CloudKit, StoreKit, UIKit/AppKit, Darwin `flock` (Store), zlib import; only RightKitDiagnostics (Foundation-only) is plausibly portable and no Windows build was run (unverified). SwiftRight should consume RightKitSwift behind `condition: .when(platforms: [.macOS])` once an adapter needs it; a Windows entry would be both meaningless in the manifest and misleading about support. SwiftRight's macOS 14 minimum already matches RightKitSwift's.

## 7. Unverified items

- Nothing was built or tested; all test counts are grep counts except the receipt's 195 (RightKitSwift, macOS arm64) and the README claims for SwiftRightServices (29/29 Mac and Windows).
- Whether tag 0.3.7 file contents equal the local tree minus 0.3.8 changes (no diff of the remote tree was done); only `VERSION` was read at 0.3.6 and 0.3.7.
- watchOS consumption of RightKitWatchBridge/LiveActivity with no watchOS entry in `platforms:`.
- `RightKitDiagnostics` API compatibility 0.1.0 to 0.3.7 for SellRight/RightSites.
- Pulse Notch: which of `Updater/*` is wired in the shipping build; Carbon/event-tap counts are grep-level.
- CodeRight/HeardRight/ScrapeRight duplicate LOC numbers are file sizes of files matching SecItem/Dictation patterns, not line-level overlap.
- Whether `scripts/check-bridge-header.sh` runs in SwiftRight qualification.
- Whether SwiftRight's Windows/Mac receipts claimed in README are current (not re-run).

## Appendix A: raw tag list and feed

`gh api repos/bogusyogi/rightkit-swift/tags` (name, commit prefix): 0.3.7 48e08799bd; 0.3.6 484f89e8bd; 0.3.5 b634259b33; 0.3.4 887c777341; 0.3.3 be2a919089; 0.3.2 a139bcd901; 0.3.1 59da0a9c88; 0.3.0 e1e8b54638; 0.1.1 929844f1ff; 0.1.0 7a15b15067. `gh api repos/bogusyogi/rightkit-swift/releases`: empty. Repo `private: true`, default branch `main`. VERSION file content at tags 0.3.6 and 0.3.7 equals the tag name.

`rightkit-versions.json:321-332`: `"swift": {"rightkit-swift": {"url": "https://github.com/bogusyogi/rightkit-swift.git", "version": "0.1.1"}}`, `"stagedSwift": {... "version": "0.3.1"}`.

Raw-grep consumer files (`/usr/bin/grep -RIl rightkit-swift`, excluding node_modules, target, .build, .git, archive, tmp, .cache): sellright/ios {project.yml, project.pbxproj, Package.resolved}; scraperight/ios-native {project.yml, project.pbxproj, Package.resolved}; scraperight/macos-native/ScrapeRightAppleCredentials {Package.swift, Package.resolved}; rightkit/swift/ios-app-template {project.yml, project.pbxproj, Package.resolved}; rightsites/ios/project.yml; coderight/apps/coderight-ios {project.yml, project.pbxproj, Package.resolved}; heardright/ios-native {project.yml, project.pbxproj, Package.resolved, Tests/RightKitSpeechE2E/{Package.swift,Package.resolved}, Tests/RightKitMobileE2E/{Package.swift,Package.resolved}}; heardright-release-0.1.167/ios-native {project.yml, project.pbxproj, Package.resolved}.

## Appendix B: dump-package key facts (manifest parse only)

| Package | toolsVersion | platforms | swiftLanguageVersions | dependencies | targets |
|---|---|---|---|---|---|
| RightKitSwift | 6.0.0 | ios 17.0, macos 14.0 | null (Swift 6 mode) | none | 14 regular + 14 test; RightKitArtifacts linker `-lz` |
| RightKitCloudKitHelper | 6.0.0 | macos 14.0 | null | fileSystem `../RightKitSwift` | 1 executable |
| SwiftRightBridge | 6.0.0 | macos 14.0 | `['5']` | none | system CSwiftRight, regular CDispatchPrivate, regular SwiftRightBridge, test |
| SwiftRightServices | 6.0.0 | macos 14.0 | null (Swift 6) | none | regular, executable FakeEngine, test |
| PulseMacPrototype | 5.9.0 | macos 13.0 | null (Swift 5) | none | regular Core, executable PulseProbe, test |
| ScrapeRightAppleCredentials | 6.0.0 | macos 14.0 | `['5']` | sourceControl rightkit-swift exact 0.3.7 | 1 executable |

Host: Xcode 27.0 (27A266a), Swift 6.4, `/usr/bin` xcodebuild/xcrun/swift.

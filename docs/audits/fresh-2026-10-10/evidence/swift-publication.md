# Swift & publication static audit — 2026-10-10

Scope: read-only inspection of `/Volumes/D/claude/rightkit/swift/**`, all local Rust/npm manifests, local version ledger, local gate receipts, & non-cache consumer manifests. No network, build, test, install, publish, or source change occurred. Registry values below are **local records only; registry unverified**.

## Result

Publication readiness is blocked for every currently publishable RightKit unit: gate code requires receipt commit equal current `HEAD`; 59 local receipts name another commit & nine release units have no receipt. Swift adds source/ledger/receipt drift: source `0.3.8`, staged `0.3.1`, adopted `0.1.1`, receipt `0.3.7`.

Evidence: current `HEAD` is `08252a681d9f9dc2911962a49fa0026a4c345dff`; gate compares evidence commit with `HEAD` & rejects dirty package trees at `/Volumes/D/claude/rightkit/packages/release/publication-gate.mjs:32-49`. It requires macOS-only Swift receipts at `/Volumes/D/claude/rightkit/packages/release/publication-gate.mjs:15-18,55-64`.

## Swift inventory

`RightKitSwift` is SwiftPM 6.0, iOS 17/macOS 14, 14 library products, 14 test targets, zero declared third-party packages. Product/target map is exact at `/Volumes/D/claude/rightkit/swift/RightKitSwift/Package.swift:7-68`; source version is `/Volumes/D/claude/rightkit/swift/RightKitSwift/VERSION:1`.

| Product | Target dependency | Platform frameworks observed | Boundary assessment |
|---|---|---|---|
| RightKitLicense | none | Foundation, CryptoKit, Security | Native license token, verifier & storage; overlaps `@rightkit/license` & `rightkit-license` domain, no code dependency. |
| RightKitDiagnostics | none | Foundation | JSONL persistence/redaction overlaps diagnostics role of `rightkit-logs`; no shared format dependency found. |
| RightKitArtifacts | zlib linker only | CryptoKit, Foundation, zlib | Native artifact validation/install; no direct Rust/npm dependency. |
| RightKitPurchases | RightKitLicense | Combine, StoreKit | Apple StoreKit boundary; dependency is declared at `/Volumes/D/claude/rightkit/swift/RightKitSwift/Package.swift:38-44`. |
| RightKitCloudSync | none | CloudKit, Foundation, Network | Apple CloudKit transport; used by Rust helper through process protocol. |
| RightKitStore | none | Foundation, Darwin | App Group storage/locks. |
| RightKitImportQueue | RightKitStore | Foundation, UniformTypeIdentifiers | Share intake queue; dependency declared at `/Volumes/D/claude/rightkit/swift/RightKitSwift/Package.swift:48-53`. |
| RightKitLiveActivity | none | ActivityKit, Foundation | Apple live activity lifecycle. |
| RightKitWatchBridge | none | Foundation, WatchConnectivity | Watch transfer ledger & protocol. |
| RightKitSpeech | none | AVFAudio, AVFoundation, Accelerate, CoreML, Speech | CoreML/Apple ASR path; capability overlaps Rust ASR/audio/VAD family. |
| RightKitIntents | none | AppIntents, Foundation | Apple App Intents bridge. |
| RightKitAppleCredentials | none | Foundation, LocalAuthentication, Security | Apple credential storage. |
| RightKitOverlay | none | AppKit, UIKit | Apple overlay behavior. |
| RightKitUI | none | SwiftUI | Apple adaptive-glass surface. |

Import inventory is limited to Apple SDK modules plus internal `RightKitCloudSync`, `RightKitLicense`, & `RightKitStore`; no third-party SwiftPM import appears in package sources. Source-level import evidence starts at `/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitLicense/LicenseClient.swift:1`, `/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitCloudSync/CloudKitSyncTransport.swift:1`, `/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitSpeech/SpeechEngines.swift:1`, & `/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitAppleCredentials/RightKitAppleCredentials.swift:1`.

`RightKitCloudKitHelper` is distinct executable-only SwiftPM package, macOS 14, with one product. It uses `../RightKitSwift` path dependency at `/Volumes/D/claude/rightkit/swift/RightKitCloudKitHelper/Package.swift:7-21`; therefore it is not standalone-publishable. Its source explicitly bridges Rust `rightkit-sync` via line JSON at `/Volumes/D/claude/rightkit/swift/RightKitCloudKitHelper/Sources/rightkit-cloudkit-helper/main.swift:1-12` & consumes `RightKitCloudSync` at `/Volumes/D/claude/rightkit/swift/RightKitCloudKitHelper/Sources/rightkit-cloudkit-helper/Session.swift:13-15`. This is deliberate cross-runtime coupling, but release packaging must replace path resolution with same-tag source or bundle helper with consumer.

Third SwiftPM manifest is record-helper package `/Volumes/D/claude/rightkit/crates/rightkit-record/helpers/macos/Package.swift:1-34`: macOS 14; executables `rightkit-record-macos-helper` & `rk-fixture-window`; internal `RecordWire` target; helper depends only on `RecordWire`; test target depends only on `RecordWire`. It declares no package dependencies. It imports Apple frameworks Foundation, AppKit, ScreenCaptureKit, AVFoundation, CoreMedia, CoreVideo, CoreGraphics, Security, UniformTypeIdentifiers at `/Volumes/D/claude/rightkit/crates/rightkit-record/helpers/macos/Sources/rightkit-record-macos-helper/CaptureSession.swift:1-7` & `/Volumes/D/claude/rightkit/crates/rightkit-record/helpers/macos/Sources/rightkit-record-macos-helper/System.swift:1-5`. It belongs to `rightkit-record`, whose Cargo manifest declares `publish = false`; no independent version, ledger entry, or gate receipt exists.

## Redundant or split capability boundaries

| Finding | Evidence | Risk |
|---|---|---|
| License is independently implemented in Swift, Rust & TypeScript. | Swift `LicenseClient` starts `/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitLicense/LicenseClient.swift:1`; Rust `rightkit-license` manifest `/Volumes/D/claude/rightkit/crates/rightkit-license/Cargo.toml:1-6`; TS client exports `createLicenseClient` at `/Volumes/D/claude/rightkit/packages/license/src/index.ts:117`. | Contract drift unless shared vectors are gated per release. |
| Diagnostics has separate Swift JSONL store & Rust logging crate. | Swift store `/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitDiagnostics/JSONLDiagnosticStore.swift:1`; Rust JSONL collection `/Volumes/D/claude/rightkit/crates/rightkit-logs/src/collect.rs:93-186`. | No direct duplicate dependency; event schema/redaction behavior can diverge. |
| Speech capability overlaps Swift CoreML engines with Rust ASR/audio/VAD. | Swift engine surface `/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitSpeech/SpeechEngines.swift:7-235`; Rust ASR identifies itself as Parakeet transcription at `/Volumes/D/claude/rightkit/crates/rightkit-asr/src/lib.rs:1-19`; Rust VAD spans `/Volumes/D/claude/rightkit/crates/rightkit-vad/src/spans.rs:40-132`. | High parity burden; no shared dependency or version handshake found. |
| Cloud sync is split correctly at runtime boundary, but helper is source-bound. | Helper package path dependency `/Volumes/D/claude/rightkit/swift/RightKitCloudKitHelper/Package.swift:13-20`; Rust config names helper at `/Volumes/D/claude/rightkit/crates/rightkit-sync/src/helper/process.rs:52-56`. | Helper cannot independently resolve published Swift source. |

No redundant third-party Swift dependency exists: `RightKitSwift` has no `dependencies:` section, while helper has only its local path dependency.

## Consumer inventory & pin compliance

Non-cache scan found six declaration groups across five product checkouts, including nested helper/test manifests. CodeRight, HeardRight & ScrapeRight use remote URL + exact version in generated/current manifests. SellRight & RightSites generator manifests use ranges, violating RightKit's exact-tag rule. CodeRight source generator also differs from generated project pin.

| Consumer | Pin | Products evidenced | Exact source |
|---|---:|---|---|
| CodeRight iOS | 0.1.1 | License, Diagnostics, UI | `/Volumes/D/claude/coderight/apps/coderight-ios/CodeRight.xcodeproj/project.pbxproj:2368-2375`; resolved pin `/Volumes/D/claude/coderight/apps/coderight-ios/CodeRight.xcodeproj/project.xcworkspace/xcshareddata/swiftpm/Package.resolved:23-29` |
| CodeRight iOS generator | from 0.1.0 | source generation declaration | `/Volumes/D/claude/coderight/apps/coderight-ios/project.yml:23-28`; generated project above is exact 0.1.1. Generator drift is current project metadata defect. |
| HeardRight iOS & E2E | 0.3.2 | UI, Purchases, CloudSync, Store, ImportQueue, LiveActivity, WatchBridge, Speech, License, Intents, AppleCredentials, Overlay | `/Volumes/D/claude/heardright/ios-native/HeardRightNative.xcodeproj/project.pbxproj:2304-2311,2430-2444`; E2E exact deps `/Volumes/D/claude/heardright/ios-native/Tests/RightKitMobileE2E/Package.swift:3-8` & `/Volumes/D/claude/heardright/ios-native/Tests/RightKitSpeechE2E/Package.swift:7-10` |
| ScrapeRight iOS & helper | 0.3.7 | AppleCredentials, Artifacts, Diagnostics, ImportQueue, Purchases, CloudSync, Store, License | `/Volumes/D/claude/scraperight/ios-native/ScrapeRightNative.xcodeproj/project.pbxproj:1151-1157,1192-1210`; helper exact pin `/Volumes/D/claude/scraperight/macos-native/ScrapeRightAppleCredentials/Package.swift:10-18` |
| SellRight iOS | resolved 0.1.0; range starts 0.1.0 | Diagnostics | non-exact declaration `/Volumes/D/claude/sellright/ios/SellRightAdmin.xcodeproj/project.pbxproj:726-733`; resolved `/Volumes/D/claude/sellright/ios/SellRightAdmin.xcodeproj/project.xcworkspace/xcshareddata/swiftpm/Package.resolved:5-11` |
| RightSites iOS generator | from 0.1.0 | Diagnostics | `/Volumes/D/claude/rightsites/ios/project.yml:16-30`; mirrors SellRight range policy but is separate source-generator checkout. |

Ledger's adopted Swift pin is `0.1.1`, staged is `0.3.1`, & local source is `0.3.8` at `/Volumes/D/claude/rightkit/packages/release/rightkit-versions.json:323-333` & `/Volumes/D/claude/rightkit/swift/RightKitSwift/VERSION:1`. Thus ledger adoption does not represent HeardRight/ScrapeRight pins, & no recorded staged version covers source 0.3.8.

## Publication metadata & dirtiness

Full source/ledger/receipt matrix covers all 80 Cargo/npm manifests, plus three SwiftPM manifests: `/Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-10/evidence/swift-publication.csv`.

- 45 publishable Cargo crates, 22 public npm packages, & one Swift package were found: 68 release units. 59 have local receipts; nine have none. Every available receipt names another commit than current `HEAD`; all 68 therefore fail gate for newly publishing current tree. This does not establish whether any version was previously published. Adopted-pin differences are lifecycle state, not defects by themselves.
- Receipt-version mismatch/missing: Cargo `rightkit-browser`, `rightkit-hotkeys`, `rightkit-license`, `rightkit-llm`, `rightkit-ocr`, `rightkit-overlay`, `rightkit-sync`; npm `@rightkit/ax`, `@rightkit/legal`, `@rightkit/license`, `@rightkit/logs`; Swift `RightKitSwift`.
- Current source metadata mismatch: `rightkit-ocr` source `0.2.0` but staged/recorded registry/receipt are `0.1.2` at `/Volumes/D/claude/rightkit/crates/rightkit-ocr/Cargo.toml:1-6`, `/Volumes/D/claude/rightkit/packages/release/rightkit-versions.json:288-290`, & `/Volumes/D/claude/rightkit/release-evidence/gate/cargo-rightkit-ocr.json:1-7`.
- Recorded registry drift: `rightkit-browser` source/staged `0.2.3`, local registry/receipt `0.2.2` at `/Volumes/D/claude/rightkit/crates/rightkit-browser/Cargo.toml:1-6`, `/Volumes/D/claude/rightkit/packages/release/rightkit-versions.json:266-269`, & `/Volumes/D/claude/rightkit/release-evidence/gate/cargo-rightkit-browser.json:1-7`. Registry remains unverified.
- Dirty publishable paths: `rightkit-fsindex` has tracked/untracked changes though version is unchanged; `rightkit-ocr` has tracked/untracked changes plus version drift. Gate rejects dirty trees at `/Volumes/D/claude/rightkit/packages/release/publication-gate.mjs:36-38`.
- Swift receipt is `0.3.7` with commit `b0566e…`, while current source is `0.3.8`; that commit is absent from this checkout. Receipt details: `/Volumes/D/claude/rightkit/release-evidence/gate/swift-RightKitSwift.json:1-24`.

Internal/non-publishable manifests: Cargo `agent-room-core`, `rightkit-agent-tools`, `rightkit-align`, `rightkit-automation`, `rightkit-channel-memory`, `rightkit-compose`, `rightkit-contracts`, `rightkit-daemon`, `rightkit-record`, `rightkit-review`, `rightkit-scenario` each declares `publish = false`; npm `@rightkit/agent-room-mcp` & `@rightkit/build-control-policy` declare `private: true`. They have no publication gate requirement.

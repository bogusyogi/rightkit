# RightKitSwift

Shared Apple-platform infrastructure for the Right Suite iOS apps — one source of
truth for the pieces every app would otherwise reimplement and let drift.

| Target | What it does |
|---|---|
| `RightKitLicense` | Verifies Ed25519 entitlement tokens against the same `contracts/license-v2.json` vector the Rust and TypeScript verifiers use, so a token accepted on one platform is accepted on all. `LicenseActivationClient` handles activation, refresh, renewable leases and removal, including `removeLease(deviceIdHash:leaseId:)` and `deactivate(activationToken:)` for durable pending-removal queues that must remove a lease or activation by its original identity without touching the local activation. `PendingLicenseRemovalQueue` persists and retries those removals, `BridgeConsumptionGate` stops a device from re-consuming a continuity bridge after the user removed its license, and `StoredLicense.needsReconnect` / `LicenseController.needsReconnect` flag a non-Pro record with no activation token. `LicenseController(client:deviceState:)` wires all three; `LicenseController(client:)` keeps the original behaviour. |
| `RightKitDiagnostics` | Bounded JSONL diagnostics with a privacy redactor (never logs keys, tokens, or device ids). |
| `RightKitArtifacts` | Checksummed artifact install with staging, atomic replace, rollback, and archive-traversal/symlink/expansion guards. |
| `RightKitSpeech` | On-device Parakeet TDT and Whisper Core ML engines, Apple Speech, VAD and dictation. Engines accept injected canonical `ConditioningProvider` / `DetokenizationProvider` hooks that every decode path routes through. `WhisperEngine.transcribeStaticWindow` returns text, the resolved AUTO language and phase timings with cooperative cancellation that drains an active Core ML prediction, and `IncrementalStatic15Decoder` schedules and assembles background Static-15 windows with generation fences. |
| `RightKitPurchases` | StoreKit 2 purchases, entitlement ledger and access hints behind a `PurchaseBackend` seam. `PurchaseOutcome.unknown` covers StoreKit's `@unknown default` result and shows `PurchaseMessages.unknownOutcome`. |
| `RightKitCloudSync` | Generic CloudKit private-zone sync engine. One adapter can own several record types (`recordType(of:)`, `recordType(forItemID:)`) and read direct types it never uploads (`directRecordTypes`, written through `CloudSyncEngine.writeDirect`). `SyncTransport.fetchRecord(named:)` reads one live record. App-side failures back off with the engine (`noteFailure`, `raiseRetryGate(toAtLeast:)`, `retryGate`). `adoptLegacyLedger` imports a pre-RightKit ledger with its change token and versions without an account call. `encodeForUpload` / `SyncLazyPayload` produce payloads lazily at upload time. `SyncTrigger.submit`. |
| `RightKitUI` | Generic adaptive-glass surface primitive: regular/clear native Liquid Glass + ConcentricRectangle on iOS/macOS 26, material fallback on older OS versions, and opaque accessibility fallback. Product radii, colors, layout, motion, and behavior stay app-local. |

## Consuming it

Swift has no public package registry like npmjs.com or crates.io (Apple is
building one via the Swift Package Index, not yet live), and SwiftPM is
git-URL-native, so apps pin this repo by URL + version tag — the same
"published, pinned, reproducible" property npm/crates give, with no registry to
host.

Pins are always exact (`exact:`), never `from:` ranges: a consumer upgrade is a deliberate
edit of the pin, recorded in `rightkit-versions.json`.

```swift
// Package.swift
dependencies: [
    .package(url: "https://github.com/bogusyogi/rightkit-swift.git", exact: "0.3.3"),
],
targets: [
    .target(name: "YourApp", dependencies: [
        .product(name: "RightKitLicense", package: "rightkit-swift"),
    ]),
]
```

In an XcodeGen `project.yml`:

```yaml
packages:
  RightKitSwift:
    url: https://github.com/bogusyogi/rightkit-swift.git
    exactVersion: "0.3.3"
```

A clone of a consuming app resolves this straight from GitHub — no workspace
checkout required.

## Testing

```sh
swift build
swift test --no-parallel
xcodebuild -scheme RightKitSwift-Package -destination 'generic/platform=iOS Simulator' build
```

### Real StoreKit tests (headless iOS simulator)

`Tests/RightKitPurchasesTests/StoreKitTestSessionTests.swift` loads products from
`Tests/RightKitPurchasesTests/StoreKit/RightKitPurchasesTests.storekit` through a programmatic
`SKTestSession` and real StoreKit 2 (`StoreKit2Backend`, `PurchaseController`). Run it with the
test-only shared scheme `.swiftpm/xcode/xcshareddata/xcschemes/RightKitPurchasesStoreKitTests.xcscheme`
(the `RightKitSwift-Package` scheme cannot run tests on iOS because other test targets use
macOS-only APIs such as `Process`):

```sh
xcrun simctl boot "iPhone 17"   # headless; never opens Simulator.app (xcodebuild also boots it)
xcodebuild test -scheme RightKitPurchasesStoreKitTests \
  -destination 'platform=iOS Simulator,name=iPhone 17'
```

No host app, test plan or scheme-level StoreKit configuration is needed. What was measured:

- **0 products came from the configuration schema, not from running headless.** A config that
  lists non-consumables in a top-level `"nonConsumables"` array is accepted by `SKTestSession`
  and saved by `storekitd`, but the local catalog ignores it and every request returns 0
  products, hosted app or not. List consumables and non-consumables under `"products"` with a
  `"type"` (`"NonConsumable"`, `"Consumable"`), exactly as Xcode writes the file.
- On the simulator the package test runs inside the `xctest` agent
  (`com.apple.dt.xctest.tool`). `storekitd` logs `Error initializing client ... OSStatus -10814`
  for it, which is noise: with a correct config the catalog still serves it.
- macOS cannot serve the catalog to a test process: `swift test` runs in
  `swiftpm-testing-helper` (no bundle id, `StoreKitInternalError.invalidApp`), and
  `xcodebuild test -destination platform=macOS` returns 0 products. The suite is therefore
  enabled only on iOS and reported as skipped by `swift test`.

## Source of truth

This repo is the **consumption mirror**. The canonical source is
`tools/rightkit/swift/RightKitSwift` in the workspace repo; a release copies that
tree here and tags it, exactly as the npm/crates packages publish from the same
workspace. Do not edit here directly — change the workspace source, then cut a
new tag.

## Releasing a new version

From the workspace: bump `RightKitSwift/VERSION`, re-run `swift test`, then mirror
the tracked tree into this repo and tag `X.Y.Z`. Record the new version in
`tools/rightkit/packages/release/rightkit-versions.json` (`swift` section) so apps
can bump their pin.

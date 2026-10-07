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

## RightKitLicense configuration (0.3.5)

`LicenseVerifier` reads the entitlement scope from the signed `entitlement_scope` claim (the key
the server signs and ScrapeRight/HeardRight assert), falling back to a legacy `scope` claim;
`LicenseClaims.scope` is nil when neither is present. Before 0.3.4 only `scope` was read, so a
lease record whose envelope carried `entitlementScope` never matched its token and was not Pro.

`LicenseActivationClient.Configuration` (`LicenseClientConfiguration`) carries the wire
differences between RightKit's original client and ScrapeRight's in-app client. Every default is
the pre-0.3.4 behaviour, and the 0.3.5 knobs default to the strict gate:

| Field | Default | ScrapeRight-style value | Effect |
|---|---|---|---|
| `deviceClass: String?` | `nil` (omitted) | `"iphone"` | Sent as `deviceClass` in the `activate` body when set. |
| `includeAppKeyInLeaseBody: Bool` | `true` | `false` | Whether lease issue/renew/remove bodies carry `appKey`. |
| `leasePathStyle: LicenseLeasePathStyle` | `.siblingOfBase` | `.originRooted(apiPrefix: "/v1")` | Lease URLs: parent of `baseURL` + `pro/<app>/devices/lease[/renew\|/remove]`, or origin + `apiPrefix` + the same. |
| `refreshErrorPolicy: LicenseRefreshErrorPolicy` | `.keepRecordOnServerError` | `.keepRecordOnAnyError` (or `.surfaceAll` to show every failure) | Default: 4xx, transport and unreadable responses throw, other statuses (5xx) keep the record. `.surfaceAll` throws everything; `.keepRecordOnAnyError` never throws (what ScrapeRight's client does today). |
| `requireActiveStatus: Bool` (0.3.5) | `true` | `false` | Pro also requires `status == "active"` (desktop `license.rs` does too). `false`: a verifying token is enough (signature, app, device, expiry still checked), as ScrapeRight's original client did. Used by `LicenseController` and `StoredLicenseResolver`; also a parameter on `StoredLicense.authorizedClaims`, `needsReconnect`, `isPro` and `satisfiesRequirement`. |
| `acceptedTokenVersions: Set<Int>` (0.3.5) | `[2]` | `[1, 2]` | Token `v` claims accepted; others fail as `LicenseError.unsupportedVersion`. Also `LicenseVerifier.verify(...acceptedVersions:)`. v1 tokens go through the same payload decoder, so they must carry `v`, `id`, `app`, `tier`, `features`, `device_id`, `iat`, `exp` (what ScrapeRight's decoder required); v1 has none of the additive claims (`entitlement_scope`/`scope`, `license_kind`, `entitlement_stage`, `license_issued_at`, `confirmation_due_at`), which decode as nil (nil scope = legacy full rights). |
| `revokingStatuses: Set<String>` (0.3.5) | `["removed", "revoked"]` | same | Refresh statuses (case-insensitive) that clear the signed token, entitlements and `validUntil`. |
| `allowedEntitlementScopes: Set<String>` (0.3.5) | `LicenseEntitlementScope.mobileCompatible` | same | Envelope scopes `activateLease`/`renewLease` accept. |

```swift
let client = LicenseActivationClient(
    configuration: .init(
        app: "scraperight", baseURL: URL(string: "https://scraperight.cc/v1/licenses")!,
        appVersion: version, deviceID: { store.deviceID() }, publicKeyBase64url: key,
        deviceClass: "iphone", includeAppKeyInLeaseBody: false,
        leasePathStyle: .originRooted(apiPrefix: "/v1"), refreshErrorPolicy: .surfaceAll
    ),
    store: KeychainLicenseStore(service: "app.scraperight.ios.native.license")
)
```

Lease URLs are now built from URL components instead of `appendingPathComponent("../pro/...")`.
For the default style the URL equals the standardized form of the old one
(`https://host/v1/licenses` → `https://host/v1/pro/<app>/devices/lease`), but the request line no
longer carries the literal `/v1/licenses/../pro/...` segment, which URLSession used to send
unresolved. A base URL with no path resolves to `/pro/<app>/devices/lease`.

**Not verified against the server.** The license server source is not in this workspace, so
whether it accepts each default and each alternative (a body without `appKey`, `deviceClass` on
activate, the normalized lease request path) was not checked. The defaults reproduce the bodies
and resolved URLs RightKit sent before 0.3.4; the alternatives reproduce ScrapeRight's client.

`LicenseController`:

- `refresh(silent: Bool = false)` renews lease-bound records (`leaseId != nil`) through
  `renewLease()` and refreshes the rest through `refresh()`. `silent: true` leaves `message` and
  `isBusy` untouched and republishes `license` only when it changed.
- `LicenseMessages` (injected as `messages:` in either initializer) overrides every string the
  controller sets: `activated`, `refreshed`, `removed`, plus optional `activatedPro` /
  `refreshedPro` used when the new record verifies as Pro (e.g. `"Pro entitlement restored."`).
  Defaults are the original strings, and `nil` Pro strings keep the original
  `record.message ?? fallback` behaviour.

- `LicenseMessages.deviceRemoved` (0.3.5, optional) replaces the message after a non-silent
  refresh whose lease renewal or legacy refresh got HTTP 404/410. `nil` keeps the original text.

### Refresh merge (0.3.5)

`refresh()` clears the signed token, entitlements and `validUntil` only for an explicit status in
`revokingStatuses`. The default is `removed`/`revoked`: the set ScrapeRight's and HeardRight's
shipped iOS clients and HeardRight's desktop `refresh_license_and_reanchor_inner` treat as
revocation. `invalid` is not in it: none of them clears the token for it (the desktop JS client
stores `invalid` and keeps the token; the desktop Rust gate then denies Pro because the status is
not `active`, which is what `requireActiveStatus: true` does here). 0.3.4 also revoked on `invalid`,
and because a body with neither `status` nor `ok` became `invalid`, an empty 2xx refresh cleared
the token. Now a 2xx body with neither field keeps the token and the current status; any other
non-revoking status (including `invalid`, and `invalid` derived from `ok: false`) is stored as sent
and keeps the token.

### Errors (0.3.5)

- An `activateLease` response without `lease` or `activationToken` throws
  `LicenseResponseError.incomplete(text)` with the server's `error` (plus `": " + detail` when
  present) instead of `LicenseError.invalidResponse`; with no such text it still throws
  `invalidResponse`. `LicenseServerResponse` gains `detail` and `errorText`.
- Non-2xx bodies read `message`, else `error` and `detail` (`"error: detail"`).
- `LicenseLeaseEnvelope.entitlement` is optional again, so a bridge response without it decodes. A
  record built from such an envelope has no `signedToken` and is never Pro.

For bridge and lease flows that receive an envelope themselves,
`LicenseActivationClient.license(fromLease:activationToken:deviceId:publicKeyBase64url:key:activatedAt:allowedScopes:)`
builds the `StoredLicense` after verifying the envelope signature and then the fail-closed scope
check `LicenseLeaseEnvelope.validateEntitlementScope(allowed:)` (default
`LicenseEntitlementScope.mobileCompatible` = `full`, `mobile`; a missing scope passes, anything
else throws `LicenseScopeError.unsupportedEntitlementScope`). Since 0.3.5 `activateLease` and
`renewLease` apply the same signature-then-scope check (against `allowedEntitlementScopes`) before
anything is stored: an unsupported scope throws `LicenseScopeError` and storage is untouched
(`renewLease` throws instead of returning the current record).

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
    .package(url: "https://github.com/bogusyogi/rightkit-swift.git", exact: "0.3.6"),
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
    exactVersion: "0.3.6"
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

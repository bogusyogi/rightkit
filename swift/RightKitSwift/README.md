# RightKitSwift

Shared Apple-platform infrastructure for the Right Suite's native Apple apps — one source of
truth for the pieces every app would otherwise reimplement and let drift. Both manifests declare
minimum deployment targets of iOS 17 and macOS 14.

| Target | What it does |
|---|---|
| `RightKitLicense` | Verifies Ed25519 entitlement tokens against the same `swift/contracts/license-v2.json` vector the Rust and TypeScript verifiers use (byte-identical copies sit in each of those packages), so a token accepted on one platform is accepted on all. `LicenseActivationClient` handles activation, refresh, renewable leases and removal, including `removeLease(deviceIdHash:leaseId:)` and `deactivate(activationToken:)` for durable pending-removal queues that must remove a lease or activation by its original identity without touching the local activation. `PendingLicenseRemovalQueue` persists and retries those removals, `BridgeConsumptionGate` stops a device from re-consuming a continuity bridge after the user removed its license, and `StoredLicense.needsReconnect` / `LicenseController.needsReconnect` flag a non-Pro record with no activation token. `LicenseController(client:deviceState:)` wires the removal queue and the bridge gate; `LicenseController(client:)` keeps the original behaviour (no queue, no gate). |
| `RightKitDiagnostics` | Bounded JSONL diagnostics with a privacy redactor: the default `standard` policy replaces any key, value or event name containing a URL, `file://`, `/tmp/`, `.caf`, `.wav`, `authorization`, `bearer`, `secret` or `token` with `[redacted]` and reduces the rest to a safe alphabet capped at 80 characters; `fields` redacts only the named fields' values and `none` transforms nothing. `StoreOptions` sets file protection, redaction policy (`standard`, `fields`, `custom`, `none`) and an uploader; `append` and `drain` (at-least-once batches) are async. Trims cut on record boundaries. |
| `RightKitArtifacts` | Checksummed artifact install with staging, a fail-closed digest (`allowUnverified` opt-in), the previous install kept aside until a post-activation validator passes, crash recovery (`recoverInterruptedInstall`), required-file checks, an install manifest (`readManifest`), and archive-traversal/symlink/expansion guards. The target links the platform SDK's zlib. |
| `RightKitSpeech` | On-device Parakeet TDT and Whisper Core ML engines, Apple Speech, VAD and dictation. `ParakeetEngine` accepts injected canonical `ConditioningProvider` / `DetokenizationProvider` hooks and `WhisperEngine` a `ConditioningProvider`; every decode path routes through them. `WhisperEngine.transcribeStaticWindow` returns text, the resolved AUTO language and phase timings with cooperative cancellation that drains an active Core ML prediction, and `IncrementalStatic15Decoder` schedules and assembles background Static-15 windows with generation fences. |
| `RightKitPurchases` | StoreKit 2 purchases, entitlement ledger and access hints behind a `PurchaseBackend` seam. `PurchaseOutcome.unknown` covers StoreKit's `@unknown default` result and shows `PurchaseMessages.unknownOutcome`. |
| `RightKitCloudSync` | Generic CloudKit private-zone sync engine. One adapter can own several record types (`recordType(of:)`, `recordType(forItemID:)`) and read direct types it never uploads (`directRecordTypes`, written through `CloudSyncEngine.writeDirect`). `SyncTransport.fetchRecord(named:)` reads one live record. App-side failures back off with the engine (`noteFailure`, `raiseRetryGate(toAtLeast:)`, `retryGate`). `adoptLegacyLedger` imports a pre-RightKit ledger with its change token and versions without an account call. `encodeForUpload` / `SyncLazyPayload` produce payloads lazily at upload time. `SyncTrigger.submit`. |
| `RightKitUI` | Generic adaptive-glass surface primitive: regular/clear native Liquid Glass + ConcentricRectangle on iOS/macOS 26, material fallback on older OS versions, and opaque accessibility fallback. Product radii, colors, layout, motion, and behavior stay app-local. |
| `RightKitStore` | App Group primitives: cross-process command queue, file lock, owner lease, and revisioned state store. |
| `RightKitImportQueue` | Share-extension intake: the pending share payload shared between an extension and its app, and a queue that imports it. |
| `RightKitLiveActivity` | Live Activity lifecycle rules (session binding, terminal restart, adoption, watchdog). The app supplies its own phase type. |
| `RightKitWatchBridge` | WatchConnectivity transfer ledger and delivery states. |
| `RightKitIntents` | App Intents recording bridge: Darwin notifications, microphone permission handshake, and recording actions. |
| `RightKitAppleCredentials` | Security.framework credential backend: raw-status probing (`probeStatus`), all-location deletion (`deleteAnyScope`), and `ForeignGenericPassword`, a read-only reader for another app's legacy file-Keychain item that does not prompt by default (macOS). |
| `RightKitOverlay` | Overlay state, hit testing, and the non-activating overlay panel model (the panel is macOS only), with per-rectangle click-through (`setClickThrough`, `hitRegions`, `updateClickThrough(forMouseAt:)`). |
| `RightKitWatch` | FSEvents file watcher (macOS): `RightKitWatcher` delivers typed item, root-changed, history-done and must-rescan events as an `AsyncStream`, with event IDs for replay. |
| `RightKitAccessibility` | Accessibility API for macOS apps (empty on iOS): `RightKitAXTrust.isTrusted(prompt:)`, `RightKitAXElement` (typed reads of role, title, windows, frame; writes of position, size, frame, any attribute; `perform`), `RightKitAXWindowAction` (raise, minimise, full screen, close, zoom through the window's buttons), and `RightKitAXObserver`, an `AXObserver` with an `AsyncStream` of events that tracks window creation and destruction. Failures are `RightKitAXFailure`. Needs Accessibility permission; the library never prompts unless asked. |
| `RightKitHotkeys` | Carbon `RegisterEventHotKey` global hot keys (macOS; empty on iOS): `RightKitHotkeyRegistrar` registers `RightKitHotkeyCombo` values (virtual key code plus `RightKitHotkeyModifiers`) and reports press and release on the main actor. No permission is needed. Event-tap hot keys are not included. |

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
        appVersion: version, deviceID: { (try? store.requireDeviceID()) ?? "" }, publicKeyBase64url: key,
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

**Not verified against the server.** The license server source is not in this repository, so
nothing here checks whether it accepts each default and each alternative (a body without `appKey`,
`deviceClass` on activate, the normalized lease request path). The defaults reproduce the bodies
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

### Serialized controller and single-flight removal retry (0.3.7)

- Every `LicenseController` operation that touches the store or publishes `license` (the initial
  load, `activate`, `activateLease`, `persist`, `refresh`, `remove`, `retryPendingRemovals`) runs
  on one serial chain, in call order; each starts only after the previous one finished. Once
  `await remove()` returns, nothing queued or in flight can republish the removed licence: earlier
  operations have finished and later ones read the emptied store. Waiting suspends, never blocks
  the main actor. A slow operation (e.g. a refresh on a stalled network) delays the ones behind it.
- `PendingLicenseRemovalQueue.retry` is single-flight: concurrent callers on one queue join the
  wave in flight and get its result (0.3.6 returned `[]`). Waves of different queue instances on
  the same persisted key run one after another. Each entry is re-read from storage right before it
  is posted and dropped as soon as the server accepts it, so a removal is posted at most once per
  wave and an acknowledged one is never posted again.

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

## RightKitAppleCredentials (0.3.8)

`SecurityAppleCredentialBackend` adds two APIs on platforms with Security.framework;
existing store/backend methods and `AppleCredentialBackend` conformers remain source compatible.

```swift
let backend = SecurityAppleCredentialBackend(accessGroup: "6KLGD3LLKF.app.scraperight.shared")
let service = "app.scraperight.shared.credentials"
let probe = backend.probeStatus(service: service, account: "pro.bridge", scope: nil)
switch probe {
case .available, .notFound: break // Keychain query is usable, even without an item.
case .missingEntitlement: break // Report missing keychain-access-groups entitlement.
case .interactionNotAllowed: break // Query could not complete without interaction.
case let .failure(status): print("Keychain status: \(status)")
}
let rawStatus = probe.rawStatus // Original OSStatus, including -34018 for missing entitlement.

// Explicit "forget everywhere" only: deletes synchronized copies on other devices too.
let outcomes = backend.deleteAnyScope(service: service, account: "pro.bridge")
let succeeded = outcomes.allSatisfy(\.succeeded)
for outcome in outcomes {
    print(outcome.scope.rawValue, outcome.rawStatus, outcome.succeeded)
}
```

- `probeStatus(service:account:scope:) -> CredentialProbe` probes the supplied identity in the
  data-protection Keychain. `.synced` and `.device` preserve strict Boolean sync matching;
  omitted/nil scope uses `kSecAttrSynchronizableAny`. It requests no secret bytes, uses
  `kSecUseAuthenticationUISkip` and an `LAContext` with `interactionNotAllowed = true`, and
  preserves the raw status. Authentication-protected items are skipped, so `.notFound` does
  not prove no protected item exists. [Apple authentication-skip documentation](https://developer.apple.com/documentation/security/ksecuseauthenticationuiskip).
- `deleteAnyScope(service:account:) -> [CredentialDeletionOutcome]` attempts every location
  without stopping on failure. `.dataProtection` sweeps synchronizable and device-only items
  with `kSecAttrSynchronizableAny`, preserving the configured access group. On macOS,
  `.legacyFile` also sweeps matching service/account items in the legacy file Keychain search
  list; it omits access group and synchronization attributes because legacy Keychains use
  ACLs. Other Apple platforms return only `.dataProtection`. Each outcome retains its
  `rawStatus`; `succeeded` accepts `errSecSuccess` and `errSecItemNotFound`. Deletion is not
  atomic across locations; inspect all outcomes before reporting complete cleanup.

0.3.8 adds passive raw-status probing and all-location deletion. `status()`, `exists`,
strict scoped `delete`, and `AppleCredentialStore.deleteEverywhere` retain their behavior.

## ForeignGenericPassword, per-rect click-through, RightKitWatch (0.5.0)

All three are additive; nothing existing changes behavior. Gap ledger #63.

### `ForeignGenericPassword` (RightKitAppleCredentials, macOS)

Reads a generic-password item that **another app or tool** filed in the legacy file-based login
Keychain (for example a CLI's OAuth token). `AppleCredentialStore` cannot see these: it owns items
in the data-protection Keychain under your access group.

```swift
switch ForeignGenericPassword.read(service: "Claude Code-credentials", account: nil, policy: .noPrompt) {
case let .success(item): use(item.string)            // newest item by modification date
case .failure(.notFound): break
case .failure(.interactionRequired): break           // the item's ACL needs the user; nothing was shown
case .failure(let other): print(other)
}
```

- Queries only the legacy file Keychain (`kSecUseDataProtectionKeychain = false`), no access group.
- Several matches (a tool that files a new item per token rotation): the newest
  `kSecAttrModificationDate` wins. Attributes are enumerated first, which never prompts.
  `ForeignGenericPassword.matches(service:account:)` lists them, newest first.
- The secret is read **by persistent reference** of the winning item, not by a second search.
- `.noPrompt` (default) never presents UI and returns `.interactionRequired` instead.
  `.allowPrompt` lets the system ask, for an action the user just started.
- `ReadError.accessDenied` is a merged status: it covers `errSecAuthFailed` (under `.allowPrompt`),
  `errSecUserCanceled` (the user dismissed the prompt) and `errSecNotAvailable` (no usable Keychain or
  session). A caller that must treat "user said no" differently from "Keychain unavailable" uses
  `readDetailed`, whose `ReadFailure` also carries the raw `OSStatus`. Behavior is unchanged.
- **Read-only for foreign items:** there is no write, update or delete here.
- No-prompt mechanics (review point): `kSecUseAuthenticationUISkip` and `LAContext.interactionNotAllowed`
  do **not** stop the legacy ACL dialog (measured: a read of an item that trusts no application waited
  ~10 s on the dialog). `.noPrompt` therefore also turns off Keychain UI around its secret-data read (the attribute
  enumeration never touches it) with `SecKeychainSetUserInteractionAllowed(false)`, resolved at runtime
  because the SDK no longer declares it. That switch is process-wide: other threads' Keychain calls
  inside that read are also refused without UI, and no-prompt reads are serialised. If the symbol is ever missing the read fails closed
  (`.keychainStatus(errSecUnimplemented)`), never prompts.

### Per-rectangle click-through (RightKitOverlay)

`RightKitOverlayPanel` stays interactive by default. New:

- `setClickThrough(_ enabled: Bool)`: whole-panel pass-through; assigns only on change.
- `hitRegions: [CGRect]`: interactive rectangles in window coordinates, measured from
  `hitRegionOrigin` (default `.lowerLeft`, the AppKit origin; `.topLeft` is the alternative). Empty
  (default) means per-region toggling is off unless a `hitShape` is set.
- `updateClickThrough(forMouseAt:) -> Bool`: pass a screen-coordinate point (`NSEvent.mouseLocation`);
  the panel takes the mouse inside any region and ignores it elsewhere. A click-through window gets
  no events itself, so call it from a mouse-moved monitor. For full pass-through with no regions use
  `setClickThrough(true)`.
- `RightKitOverlayHitRegions` is the pure rule (cross-platform) behind it.

### `RightKitWatch` (macOS; empty on iOS)

```swift
let watcher = try RightKitWatcher(.init(paths: [folder], sinceWhen: savedID ?? .sinceNow, latency: 0.5))
for await event in watcher.events {
    switch event {
    case let .item(path, kind, id): handle(path, kind); savedID = id
    case let .rootChanged(path): break      // the watched folder itself moved or was deleted
    case .historyDone: break                // replay since savedID finished; later events are live
    case let .mustRescan(path, reason, id): rescan(path); savedID = id   // incl. .bufferOverflow
    }
}
```

`FSEventStreamCreate` with `FileEvents | UseCFTypes` (plus `WatchRoot`, needed for `rootChanged`;
`NoDefer` and `IgnoreSelf` optional), delivered on a private dispatch queue into a bounded
`AsyncStream` (`maxBufferedEvents`, default 4096). A consumer that stalls past the bound keeps the
oldest buffered events, loses the newest, and then receives one `mustRescan` with reason
`.bufferOverflow` per watched path: rescan it. `watcher.droppedEventCount` reports the loss. `RightKitWatchEventID` is `Codable`; persist the ID of the last event you *handled*
(`event.eventID`) and pass it as `sinceWhen` to replay. `RightKitWatchEventID.current()` is a bookmark
to store before the first watch. `watcher.lastEventId` is the newest event ID seen (including any discarded on overflow);
`watcher.flush()` waits for already-seen events; `stop()` (or ending the iteration) tears down.
Event IDs are per volume and journal; a replay that cannot be honoured reports `mustRescan`.

## Consuming it

This package is not published to a registry. SwiftPM resolves it from a git URL,
so apps pin this repo by URL + version tag — the same
"published, pinned, reproducible" property npm/crates give, with no registry to
host.

Pins are always exact (`exact:`), never `from:` ranges: a consumer upgrade is a deliberate
edit of the pin, recorded in `rightkit-versions.json`.

The version in the examples below is illustrative. The version apps may pin is the `swift` section of
`packages/release/rightkit-versions.json`, or `stagedSwift` until the tag is recorded there. `VERSION` is the version
under development; it is published when `right-release publish swift` pushes its tag.

```swift
// Package.swift
dependencies: [
    .package(url: "https://github.com/bogusyogi/rightkit", exact: "0.7.0"),
],
targets: [
    .target(name: "YourApp", dependencies: [
        .product(name: "RightKitLicense", package: "rightkit"),
    ]),
]
```

In an XcodeGen `project.yml`:

```yaml
packages:
  RightKitSwift:
    url: https://github.com/bogusyogi/rightkit
    exactVersion: "0.7.0"
```

The URL is the public repository `bogusyogi/rightkit`; its root `Package.swift` exposes the same products as
`swift/RightKitSwift/Package.swift`, with each target pointing into `swift/RightKitSwift`. The tag is a plain semver tag
(`0.7.0`, no `v`) on that repository, cut from the commit whose `swift/RightKitSwift/VERSION` matches. SwiftPM
derives the package identity from the URL, so it is `rightkit` (not `RightKitSwift`): write
`.product(name: "RightKitLicense", package: "rightkit")` in a `Package.swift`. SwiftPM clones the whole repository
once per machine (a shared cache under `~/Library/Caches/org.swift.swiftpm`); only the resolved tag is checked out. The tools
version is 6.1, so the consumer needs Xcode 16.3 or newer.

A clone of a consuming app resolves this straight from GitHub — no workspace
checkout required.

## Testing

```sh
swift build
swift test --no-parallel
xcodebuild -scheme RightKitSwift-Package -destination 'generic/platform=iOS Simulator' build
```

CI runs `swift build` and `swift test` for this directory on the macOS runner (`docs/CI.md`); RightKit builds and
tests run in GitHub Actions, not locally.

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
  lists non-consumables in a top-level `"nonConsumables"` array is accepted by `SKTestSession`,
  but the local catalog ignores it and every request returns 0 products. List consumables and
  non-consumables under `"products"` with a `"type"` (`"NonConsumable"`, `"Consumable"`), exactly
  as Xcode writes the file.
- On the simulator the package test runs inside the `xctest` agent, and with a correct config
  the local catalog serves it.
- macOS cannot serve the catalog to a test process: `swift test` runs in
  `swiftpm-testing-helper` (no bundle id, `StoreKitInternalError.invalidApp`), and
  `xcodebuild test -destination platform=macOS` returns 0 products. The suite is therefore
  enabled only on iOS and reported as skipped by `swift test`.

## Source of truth

`swift/RightKitSwift` in `bogusyogi/rightkit` is both the source and the distribution: apps resolve the repository
root `Package.swift` at a tag. The nested `swift/RightKitSwift/Package.swift` is what CI builds and tests; change both
manifests together (`scripts/swift-root-manifest.test.mjs` fails on drift).

## Releasing a new version

Bump `swift/RightKitSwift/VERSION`, date the changelog section, let CI pass on that commit, then run
`right-release publish swift`, which tags that commit `X.Y.Z` (plain semver) and pushes the tag to `bogusyogi/rightkit`. Record the version in `packages/release/rightkit-versions.json` (`stagedSwift`, then `swift` once
published) so apps can bump their pin.

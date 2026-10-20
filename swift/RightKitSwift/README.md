# RightKitSwift

Shared Apple-platform infrastructure for the Right Suite's native Apple apps — one source of
truth for the pieces every app would otherwise reimplement and let drift. Both manifests declare
minimum deployment targets of iOS 17 and macOS 14.

| Target | What it does |
|---|---|
| `RightKitLicense` | Verifies Ed25519 entitlement tokens against the same `swift/contracts/license-v2.json` vector the Rust and TypeScript verifiers use (byte-identical copies sit in each of those packages), so a token accepted on one platform is accepted on all. `LicenseActivationClient` handles activation, refresh, renewable leases and removal, including `removeLease(deviceIdHash:leaseId:)` and `deactivate(activationToken:)` for durable pending-removal queues that must remove a lease or activation by its original identity without touching the local activation. `PendingLicenseRemovalQueue` persists and retries those removals, `BridgeConsumptionGate` stops a device from re-consuming a continuity bridge after the user removed its license, and `StoredLicense.needsReconnect` / `LicenseController.needsReconnect` flag a non-Pro record with no activation token. `LicenseController(client:deviceState:)` wires the removal queue and the bridge gate; `LicenseController(client:)` keeps the original behaviour (no queue, no gate). |
| `RightKitDiagnostics` | Bounded JSONL diagnostics with a privacy redactor: the default `standard` policy replaces any key, value or event name containing a URL, `file://`, `/tmp/`, `.caf`, `.wav`, `authorization`, `bearer`, `secret` or `token` with `[redacted]` and reduces the rest to a safe alphabet capped at 80 characters; `fields` redacts only the named fields' values and `none` transforms nothing. `StoreOptions` sets file protection, redaction policy (`standard`, `fields`, `custom`, `none`) and an uploader; `append` and `drain` (at-least-once batches) are async. Trims cut on record boundaries. |
| `RightKitArtifacts` | Checksummed artifact install with staging, a fail-closed digest (`allowUnverified` opt-in), the previous install kept aside until a post-activation validator passes, crash recovery (`recoverInterruptedInstall`), required-file checks, an install manifest (`readManifest`), and archive-traversal/symlink/expansion guards. The target links the platform SDK's zlib. |
| `RightKitSpeech` | On-device Parakeet TDT and Whisper Core ML engines, Apple Speech, VAD and dictation. `ParakeetEngine` accepts injected canonical `ConditioningProvider` / `DetokenizationProvider` hooks and `WhisperEngine` a `ConditioningProvider`; every decode path routes through them. `WhisperEngine.transcribeStaticWindow` returns text, the resolved AUTO language and phase timings with cooperative cancellation that drains an active Core ML prediction, and `IncrementalStatic15Decoder` schedules and assembles background Static-15 windows with generation fences.  **Dictation lifecycle (#595, #597, #599):** `LiveDictationSession` emits `Event.stopped` exactly once per start, whatever ended it (`stop()`, a final on the legacy recognizer, a failure, an interruption); a replaced start ends silently. `Event.final` is one segment (the `SpeechAnalyzer` path emits one per pause and keeps listening); `finalizedText` holds every segment so far. `Event.interrupted(.began / .inputRouteLost / .mediaServicesReset)` precedes `.stopped` when the audio session takes the microphone; `resumesAfterInterruption: true` (default false) restarts on the system's go-ahead. `onDeviceOnly: false` is the explicit opt-in that lets a locale with no on-device model use network recognition (status event says so); the default fails closed. **Core ML (#364):** `CoreMLModelValidator`, `CoreMLValidationReport`, `CoreMLArtifactValidationReport`, `ParakeetPipelineConfig`, `ParakeetVocabulary` are public (`validateInstalledModel(at:)`, `validateInstalledArtifact(backend:requiredFiles:requiredStageNames:at:)`, `stageURL(for:in:)`, `readyMarkerName`, `hasReadyMarker(at:)`); `CoreMLModelLoader` loads a stage asynchronously (`.mlpackage` compiled first, `.cpuAndNeuralEngine` by default) and `loadComputePlan`. **Model downloads (#364):** `ModelDownloadCoordinator.prepare(_:config:progress:)` reuses an installed model, else adopts a staged or retained archive, else (`ModelGrantClient` grant, `ModelDownloader` background `URLSession` download with retries and one grant refresh) and installs through `ModelInstaller`, which wraps `ArtifactInstaller`: SHA-256 verified, activated with the previous install aside, validated by loading every Core ML stage, and rolled back on failure, so the newest working model is never lost and no `.previous` copy outlives a settled install. `ModelDownloadStaging` parks a download that finished while the app was not running; `ModelDownloader.handleEventsForBackgroundURLSession` / `reattach()` are the app hooks. `ModelArtifactSpec`, `ModelStoreLayout` and `ModelDownloadConfig` carry the app's catalogue. Needs `RightKitArtifacts`. |
| `RightKitAudio` | Microphone capture and recording for iOS, watchOS and macOS, with the session policy left to the caller (#363, #597). `WarmAudioCapture`: a kept-warm `AVAudioEngine` tap that converts to a caller-supplied format and hands buffers to a handler on the tap thread; the engine is rebuilt on every start and on `AVAudioEngineConfigurationChange` behind generation guards; `stop()` seals delivery; `startVerified` / `verifyDelivering` / `deadTapTimeout` report an engine that started with a dead tap (`CaptureRawHealth.verdict`). `AudioFileRecorder`: 16 kHz mono 16-bit PCM (or AAC) to a URL with `start(to:)`, `stop()`, `cancel()`, `meterLevel`, permission request, session activation, a finalize wait so the file is complete when `stop()` returns, and interruption handling (`.stopAndKeepFile` or `.pauseAndResume`) reported on `events`. `AudioSessionController` (activate / deactivate with the watchOS 27 async API), `AudioSessionConfiguration`, `MicrophonePermission`, and `AudioSessionMonitor` (interruption, route change, media-services events as `AudioSessionEvent`). |
| `RightKitTTS` | On-device text to speech (#628). `SynthesisPipeline` chunks text by sentence (`TextChunker`), renders each chunk with `AVSpeechSynthesizer.write`, levels it, and streams `AudioChunk`s (`chunks(for:)`, cancelled when the consumer stops) or stitches them (`synthesize`); `VoiceSelection` / `VoiceInfo.installed` pick the voice. `PlaybackController` (`@Observable`, main actor) plays chunks as they arrive with pause, resume, `skip(to:)` by chunk, `setRate` (0.5 to 2.0, pitch preserved), and handles interruptions (resumes only on the system's recommendation), unplugged headphones (pauses), media-services reset and engine reconfiguration (rebuilds from the chunk in progress). |
| `RightKitPurchases` | StoreKit 2 purchases, entitlement ledger and access hints behind a `PurchaseBackend` seam. `PurchaseOutcome.unknown` covers StoreKit's `@unknown default` result and shows `PurchaseMessages.unknownOutcome`. |
| `RightKitCloudSync` | Generic CloudKit private-zone sync engine. One adapter can own several record types (`recordType(of:)`, `recordType(forItemID:)`) and read direct types it never uploads (`directRecordTypes`, written through `CloudSyncEngine.writeDirect`). `SyncTransport.fetchRecord(named:)` reads one live record. App-side failures back off with the engine (`noteFailure`, `raiseRetryGate(toAtLeast:)`, `retryGate`). `adoptLegacyLedger` imports a pre-RightKit ledger with its change token and versions without an account call. `encodeForUpload` / `SyncLazyPayload` produce payloads lazily at upload time. `SyncTrigger.submit`. |
| `RightKitUI` | Generic adaptive-glass surface primitive: regular/clear native Liquid Glass + ConcentricRectangle on iOS/macOS 26, material fallback on older OS versions, and opaque accessibility fallback. Product radii, colors, layout, motion, and behavior stay app-local. |
| `RightKitStore` | App Group primitives: cross-process command queue, file lock, owner lease, and revisioned state store. `AppSettingsStore` is a declared-key, typed settings store over App Group `UserDefaults` (manifest of keys, kinds, defaults and protected flags, all-or-nothing verified `patch`, `null` resets, revision, change events, `pollExternal()`); code generation and the cross-language parity check stay in tooling. |
| `RightKitImportQueue` | Share-extension intake: the pending share payload shared between an extension and its app, and a queue that imports it. |
| `RightKitLiveActivity` | Live Activity lifecycle rules (session binding, terminal restart, adoption, watchdog). The app supplies its own phase type. `KeyedLiveActivityHost` runs one activity per key (`start(key:...)`, `update(key:...)`, `end(key:...)`, `endAll(except:)`, `endForeignOrphans(isClaimed:)`) under per-key fences; `LiveActivityHost` keeps the single-activity behaviour. |
| `RightKitFS` | Contained file access. `RightKitAppRoot` (`Orthic Labs/<App>` or an explicit `init(root:name:)`): `path`, `dataDir`, `atomicWrite`, `copyVerified`, `appendRecord`, `readData`, `exists`, `remove`, `list`, and `migrate(from:to:)` (verified, one-shot, never overwrites). `RightKitContainedRoot(base:)` does the same containment (`resolve(_:allowRoot:)`, `atomicWrite`) under any base directory. Refuses `..`, absolute paths, NUL, and symlinks (dangling, looping or leading outside); mirrors `rightkit-fs` `contain_lenient`. |
| `RightKitWatchBridge` | WatchConnectivity transfer ledger and delivery states. |
| `RightKitIntents` | App Intents recording bridge: Darwin notifications, microphone permission handshake, and recording actions. |
| `RightKitAppleCredentials` | Security.framework credential backend: raw-status probing (`probeStatus`), all-location deletion (`deleteAnyScope`), and `ForeignGenericPassword`, a read-only reader for another app's legacy file-Keychain item that does not prompt by default (macOS). |
| `RightKitOverlay` | Overlay state, hit testing, and the non-activating overlay panel model (the panel is macOS only), with per-rectangle click-through (`setClickThrough`, `hitRegions`, `updateClickThrough(forMouseAt:)`). |
| `RightKitWatch` | FSEvents file watcher (macOS): `RightKitWatcher` delivers typed item, root-changed, history-done and must-rescan events as an `AsyncStream`, with event IDs for replay. |
| `RightKitAccessibility` | Accessibility API for macOS apps (empty on iOS): `RightKitAXTrust.isTrusted(prompt:)`, `RightKitAXElement` (typed reads of role, title, windows, frame; writes of position, size, frame, any attribute; `perform`), `RightKitAXWindowAction` (raise, minimise, full screen, close, zoom through the window's buttons), and `RightKitAXObserver`, an `AXObserver` with an `AsyncStream` of events that tracks window creation and destruction. Failures are `RightKitAXFailure`. Needs Accessibility permission; the library never prompts unless asked. |
| `RightKitHotkeys` | Carbon `RegisterEventHotKey` global hot keys (macOS; empty on iOS): `RightKitHotkeyRegistrar` registers `RightKitHotkeyCombo` values (virtual key code plus `RightKitHotkeyModifiers`) and reports press and release on the main actor. No permission is needed. Also `RightKitHotkeyCombo.parse("cmd+opt+k")`, `RightKitEventTapHub` (one session event tap on its own thread with priority-ordered handlers and swallow; `start()` never prompts and returns false when macOS refuses), the pure `RightKitTapChain`, and `RightKitKeystrokes` (post a chord, Command-V paste, typed text or a mouse button, lifting any Shift, Option or Control the user still holds; needs Accessibility, check `canPost`). |
| `RightKitEngineClient` | Client of a `rightkit-job-queue` engine; no native symbols. `EngineClient` (call, subscribe, lifecycle), `EngineEvent`, `EngineError {code, message, retryable}`, `EngineLifecycle`, `EngineEmpty`, the typed `call(_:_:as:)` helper, and `AttachedEngine` (macOS: a verified Unix-socket client of a resident engine served by `rightkit-daemon`). Product-neutral: the app passes `stateDirectory`, `expectedExecutables`, `app`, `engineName` and `eventsPollMethod`. |
| `RightKitEngine` | Native-backed engine supervision: `SupervisedEngine` over `rightkit-job-queue`'s C ABI (`CRightKitEngine`, the `native/` staticlib), shutdown report types. Replaces the retired `RightKitPulse`. Re-exports `RightKitEngineClient`. The package does not link the archive; the app does (see "Hub shell and engine" below). |
| `RightKitHub` | Shared SwiftUI hub base: `HubSection`/`HubShell`/`HubScene`, `HubTheme`/`HubBrand`, toasts, search, components (`HubCard`, `HubBarChart`, ...), platform services (`HubPlatformServices`, `MacPlatformServices`), and the settings kit (`HubSetting`, `HubSettingsView`, `EngineSettingsStore`). Re-exports `RightKitEngineClient`, not `RightKitEngine`. |
| `RightKitHubQA` | Scripted `QAEngineClient`, `RecordingPlatformServices`, `InMemorySettingsStore` and a component gallery for QA runs. Release builds never link it. |
| `RightKitSystem` | macOS system services (empty on iOS): `RightKitProcesses` (running apps, Dock apps, launch/terminate/activate `AsyncStream`, kernel process table by executable suffix, pid liveness with start-time check, `stop`), `RightKitBoundedProcess` (timeout and capped output), `RightKitSupervisedChild` (restart backoff, crash latch, rotated stderr log via `RightKitChildLog`), `RightKitBanners` (Notification Center banners, presenter, permission asked only when undetermined) with `RightKitThresholdTracker`, `RightKitLoginItem` (`SMAppService`, optional install-path rule), `RightKitPermissionProbes` (silent status reads and the standard system prompts), `RightKitRuntime` (test-host detection and real-data guards). |

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

### Runtime key permission, style mask and the full-screen split (RightKitOverlay)

- `panel.allowsKey` (seeded from `configuration.canBecomeKey`) is what `canBecomeKey` answers; set it at runtime. `canBecomeKey` is `open`, so a subclass may override it; `canBecomeMain` stays `final` false.
- `RightKitOverlayPanelConfiguration.styleMask` (default `[.borderless, .nonactivatingPanel]`) is applied at creation; drop `.nonactivatingPanel` with `canBecomeKey: true` for a panel that activates the app (an unlock panel).
- Off-main full-screen check: `RightKitOverlayFullScreen.context(on:...)` on the main actor, `onScreenWindows()` (blocking WindowServer call) on any thread, then `evaluate(_:windows:)`. `Context.needsWindowList` is false when the answer is already known.

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

## Hub shell and engine (RightKitHub, RightKitEngine, RightKitEngineClient)

Moved from Pulse (issue 425); names are RightKit's and nothing in them is product-specific. The app supplies its own state folder, engine name, `hello` app label and events poll method to `AttachedEngine`, and its own environment variable names to `SupervisedEngine(environmentKeys:)`.

- `RightKitHub` and `RightKitHubQA` are views and obey the control rule (only controls SwiftRight has proven; no Path, ProgressView, `.sheet`, Form, Picker, gradients, `.background`, ...). Check with `python3 swift/RightKitSwift/scripts/lint-controls.py` (reads `proven-controls.json`, a copy of SwiftRight `docs/proven-controls.json` at commit 455bb09f; refresh by copying it again). `HubTheme.swift` is to be generated by the `rightkit-theme` CLI; do not tune it by hand.
- `RightKitEngine` needs the native archive. The package does not link it (no `unsafeFlags`, no library path). The owner builds `native/` (a Rust staticlib crate, its own `[workspace]`, crate `rightkit-engine-native`) and the app links `librightkit_engine_native.a`, adds `native/target/release` to its library search paths and adds the system libraries that `rustc --print native-static-libs` reports for that crate (macOS: CoreFoundation and Security frameworks, `-liconv -lSystem -lc -lm`). `nm -g` must list every `rkp_*` symbol and `rke_install_engine`. `RightKitHub` and `RightKitEngineClient` need none of this.
- ABI rules kept by `SupervisedEngine`: one engine command and one snapshot per process (a second with a different snapshot is refused with `stale_generation`); no cancel (cancelling a task detaches the wait, the native call runs to its result or timeout); the event callback copies the frame and returns; buffers are freed with `rkp_buf_free`.
- `rightkit-engine-probe` (an executable that starts a `SupervisedEngine`, calls `ping`/`status` and exits) lives in `Sources/rightkit-engine-probe`. It links the native archive, so it is not a product of the package manifests; build it by hand with the link flags above.

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
    .package(url: "https://github.com/bogusyogi/rightkit", exact: "0.9.0"),
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

<!-- credentials-license-lane:start -->
## Credentials and license additions (round 2)

- `SecretsStore.keys(prefix:)` lists account names without reading values; `set(_:_:tag:)` and `tag(_:)` keep a small non-secret tag in `kSecAttrGeneric`.
- `AppleCredentialStore(accountValidator: AppleCredentialStore.arbitraryAccounts)` allows URL-shaped accounts; `readData` / `writeData` skip trimming; `accounts(scope:)` enumerates; `read(_:scope:broadSynced: true)` opts into the broad synchronizable lookup; `status()` reports `.missingEntitlement` for the store's own service. `SyncedCredentialReconciler` applies the opt-in, never-overwrite sync policy.
- `ContinuityBridgeClient` publishes and exchanges bridge tokens; inject a `ContinuityBridgeTransport` to test without a `URLProtocol`. A bridge lease carries no entitlement until `LicenseActivationClient.license(fromLease:...)` verifies it.
- `KeychainLicenseStore.requireDeviceID(account:options:)` takes a `DeviceIDOptions` seed, accessibility class and corrupt-id policy.
- `SyncedEnvelopeStore` syncs a provider key as a revisioned envelope (`{v, app, provider, credentialId, revision, updatedAt, state, secret}`): `pull` returns nothing, adopt, erase, conflict or unreadable; `publishTombstone` propagates a deletion without resurrection. The caller applies a pull to its device-only store, then calls `markApplied`.
<!-- credentials-license-lane:end -->

## RightKitHelperProtocol (issue 735)

Foundation-only wire helpers for Swift CLI helpers driven over stdin/stdout. `HelperProtocol.serve(input:output:handler:)` reads line-delimited JSON requests and writes one JSON response line per request (flushed per line) until EOF. Malformed and oversized lines get a typed `{"ok":false,"error":"..."}` reply and the loop keeps going. `runOnce` serves a spawn-per-request helper. The module holds no global handles: the caller passes `.standardInput` and `.standardOutput`.

```swift
exit(HelperProtocol.serve(input: .standardInput, output: .standardOutput) { (req: Request) in
    Response(ok: true, value: handle(req))
})
```


<!-- BEGIN network-lane (RightKitHTTP, RightKitLLM, RightKitUpdate, RightKitOAuth, RightKitAccount) -->
## Network modules (issues 365, 415, 627)

| Target | What it does |
|---|---|
| `RightKitHTTP` | Async JSON client (`HTTPClient`): HTTPS only (HTTP to loopback), no embedded credentials, bounded bodies, bearer, query, JSON and form bodies, status mapping to `HTTPError` (the server's own `error`/`message`/`detail` text, the bearer removed), idempotent-verb retry with `Retry-After`, and `download` to a `.part` file with size and SHA-256 enforcement. Redirects off HTTPS are refused and `Authorization` is dropped on a cross-host hop. `HTTPTransport` is the test seam. |
| `RightKitHTTP` network policy | `NetPolicy` extends the URL policy (no second module): public-only by default, with opt-in `allowing: [.loopback, .privateNetwork]` per user-configured endpoint. Link-local and cloud-metadata (`169.254.169.254`, `fd00:ec2::`, `metadata.google.internal`), unspecified, multicast and reserved destinations can never be allowed; IPv6 forms that embed IPv4 and legacy IPv4 spellings are classified by the embedded address, and DNS answers are checked (one bad address denies the host). `HTTPClient(policy:)` enforces it before sending; `PolicyRedirectGuard` and `NetPolicy.makeSession()` check every redirect hop. |
| `RightKitLLM` | OpenAI-compatible chat completions (`LLMClient`) over `RightKitHTTP`: presets that match `rightkit-llm` (OpenAI, OpenRouter, Groq, DeepSeek, Together, Mistral, xAI, Gemini, NVIDIA, Ollama, LM Studio), keys from a RightKitCredentials `SecretsStore` (account = provider id), a caller-set `User-Agent` (Groq refuses the default), data-driven model quirks (`reasoning_effort: none` for Qwen; low effort and no reasoning text for Groq gpt-oss), truncation and empty-output errors, and ordered failover across routes with an `accept` hook for app-side output checks. |
| `RightKitUpdate` | Update-check owner, the Swift counterpart of `@rightkit/app-update`: `SemanticVersion` (same precedence vectors), `UpdateChannels` (name rule, https-only endpoints, fallback), `CheckGate`, `BootProbation`, release feeds (`GitHubReleaseFeed` with ETag, `StaticManifestFeed` for `latest.json` with a required `patch`/`update` tier, `AppStoreLookupFeed`, `ReleaseStatus`, `NightlyDiscovery` with warn-once gating) and `UpdateClient` (check, download, stage, install). On macOS `MacAppInstaller` extracts a `.dmg` or `.zip`, requires a `DeveloperIDVerifier` (bundle id, version, Developer ID requirement pinned to a team, strict nested-code validation, Gatekeeper notarization) on the staged copy and again on the copy placed beside the install, then swaps it in atomically. It cannot be built without a verifier, so an unverifiable artifact is never installed. No step keeps a `.previous` copy and a stale one is deleted (decision D1), so `BootProbation` reports `failed` instead of a rollback path. iOS is check-only (`AppStoreLookupFeed`). |
| `RightKitOAuth` | Authorization code + PKCE (S256 only; verifier and state from `SecRandomCopyBytes`), `OAuthClient` (token exchange, refresh that keeps the old refresh and ID tokens when omitted, RFC 7009 revoke, providers that report errors with HTTP 200, `invalid_grant` as `needsReauth`), `OAuthSession` (single-flight refresh over an `OAuthTokenStore`), `SecretsTokenStore` (the stored record is byte-compatible with `rightkit-oauth`'s token codec), and `OAuthSignIn` with `SystemWebAuthRunner` (`ASWebAuthenticationSession`; custom URL scheme redirects). |
| `RightKitAccount` | Suite shop/account client: `AccountSessionClient` (Sign in with Apple, magic link, renew, sign out, delete account, device list/rename/remove, link a StoreKit purchase) with `AccountSessionError`, `AccountDeviceRow`, the device-local `AccountSessionKeychainStore` on `SecretsStore`, and `AccountSessionStore` (a clearing fence so a late sign-in or renewal never undoes a sign-out; `renewChecked()`; `onChange`). `requestWindowsLinkCode` stays in `RightKitLicense`. |
<!-- END network-lane -->

## PK16: shared owners for the Apple hosts (0.9.0 staging)

Which product carries which host mechanic, and what each app keeps. Names are the ones in `CHANGELOG.md`.

| Consumer and anchor | Shared owner | App keeps |
|---|---|---|
| Pulse hub: `HubSettingsStore`, `HubServicesEnvironment`, `HubPlatformServices` | `RightKitHub` (the same source; `RightKitHub` re-exports `RightKitEngineClient` only, so a target that supervises an engine imports `RightKitEngine` itself) | menus, sections, action semantics |
| Pulse `ResidentEngineClient::{current,detach,call}` | `ResidentEngineLink` over `AttachedEngine` (`RightKitEngineClient`) | app label, executable path, the "wanted" setting, the health probe |
| Pulse `mac/Notch/Sources/Updater` | `GitHubReleaseFeed`, `SemanticVersion`, `DeveloperIDVerifier`, `MacAppInstaller`, `MacAppRelauncher` (`RightKitUpdate`) | bundle id, team id, repository, install path, user-visible copy, `Runtime.refuseUnderTest` guards |
| Pulse `EngineService` (not an inventory anchor) | `RightKitLoginItem(kind: .agent(plistName:))` covers register, unregister and status; the `launchctl` job probe and the kickstart-once rule are still Pulse's | |
| Pulse iOS `AppLock` | `AppLockCoordinator`, `SystemOwnerAuthentication`, `OwnerAuthenticationResult`, `BiometryKind` (`RightKitCredentials`) | the enabled setting and default, prompt text, lock screen |
| HeardRight helper `CredentialStore` | `AppleCredentialHelperHost` (`RightKitAppleCredentials`) | service, access group, account allowlist, message wording |
| HeardRight `CloudSyncNotifications` | `CloudKitWakeHost` (macOS), `CloudKitWakeFilter`, `CloudKitEntitlement`, `CloudKitSubscriptionLifecycle` (`RightKitCloudSync`) | container, zone and subscription ids, the `hr_cloud_sync_*` FFI, the signed environment gate, record schema and merge rules |
| HeardRight `PillCallbacks` | `RightKitCallbackRelay`, `RightKitMainThread` (`RightKitSystem`) | the C ABI, callback payloads, domain actions |
| HeardRight iOS licence | `LicenseController` (compare-and-save refresh, immediate removal), `PendingLicenseRemovalQueue`, `LegacyLicenseRemovalMigration`, `BridgeConsumptionGate`, `KeychainLicenseStore.loadNow/saveNow/removeNow` (`RightKitLicense`) | endpoints, copy, Windows link policy, entitlement source precedence |
| HeardRight iOS Pro bridge | `ContinuityBridgeClient`, `ContinuityBridgeExchange/Issue.verifiedLicense`, `exchangeForLicense` (`RightKitLicense`) | `ProBridgeFailure` copy, StoreKit proof, `ProResolutionState` |
| HeardRight iOS account | `AccountSessionClient/Store` (`RightKitAccount`) | Apple and magic-link UI, the notification name, delete-account policy |

Adopting the removal queue keeps an owed removal and the suppression flag: call `LegacyLicenseRemovalMigration.adopt(into:defaults:activationTokenKey:leaseRemovalsKey:)` and `adoptBridgeSuppression(into:defaults:legacyKey:)` once at launch, before the first `LicenseController`, with the keys the app used before (HeardRight: `heardright.license.pendingDeactivationToken`, `heardright.license.pendingLeaseRemovals`, `heardright.bridge.consumption.suppressed`).

## Native evidence owed (PK16-1)

The generic `swift test` run cannot reach a Secure Enclave, an entitled Keychain access group, iCloud or a store. For each API a named consumer uses: the typed result that exists, and what a device or entitled-host run must still show before the tag is called qualified for that consumer.

| API (consumer) | Typed result today | Native evidence owed |
|---|---|---|
| `AppleCredentialStore` / `SecurityAppleCredentialBackend`, synced scope (HeardRight helper, iOS) | `AppleCredentialError.missingEntitlement`; `CredentialProbe` | a build signed with the `keychain-access-groups` entitlement writes on one Apple device and reads on another; `status` ready and unavailable |
| `AppleCredentialHelperHost` over the Security backend (HeardRight helper) | the wire errors in `AppleCredentialHelperHostTests` | the nested login-item helper, spawned by the real app, answers each command |
| `KeychainSecretsBackend`, `KeychainLicenseStore`, `AccountSessionKeychainStore` (HeardRight iOS, CodeRight iOS) | `SecretsError.keychain`, `LicenseStorageError.keychain` | a device run: device-only accessibility class, survives relaunch, absent from iCloud Keychain |
| `CloudKitSyncTransport` (ScrapeRight, CodeRight, HeardRight iOS) | `SyncTransportError`; CloudKit itself TRAPS without the entitlement and cannot be asked on iOS | a signed device build against the Development environment; no production traffic in a test |
| `CloudKitWakeHost` (HeardRight macOS) | `CloudKitEntitlement.containerNotEntitled` / `.cloudKitServiceMissing` (macOS reads its own signature); `.unsupportedPlatform` elsewhere | a signed macOS build: subscription saved, a remote push accepted, a foreign push refused, account change re-subscribes |
| `OwnerKey` (Pulse) | `OwnerKey.Failure.unavailable`; `isAvailable` | a Mac with Touch ID: key created without a keychain access group (documented as unverified), sign, biometry-set change |
| `UserVerification`, `SystemOwnerAuthentication`, `AppLockCoordinator` (Pulse iOS) | `UserVerificationOutcome.unavailable`, `OwnerAuthenticationResult.noPasscodeSet` / `.unavailable` (mapping tested) | an iPhone with Face ID: unlock, cancel, lockout, no-passcode device; the lock on return after 30 s |
| App Group stores and queues (CodeRight widgets, ScrapeRight and HeardRight share extensions) | `AppGroupStoreError.lockUnavailable` / `.defaultsUnavailable` (tested) | an app plus extension pair on a device sharing the group container |
| `StoreKit2Backend`, `PurchaseController` (ScrapeRight, CodeRight, HeardRight iOS) | `PurchaseBackendError.unsupportedPlatform`; StoreKit test session coverage | sandbox purchase, restore, refund and offer-code on a device |
| `LiveActivityHost` (HeardRight iOS) | `LiveActivityStartError.disabled` | a device with Live Activities enabled and disabled |
| `LoginItem` agent registration (Pulse) | `RightKitLoginItemStatus` (`requiresApproval`, `notFound`) | the installed app registers the agent at `/Applications` and approval is shown |
| `AttachedEngine` peer verification (Pulse) | `unsupported_platform` off macOS; `engine_starting` | the installed app attached to the installed engine (`getpeereid`, `proc_pidpath`) against a live heartbeat |
| `MacAppInstaller` / `MacAppRelauncher` (Pulse) | `UpdateError` cases | a notarized build updating itself from a dmg and reopening |
| AX, event tap, hot keys, overlay (Pulse notch) | `RightKitAXTrust`, `start() == false` without permission | the installed app with and without the privacy grants |


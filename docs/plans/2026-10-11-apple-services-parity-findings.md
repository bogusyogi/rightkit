# Apple services parity: what HeardRight iOS does and what each iOS app lacks (ledger #102)

Read-only source lookup, 2026-10-11. No builds, no network. Tags: V verified in source, D documented not verified, N not found.

Four of the five things you asked for are covered below: Q1, Q2 (the credential path, traced in HeardRight source), Q3 and Q4, plus the Q5 table. Short answer on credentials: only Pro and licence state crosses between desktop and phone. Provider API keys do not cross in HeardRight's code. Paths are relative to `/Volumes/D/claude` unless they start with `swift/`, `crates/` or `docs/`. Those three prefixes are under `rightkit/`.

Status tags: V = verified in source, D = documented, not verified, N = not found.

## Q1. RightKitSwift products, versions and tags

**HeardRight pins 0.4.0 (V).**
- `heardright/ios-native/project.yml:17-19` sets `exactVersion: "0.4.0"`.
- `HeardRightNative.xcodeproj/project.xcworkspace/xcshareddata/swiftpm/Package.resolved` resolves `rightkit-swift` 0.4.0 at revision `88a18dc620896ff5df80bf35e1f8c94685bca875` (V).

**Products the HeardRight app target links (V, `project.yml:131-154`) and where it uses them:**

| Product | What it does | HeardRight use |
|---|---|---|
| RightKitCloudSync | CloudKit private-zone sync engine | `HeardRight/TranscriptCloudSync.swift:3`, transport at `:229`, engine at `:256` |
| RightKitPurchases | StoreKit 2 purchases, entitlement ledger, account-token store | `HeardRight/StoreKitProController.swift:4`; `PurchaseController` and `StoreKit2Backend()` at `:117-123` |
| RightKitLicense | Ed25519 token verify, activation and lease client, removal queue, bridge-consumption gate | `HeardRight/LicenseController.swift:3`, client built at `:193-195`; `Shared/LicenseEntitlement.swift:16` |
| RightKitAppleCredentials | Keychain store with synced or device-only scope | `Shared/AppleSyncedCredentialStore.swift:4`, store built at `:103` |
| RightKitStore | App Group queue, lock, lease | `HeardRight/RecordingOwnershipLease.swift:3`; `Shared/SharedDictationCommand.swift:2` |
| RightKitDiagnostics | Bounded JSONL log with redaction | `Shared/DiagnosticsReport.swift:2` |
| RightKitImportQueue | Share-extension intake | `Shared/SharedImportQueue.swift:2` |
| RightKitLiveActivity | Live Activity lifecycle | `HeardRight/RecordingLiveActivityController.swift:4` |
| RightKitWatchBridge | WatchConnectivity ledger | `HeardRight/WatchRecordingInbox.swift:4` |
| RightKitSpeech | On-device ASR | `HeardRight/OnDeviceTranscriber.swift:2` |
| RightKitUI | Glass surface primitive | `HeardRight/RecordingPillView.swift:2` |
| RightKitIntents | App Intents recording bridge | `Shared/HeardRightAppIntents.swift:8` |

HeardRight does not link RightKitArtifacts, RightKitOverlay or RightKitWatch.

**Tags (V for what is local):**
- `git tag -l` in `rightkit` returns nothing. `.git/refs/tags` is empty and there is no `.git/packed-refs`.
- Remote tags on `github.com/bogusyogi/rightkit-swift` cannot be checked without network access.
- `swift/ios-app-template/project.yml:15-17` says tag 0.3.0 was verified with `git ls-remote` on 2026-10-10 (D).
- The feed `packages/release/rightkit-versions.json:363-374` has `swift.rightkit-swift` and `stagedSwift.rightkit-swift` both at 0.4.0 (V).
- `swift/RightKitSwift/VERSION` is 0.6.0. `docs/PLATFORM-MATRIX.md:33` calls this "untagged" (V).

**Changes from 0.4.0 to the tree (`swift/RightKitSwift/CHANGELOG.md`, V):**
- **0.5.0**
  - RightKitAppleCredentials gains `ForeignGenericPassword` (macOS, read-only).
  - RightKitOverlay gains per-rectangle click-through.
  - New product RightKitWatch (FSEvents file watcher, macOS only).
- **0.6.0**
  - RightKitDiagnostics gains `StoreOptions`, redaction policies, and async `append` and `drain`.
  - RightKitArtifacts: `install` is now `async`, takes `InstallOptions`, needs a digest unless `allowUnverified` is set, and writes a manifest with crash recovery. This is source-breaking.
- **Unreleased (S01-S06)**
  - Speech engine concurrency fixes.
  - CloudSync state store takes a cross-process `flock`.
  - RightKitWatch event buffer is bounded.
  - **Wire change:** RightKitLicense lease activation now sends `platform=macos` on macOS (it sent `ios` before), so Macs now count against the computer seat pool.
  - The CloudKit helper refuses requests whose protocol version is not 1.
- Nothing in 0.5.0 or 0.6.0 adds to CloudSync, Purchases or the bridge-consumption gate. Those changes are in Unreleased (S03, S05), not 0.5.0 or 0.6.0.

## Q2. Credential path and Pro bridge, reported separately from CloudKit data sync

### CloudKit is the transcript data path only (V)

- Container `iCloud.app.heardright.ios.native` (`TranscriptCloudSync.swift:198`), record type `Transcript` (`:201`), subscription `heardright.transcripts.subscription.v1` (`:200`).
- Zone `HeardRightTranscripts` (`heardright/tauri-app-next/src-tauri/src/cloud_sync.rs` header).
- Fields written are `id`, `text`, `rawText`, `createdAt`, `updatedAt`, `source`, `folder`, `tags`, `metadataJSON` (`TranscriptCloudSync.swift:48-60`). No credential, key or licence field.
- `docs/APPLE_CONTINUITY_ARCHITECTURE.md:11-15` states that CloudKit, Keychain and entitlement are three separate stores.

### What crosses, by mechanism

**1. Pro bridge token through iCloud Keychain (Apple devices only).**
- The token is opaque and issued by the backend. The docs say it "grants nothing locally", and the code never verifies it locally.
- Keychain item: service `app.heardright.shared.credentials`, access group `6KLGD3LLKF.app.heardright.shared`, account `pro.bridge`, synchronizable.
  - iOS side: `AppleSyncedCredentialStore.swift:58-59` (V).
  - Mac helper: `macos-native/HeardRightPillMac/Sources/HeardRightAppleCredentials/main.swift:147-175` (V).
- **Mac desktop publishes the token.**
  - `src-tauri/src/pro_bridge.rs:385` `publish_inner` posts the activation token to `/v1/licenses/bridge/from-activation`.
  - It then writes the returned `bridgeToken` to the synced Keychain item through the helper.
- **iOS publishes from StoreKit but sets `publishBridge: false`.**
  - `HeardRight/ProEntitlementResolver.swift:224-233` calls `ProBridge.fromStoreKitURL`. It ignores any returned bridge token and never publishes a cloud bridge from iOS StoreKit.
  - So the Mac is the only publisher of the synced token.
- **Phone and Mac consume it.**
  - `pro_bridge.rs:449` `consume_inner` and `ProEntitlementResolver.swift:325` post the token to `/v1/licenses/bridge/exchange`.
  - The server returns a device lease, which is stored through `license::persist_leased_license` (`pro_bridge.rs:~520`).
- **iOS wire client:** `Shared/ProBridge.swift:23-90`.
- **RightKitSwift has no bridge client (V).** It only ships `BridgeConsumptionGate` (`RightKitLicense/LicenseRemovalQueue.swift:255`). The HTTP calls and payloads are HeardRight-local.

**2. StoreKit.**
- `StoreKitProController.swift:117-123` uses RightKitPurchases with `KeychainAccountTokenStore(service: "app.heardright.ios.native.storekit", account: "app-account-token")`.
- The `appAccountToken` is a random UUID (`RightKitPurchases/PurchaseConfig.swift:7-17`). It is used only to link an upgrade purchase to its base purchase (`accountToken: .inherited(from:)`, `StoreKitProController.swift:68-69`). It does not link a phone to a desktop.
- The signed transaction is sent to the server, which verifies it. Per `docs/PRO_BRIDGE_BACKEND_CONTRACT.md` the server checks signature, bundle id, product id, environment and revocation (D).
- Also present: `LicenseEntitlement.swift:92` `linkStoreKitURL` (`/v1/shop/pro/link-storekit`). I did not trace its call site.

**3. Licence lease from the licence server.**
- The base URL is `https://heardright.app/v1/licenses/` (`LicenseController.swift:~183`).
- **Server source is not in this workspace (V by absence).** `docs/PRO_BRIDGE_BACKEND_CONTRACT.md:3-9` says the backend is not in the repo.
- I searched `heardright/cloudflare-workers` (only `model-serve`), `heardright/owner`, `sellright`, `rightkit/packages` and `rightkit/crates/rightkit-license` for `bridge/exchange`, `license_bridge` and `windows-link`. There were no hits for server code.
- The SellRight licensing API source: N.

**4. Provider API keys — they do NOT cross in HeardRight's code.**
- The iOS store has an `api.<provider>` account type (`AppleSyncedCredentialStore.swift:36-46`).
- The only user of it is iOS BYOK polish keys, for Groq and OpenRouter (`HeardRight/BYOKPolishKeyStore.swift:100-133`; `ContentViewSettings.swift:376`).
- These sync iPhone-to-iPhone or iPhone-to-Mac through iCloud Keychain, only when the user enables "Sync across Apple devices".
- **The Mac helper rejects anything but two accounts.** `main.swift:147-149` defines `enum Account { proBridge = "pro.bridge", syncConsent = "sync.consent" }`, and its header says "two allowlisted synchronized accounts" (`:11`). `api.*` is not writable from the desktop.
- Desktop provider keys live in the OS keyring, device-only (`src-tauri/src/l3_keys.rs:1`, `keyring_core`).
- I searched the helper allowlist, `pro_bridge.rs`, `ProBridge.swift` payloads and the CloudKit adapter fields, and found no path where a desktop-entered provider API key reaches iOS. The owner's lead (keys travel via the Pro bridge) is not supported by the code.

### What is never synced

- `AppleSyncedCredentialStore.swift:14-24` and `docs/APPLE_CONTINUITY_ARCHITECTURE.md:33-35,169` list the never-synced items: raw licence key, StoreKit JWS (sent only to the issue endpoint), device id, activation token, signed entitlement, licence clock, DB keys.
- Audio never enters CloudKit (`APPLE_CONTINUITY_ARCHITECTURE.md:24`).

### Trust model, encryption and keys

- **Same-person proof for the Apple path:** the same Apple Account, via iCloud Keychain and the shared access group. The server also enforces seat limits, source status and revocation (D, contract doc).
- **Transport:** HTTPS (`ureq` to `heardright.app`).
- **Encryption at rest:** the client adds none beyond the Keychain. The server stores only a hash of the bridge token (D, `PRO_BRIDGE_BACKEND_CONTRACT.md` schema).
- Whether iCloud Keychain is end-to-end encrypted is Apple's behaviour, not verified here.
- I found no app-level key that encrypts secrets in transit.

### Windows (V)

- `pro_bridge.rs:31` is `#![cfg(target_os = "macos")]`, so there is no Windows bridge client.
- `docs/APPLE_CONTINUITY_ARCHITECTURE.md:133-141` and `PRO_BRIDGE_BACKEND_CONTRACT.md` ("Windows") say Windows cannot consume iCloud Keychain or CloudKit.
- What Windows has is a link code (HR-PRO-015):
  - `src-tauri/src/license/lease.rs:649-750` implements request and redeem.
  - An already-leased Apple device calls `/v1/pro/heardright/devices/windows-link/request`. The code lasts 10 minutes and is single-use.
  - Windows calls `/windows-link/redeem` and gets its own computer-pool lease.
  - The `lease.rs:649-659` comments call out that it is not OS-gated.
  - Frontend: `src/lib/license/windowsLink.ts`.
- **rightkit-sync on Windows:** `crates/rightkit-sync/src/helper/process.rs:107-111` returns `Unavailable` when no helper binary exists, and there is no Windows helper in the repo. `docs/PLATFORM-MATRIX.md:93,193` agrees. No CloudKit Web Services or relay code is present (N).
- HeardRight desktop does not use rightkit-sync (V). `tauri-app-next/src-tauri/Cargo.toml` has no such dependency. It uses its own `cloud_sync*.rs` plus the `HeardRightCloudSync` Swift helper. `crates/rightkit-sync/API.md:154` lists migrating it as a plan ("Replace ...").

### RightKit-side credential pieces

- RightKitAppleCredentials: `AppleCredentialStore` with `AppleCredentialScope` `.synced` and `.device` (`RightKitAppleCredentials.swift:9,77`).
- `crates/rightkit-secrets/src/apple_helper.rs` is a Rust client for a synced-Keychain Swift helper. The helper binary itself is not in `swift/`. It lives in `heardright/tauri-app-next/macos-native` and `scraperight/macos-native` (V).
- Windows has Credential Manager, device-only (`crates/rightkit-secrets/README.md`).

## Q3. Host-app checklist as recorded

**Conventions (V in the HeardRight and ScrapeRight project files):**
- CloudKit container: `iCloud.app.<app>.ios.native`.
- App Groups: `group.app.<app>.ios.native` plus the shared `group.app.rightsuite.models`.
- Keychain access group: `$(AppIdentifierPrefix)app.<app>.shared`. The Security APIs need the literal `6KLGD3LLKF.app.<app>.shared`.
- StoreKit product ids: `app.<app>.pro.*`. HeardRight has `app.heardright.pro.ios.lifetime`, `.ios.upgrade`, `.full.lifetime` and `.lifetime` in `ios-native/StoreKit/HeardRight.storekit`. ScrapeRight has `app.scraperight.pro.lifetime` in `StoreKit/ScrapeRight.storekit`.
- Template convention (`swift/ios-app-template/Config/App.entitlements`): `iCloud.com.rightsuite.template`, `group.com.rightsuite.template`, and `Template.storekit` with `com.rightsuite.template.pro`.

**Entitlements HeardRight carries (`HeardRight.entitlements`):**
- App Groups
- `icloud-container-identifiers` plus `icloud-services` = CloudKit
- `aps-environment` = `$(APS_ENVIRONMENT)`, development in Debug and production in Release
- `applesignin`
- `keychain-access-groups`

**Info.plist keys (`project.yml`):**
- `UIBackgroundModes` with `remote-notification`, for CloudKit silent pushes.
- Also present: `NSSupportsLiveActivities`, `BGTaskSchedulerPermittedIdentifiers` and `NSMicrophoneUsageDescription`.
- The `.storekit` file is a `fileGroups` entry.
- `docs/APPLE_CONTINUITY_ARCHITECTURE.md:81` says push needs main-app APNs entitlements and matching provisioning profiles.

**Signing:** `docs/MAC_DEVELOPER_ID_SIGNING.md:35` says restricted CloudKit and APNs entitlements need an app-specific provisioning profile that authorises the bundle id, team and certificate. A helper must be a signed `.app` with an embedded profile (`swift/RightKitCloudKitHelper/README.md`). `docs/rules/release-signing.md` states only that Apple accounts exist and are provisioned, with no per-app list.

**CodeRight, what exists today (V):**
- Bundle ids: `app.coderight.CodeRight`, `.widgets` and `.fileprovider` (`CodeRight.xcodeproj/project.pbxproj:1972,2072,2096`).
- Team `6KLGD3LLKF`.
- Entitlements are App Group `group.app.coderight.ios` only, in all three entitlements files.
- README lines 76-79 say both App IDs must be explicit and the App Group associated.
- No iCloud, CloudKit, keychain-access-groups, aps or applesignin entitlements.
- The keychain-group gap is noted at `coderight/docs/canon/iOS-master-canon-gap-audit.md:153-159`.
- Links RightKitLicense, RightKitDiagnostics and RightKitStore at exactVersion 0.4.0 (`project.yml:19-20,111-116`).
- Not recorded for CodeRight: CloudKit container, schema deployment, StoreKit products, App Store Connect records, and whether the keychain group is registered.

**Not recorded for HeardRight:**
- App ID capability registration and App Store Connect product creation.
- CloudKit Production schema deployment. `cloud_sync.rs` (`PERMANENT_SCHEMA_FAILURE_BACKOFF`) only handles a Production-schema rejection (needs a Console schema deployment), and `docs/DOGFOOD.md:131` (HR-SYNC-013) lists installed-build qualification as an open gate.

## Q4. Reference and guide documents

**HeardRight integration references:**
- `heardright/docs/APPLE_CONTINUITY_ARCHITECTURE.md` is canonical.
- `heardright/docs/PRO_BRIDGE_BACKEND_CONTRACT.md`
- `heardright/ios-native/Shared/ProBridge.swift`
- `heardright/ios-native/Shared/AppleSyncedCredentialStore.swift`
- `heardright/tauri-app-next/macos-native/HeardRightPillMac/Sources/HeardRightAppleCredentials/main.swift` and `HeardRightCloudSync`
- `scraperight/macos-native/README.md` is the second example.

**RightKit docs:**
- `swift/RightKitSwift/README.md` (products and consuming) and `swift/README.md` (extraction contract)
- `crates/rightkit-sync/API.md` (wire protocol and per-app migration)
- `swift/RightKitCloudKitHelper/README.md` (manual real-CloudKit gate)
- `docs/PLATFORM-MATRIX.md`
- `/Volumes/D/claude/docs/RIGHTKIT-SHARED-PACKAGES.md` and `RIGHTKIT-SHARED-PLATFORM.md`
- `swift/ios-app-template/`

There is no dedicated adopter guide for CloudKit, the Pro bridge or credentials. The template has no README and only wires RightKit products (N).

## Q5. First-party iOS targets

HeardRight is the reference column. "Partial" means the code is present but not the full HeardRight shape. For everything below, "same as HeardRight" means the same mechanism and file shape, not identical code.

| App / bundle id | RightKitSwift pin and products | CloudKit sync | StoreKit | Pro bridge from desktop | Licence client | Credentials from desktop | Entitlements | Lacks vs HeardRight |
|---|---|---|---|---|---|---|---|---|
| **HeardRight** `app.heardright.ios.native` (`ios-native/project.yml`) | exact 0.4.0 (`:17-19`); 12 products (above) | Yes, `TranscriptCloudSync.swift:229` | Yes, `StoreKitProController.swift:117` | Yes, Mac publishes and iOS consumes via iCloud Keychain `pro.bridge` (`ProBridge.swift`, `pro_bridge.rs`) | Yes, `LicenseController.swift:193` | No for provider keys (allowlist is `pro.bridge` and `sync.consent` only); iOS BYOK keys sync Apple-to-Apple | iCloud/CloudKit, App Groups, keychain group, aps, applesignin | reference |
| **ScrapeRight** `app.scraperight.ios.native` (`scraperight/ios-native/project.yml:97`) | exact 0.4.0 (`:20-22`); AppleCredentials, Artifacts, Diagnostics, ImportQueue, Purchases, CloudSync, Store, License (`:101-116`) | Yes, `Sync/ICloudSyncModel.swift:75` | Yes, `Pro/ProPurchases.swift:36-39`; product `app.scraperight.pro.lifetime` | Yes, same shape: `Pro/ProBridgeResponses.swift`, access group `6KLGD3LLKF.app.scraperight.shared` (`Shared/AppleSyncedCredentialStore.swift:89`); desktop helper allowlist `pro.bridge` and `sync.consent` (`macos-native/.../main.swift:156-158`) | Yes, `Pro/ProLicense.swift:71` | No for desktop-entered provider keys: the allowlist has no `api.*` account | Yes, `ScrapeRight.entitlements`; `aps-environment` is hard-coded `development` (`project.yml:88`) | No Speech, UI, LiveActivity, WatchBridge or Intents; no Watch or Widgets target (not listed in the files I read); iPhone only (`TARGETED_DEVICE_FAMILY: "1"`); aps not build-config driven. Also has Artifacts. |
| **CodeRight** `app.coderight.CodeRight` (`coderight/apps/coderight-ios/project.yml`) | exact 0.4.0; License, Diagnostics, Store | No. No `CloudKit` use and no iCloud entitlement (N) | No (N in `CodeRight`; grep of StoreKit imports returned none) | No. App-local `License/` files only | Partial. `License/LicenseStore.swift:47-50` uses `LicenseVerifier` only; I did not find `LicenseActivationClient` there | No. Keychain is app-local (`Storage/KeychainStore.swift`, `LocalRuntime/*SecretStore.swift`), no access group | App Group `group.app.coderight.ios` only | Lacks RightKitCloudSync, RightKitPurchases, RightKitAppleCredentials; no iCloud container, no keychain access group, no aps, no `.storekit`, no bridge client, no `remote-notification` background mode |
| **SellRight** `app.sellright.ios.admin` (`sellright/ios/project.yml`) *(outside RightKit scope, listed for completeness)* | `from: 0.1.0` (range pin, contradicts the "exact" rule); RightKitDiagnostics only | No | No | No | No | No | associated-domains, aps; no iCloud or keychain group | Everything except Diagnostics |
| **RightSites** (`rightsites/ios/project.yml`) *(outside RightKit scope, listed for completeness)* | Identical to SellRight's file (`diff -q` shows no difference); same bundle ids `app.sellright.ios.admin`. No distinct RightSites iOS app (V) | same | same | same | same | same | same | same |
| **Pulse** | `pulse/mac/Package.swift` and `Notch/project.yml` are macOS; no iOS app target found | n/a | n/a | n/a | n/a | n/a | n/a | No iOS target |
| **SwiftRight** | Only SPM packages, examples and probes; no iOS app project found (platform settings not inspected) | n/a | n/a | n/a | n/a | n/a | n/a | No iOS app |
| **GenRight, CutRight, ViewRight, MailRight, PostRight** | Only `src-tauri/icons/ios` or schema files; no iOS project (N) | n/a | n/a | n/a | n/a | n/a | n/a | No iOS app |

`swift/ios-app-template` (`com.rightsuite.template`) pins `exactVersion: 0.3.0` and links License, Diagnostics, Artifacts, Store, ImportQueue, LiveActivity, WatchBridge and Purchases (`project.yml:13-49`). Its entitlements have iCloud/CloudKit and an App Group only, with no keychain group (`Config/App.entitlements`). It does not link RightKitCloudSync or RightKitAppleCredentials. Its comment at `project.yml:15-17` says tag 0.3.0 exists on the remote (D).

## Contradictions between docs and code

- `swift/README.md` says VERSION is 0.5.0, but `VERSION` is 0.6.0. `docs/PLATFORM-MATRIX.md:139,270` already notes this.
- The XcodeGen example in `swift/RightKitSwift/README.md` shows `exactVersion: "0.3.8"`, and the SwiftPM example shows 0.6.0. Neither is the published 0.4.0.
- `docs/RIGHTKIT-SHARED-PLATFORM.md:94` says HeardRight iOS uses "RightKitSwift 0.3.1". `project.yml` pins 0.4.0. `RIGHTKIT-SHARED-PACKAGES.md:196` still says 0.2.0 staged.
- The code comment at `LicenseController.swift:176` says "published RightKit 0.3.0 client", while the pin is 0.4.0.
- `docs/PLATFORM-MATRIX.md:93` lists scraperight as a rightkit-sync consumer. HeardRight desktop is not one (V), and the `API.md:154` table treats migration as a plan. I did not check the ScrapeRight desktop.
- `swift/RightKitSwift/README.md` says pins are always exact. SellRight uses `from: 0.1.0`.
- The `rightkit-secrets` README implies a RightKitAppleCredentials helper, but no helper binary or target exists in `swift/`. Each app carries its own.
- The Swift README's "Foundation/CryptoKit only" claim in `Package.swift:5-6` sits alongside Core ML, CoreServices and StoreKit products.

## Not found, but needed by an adopter

- A server or worker source for `/v1/licenses/bridge/*` and `/v1/pro/heardright/devices/windows-link/*`. The docs say it is not in the repo.
- Any path for desktop-entered provider API keys to reach iOS. The Mac helper allowlist excludes `api.*`.
- A Windows route to CloudKit: no helper, Web Services client or relay.
- A shared Pro-bridge client and the Mac credentials helper as RightKit products. Both are app-local.
- A published tag you can check locally for 0.5.0 or 0.6.0, and a feed entry above 0.4.0.
- A CloudKit and Pro-bridge adopter guide.
- Recorded Apple-side setup for CodeRight (container, keychain group, products), or for HeardRight (schema deployment, App Store Connect).
- The template does not include CloudSync or AppleCredentials.

## Addendum (2026-10-11): the licence and Pro-bridge server is RightSites

Found by the CodeRight chat, not verified by the RightKit lead: the server is the RightSites repository (`bogusyogi/rightsites`, a fork of SellRight), deployed on the Hetzner host (`docs/rules/ssh-server-access.md` lines 40-74). Endpoints: `packages/api/src/rightsites-plugin/pro-bridge.ts` (`/v1/licenses/bridge/from-activation`, `/from-storekit`, `/exchange`), `rightsites-plugin/pro-devices.ts` (device leases, `/windows-link/request`, `/windows-link/redeem`, `/v1/shop/pro/link-storekit`), `packages/api/src/routes/auth.ts` (`/v1/shop/auth/apple`). The bridge is pinned to HeardRight (`pro-bridge.ts:11` `const APP = 'heardright'`; `pro-devices.ts:185` one StoreKit bundle id), so enabling another app is a code and configuration change there plus a production deploy. RightSites is outside RightKit's scope; the owner handles it. This supersedes the earlier statements that the server source was not in the workspace and the inference that it was SellRight.

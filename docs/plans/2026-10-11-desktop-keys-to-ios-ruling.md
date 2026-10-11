# Desktop-entered provider keys on iOS: design ruling (ledger #102)

Decided by the RightKit lead on 2026-10-11 after a read-only Astra consult (its full answer is below). Requirement from the owner: provider API keys and Pro or licence state entered on desktop appear on the iOS app without manual entry, in every first-party app with both (HeardRight, ScrapeRight, CodeRight).

## What exists today (verified in source, see `2026-10-11-apple-services-parity-findings.md`)

- Pro and licence state crosses: the Mac desktop writes a server-issued bridge token to a synchronizable iCloud Keychain item (`pro.bridge`) through a signed helper; the phone exchanges it for its own lease. Windows uses a 10-minute link code.
- Provider API keys do not cross from desktop to iOS in any app. The Mac helper allowlists two accounts (`pro.bridge`, `sync.consent`).
- StoreKit cannot carry secrets: `appAccountToken` is a UUID that associates purchases.

## Ruling: build order

1. **iCloud Keychain, Mac desktop to iOS (first).** Extend the Mac credentials helper's allowlist to versioned provider accounts in the existing shared access group. No server sees a key; Apple encrypts end to end; it reuses the mechanism the Pro bridge already uses and needs no backend. Covers every Mac desktop user of all three apps.
2. **Paired-device transfer for a Windows desktop (second).** A Windows desktop cannot write iCloud Keychain. Pair the phone once (QR shown on the desktop, identity confirmed on both), then transfer over the local network through `rightkit-send` (certificate-proven peers; its iOS C surface is already planned). No server holds a key.
3. **Encrypted relay (third, only if the licence backend takes it on).** Astra ranks this first because it serves Mac and Windows with one mechanism and works off the local network. It is placed third here because it needs new endpoints on the licence backend, whose source and owner are not in this workspace; nothing else in this plan waits on it.

StoreKit is not used as a transport for credentials.

## Data contract (all transports)

- Keychain service `app.<app>.shared.credentials`, access group `$(AppIdentifierPrefix)app.<app>.shared` (resolved literally at runtime).
- Accounts: existing `pro.bridge`, `sync.consent`; new `api.<provider>.v1` for an explicit allowlist of lowercase provider ids per app.
- Value: `{ v: 1, app, provider, credentialId (UUID), revision (monotonic integer), updatedAt (UTC), state: "active" | "deleted", secret | null }`. A deletion on desktop publishes a tombstone (`state: "deleted"`, `secret: null`) at a higher revision; a recipient erases its copy on receipt and never resurrects an older revision.
- Synchronizable generic-password items with `AfterFirstUnlock` for the Apple transport; non-synchronizable `AfterFirstUnlockThisDeviceOnly` for anything received by pairing and for private pairing keys.
- One authoritative transport per credential; one owning desktop writes each credential.

## Consent and UI requirements

- Sharing provider keys is opt-in and separate from transcript sync and from Pro. The control names the providers and the destination ("your Apple devices" or the paired device by name) and shows pending or applied state.
- Text must say that stopping sync keeps existing copies, that deleting on desktop removes the key from connected devices on their next sync, and that neither invalidates a key already copied: offer the provider's own revocation.
- Revealing or exporting a key and enrolling a device require local authentication.

## Never transferred

Raw licence keys, activation tokens, device leases, licence clocks, database keys, private pairing keys, licensing device ids. StoreKit signed transactions go only to verification endpoints. Keys never enter CloudKit, logs or diagnostics.

## What moves into RightKit

- Bridge HTTP client: `RightKitLicense` (today app-local in HeardRight and ScrapeRight).
- Mac credentials helper: a RightKitSwift target with a signed per-app allowlist, embedded through `right-release`.
- Credential model, revision and tombstone logic: `RightKitAppleCredentials` and a Rust client in `rightkit-secrets`.
- Apps keep identifiers, endpoints, provider choices, entitlements and presentation.

## Tests

Mac to iOS and Windows to iOS in each app: migration of an existing install, consent off and on, offline catch-up, deletion (tombstone applied, no resurrection), rotation, replay of an old revision, a substituted or unknown provider id refused, and negative tests that a key never appears in CloudKit records, logs or diagnostics output.

## Owner-side setup per app (Apple developer account)

App ID with Keychain Sharing (group `app.<app>.shared`), iCloud with the CloudKit container `iCloud.app.<app>.ios.native`, Push Notifications, Sign in with Apple, App Groups; provisioning profiles for the app and for the Mac helper; App Store Connect products `app.<app>.pro.*`. Not recorded as done for CodeRight.

---

## Astra consult, verbatim

**Build an end-to-end encrypted, paired-device relay first for HeardRight, ScrapeRight & CodeRight.** It meets Mac **& Windows → iOS** requirements without re-entering keys. Retain Pro’s separate server-issued lease flow. Add native iCloud Keychain transport second; LAN transfer third. Never use StoreKit as secret transport.

Verified: Mac publishes `pro.bridge` through Keychain; its helper accepts only `pro.bridge` & `sync.consent`. Desktop API keys remain device-local. StoreKit purchases can establish entitlement, but do not transport those keys. [Bridge publisher](/Volumes/D/claude/heardright/tauri-app-next/src-tauri/src/pro_bridge.rs:385), [helper allowlist](/Volumes/D/claude/heardright/tauri-app-next/macos-native/HeardRightPillMac/Sources/HeardRightAppleCredentials/main.swift:147), [desktop store](/Volumes/D/claude/heardright/tauri-app-next/src-tauri/src/secret_store.rs:1).

| Mechanism | Security, compromise & operation | Cost / review assessment |
|---|---|---|
| **Apple Keychain** | Same Apple Account plus per-app access group. Apple encrypts end-to-end; RightKit servers receive nothing. Compromised relay/licence server gains no provider keys. Trusted-device/account compromise remains relevant. Updates/deletions propagate eventually; cached keys work offline. Requires explicit Apple-sync consent. | Lowest cost; extend helper allowlist. Low transport-specific review risk. Apple-only. [Apple security](https://support.apple.com/en-gb/guide/security/sec1c89c6f3b/web) |
| **Encrypted relay** | Encrypt separately to enrolled device public keys. Server compromise exposes ciphertext, routing metadata & timing; permits withholding/replay attempts, not decryption **if device identities are independently authenticated**. Queue changes until recipients reconnect. Explicit device pairing & provider selection. | Highest cost: protocol, backend, recovery & security review. No inherent StoreKit dependency; disclose relay retention/privacy. |
| **LAN / QR pairing** | Pinned certificates resist network interception; certificates prove possession, not ownership. QR/fingerprint verification establishes trusted recipient. No server-held secrets. Both devices must reconnect for rotation/deletion; works without internet. Consent names recipient & providers. QR carries pairing material, never API keys. | Medium–high: iOS integration remains needed; `rightkit-send` currently lacks C ABI/xcframework. Local-network permission & clear review explanation required. [Status](/Volumes/D/claude/rightkit/crates/rightkit-send/README.md:1) |
| **StoreKit** | **Cannot carry arbitrary secrets.** `appAccountToken` is purchase-account association via UUID, not secret storage or authentication. Transactions support entitlement verification/restoration; a UUID could reference backend state, but backend transfer remains separate. Key consent, rotation & deletion are unsupported. | Reject for credentials. StoreKit remains appropriate for purchases. [Apple API](https://developer.apple.com/documentation/storekit/product/purchaseoption/appaccounttoken(_:)) |

For all three real transports, stolen unlocked devices can use accessible keys; privileged compromise can extract them. Require local authentication for revealing/exporting keys & enrolling devices. Desktop deletion must publish persistent, versioned tombstones, erase recipient caches on receipt & prevent stale resurrection. Rotation publishes newer revisions. Disabling sync only stops participation. Neither action invalidates copied provider credentials: offer provider-side revocation. Offline recipients retain old copies until reconnecting.

**Windows:** pair iPhone once by scanning desktop QR & confirming device identity. Use authenticated encryption with independently pinned recipient keys; bridge tokens authorize enrollment but cannot authenticate public keys against a malicious server. Short-code fallback requires authenticated pairing, expiry & attempt limits. Windows subsequently relays encrypted changes automatically. Generalize licence pairing to let an activated Windows desktop sponsor iOS; existing [Windows-link flow](/Volumes/D/claude/heardright/tauri-app-next/src-tauri/src/license/lease.rs:649) runs in reverse. Backend implementation requires work outside this workspace’s [client-only contract](/Volumes/D/claude/heardright/docs/PRO_BRIDGE_BACKEND_CONTRACT.md:3).

**Data contract:** service `app.<app>.shared.credentials`; access group `$(AppIdentifierPrefix)app.<app>.shared`, resolved literally at runtime. Accounts: existing `pro.bridge`; new `api.<provider>.v1` for explicit allowlisted lowercase provider IDs. Versioned naming prevents legacy readers treating JSON as API keys.

Each credential value contains `{v:1, app, provider, credentialId, authorityPublicKey, revision, updatedAt, state, secret}`; UUID credential ID, monotonically increasing integer revision, UTC timestamp, `active|deleted`, null secret for tombstones. One owning desktop writes each credential; ownership changes require explicit handoff. Relay envelopes bind app, recipient, credential ID & revision cryptographically; keep metadata inside ciphertext where possible.

Use generic-password items, data-protection Keychain, strict scope queries: synchronizable plus `AfterFirstUnlock` for Apple transport; non-synchronizable plus `AfterFirstUnlockThisDeviceOnly` for relay recipients/private keys. These match [shared storage primitives](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitAppleCredentials/RightKitAppleCredentials.swift:215). Select one authoritative transport per credential.

UI must say: “Share selected provider keys with your paired devices”; “Our server stores encrypted copies”; “Delete from connected devices”; “Stopping sync keeps existing copies.” Separate credential, transcript & Pro consent; show pending/applied status. Never transfer raw licence keys, activation tokens, device leases, licence clocks, database keys, private pairing keys or licensing device IDs between devices. StoreKit JWS goes only to verification endpoints.

Move bridge HTTP clients into **RightKitLicense**, helper into **RightKit Swift** with signed per-app allowlists, credential orchestration into **RightKitAppleCredentials**, Rust transport/helper client into **rightkit-secrets**. Apps retain identifiers, endpoints, provider choices, entitlements & presentation.

Test all three apps across Mac/Windows→iOS: migration, consent, offline catch-up, deletion, rotation, replay, substituted public keys, wrong app/group, revoked devices & lease revocation. Canary secrets must never appear in app CloudKit records, logs, diagnostics, crashes or plaintext relay storage.

Owner checklist, per app:

- Register explicit iOS/Mac-helper App IDs under matching team; enable Keychain Sharing & provision matching access groups.
- Provision/sign helper & app with authorized profiles. [Apple requirements](https://developer.apple.com/documentation/technotes/tn3137-on-mac-keychains)
- **No CloudKit container needed for credentials.** For document/transcript sync, associate `iCloud.app.<app>.ios.native`, enable CloudKit/push & deploy production schema.
- Configure equivalent Pro IAP products, restore flow & review notes; keep BYOK separate from paid-feature unlocking. [Review rules](https://developer.apple.com/app-store/review/guidelines/#other-purchase-methods)

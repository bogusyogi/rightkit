# RightKit send for iOS: Swift-native `RightKitSend` (design ruling)

Date: 11 October 2026. Status: design ruling by Sage, not yet reviewed by Pulse (protocol owner) or ScrapeRight (first consumer). Nothing here is implemented. Supersedes the C-surface section of `2026-10-10-rightkit-send-design.md` if Gate 0 passes.

Path shorthand: `P/` = `/Volumes/D/claude/pulse/core/src/localsend/`, `C/` = `/Volumes/D/claude/rightkit/crates/rightkit-send/src/`.

## 1. Decision: (a), a Swift-native `RightKitSend`, behind a spike gate

Build `RightKitSend` in `/Volumes/D/claude/rightkit/swift/RightKitSwift` on Network.framework, Security and CryptoKit. The Rust crate stays the desktop engine. A shared fixture plus a live cross-engine job keeps the two identical. If the Gate 0 spike (section 7) fails, fall back to (b) behind the same Swift API.

Reasons, each from inspected evidence:

- **(b) does not exist yet and still needs Swift.** The crate has no C surface, no xcframework and no `suspend`/`resume` (`rightkit-send/README.md:7-8,58-59`). Bonjour on iOS is assigned to Swift either way (`README.md:39-41`). So (b) is engine + new FFI + Swift shell, not "one implementation".
- **RightKitSwift is source-only.** It has no binary target and states "Foundation/CryptoKit only: no third-party dependency" (`Package.swift:6,10-73`). Apps pin an exact git tag (`/Volumes/D/claude/scraperight/ios-native/project.yml:19-22`) and the mirror is private (ledger #63, line 69). (b) adds the package's first binary artifact and a cargo cross-compile lane.
- **Runtime shape.** The crate polls: a 50 ms non-blocking accept loop (`C/lib.rs:1119-1121`) and a 100 ms liveness tick (`C/lib.rs:1237-1242`). Its runtime budget is all `null` (`runtime-budget.json:4-13`). Network.framework is event-driven and one `NWListener` owns both the port and the Bonjour record.
- **Platform crypto only.** ScrapeRight declares `ITSAppUsesNonExemptEncryption: false` (`project.yml:47`); (a) keeps that accurate without reviewing a bundled rustls/ring stack (`Cargo.toml:25`). The export-compliance and binary-size cost of (b) is unverified; I did not measure it.
- **The phone's rule set is a strict subset.** No multicast (`C/lib.rs:12`), no callback tier (`/Volumes/D/claude/rightkit/docs/plans/2026-10-10-rightkit-send-design.md:29`), an app-owned trust set, five routes (`C/receive.rs:89-106`).
- **The agreed identity split survives.** `SecIdentityCreate(allocator, certificate, privateKey)` is public on iOS 11.2+ (`SecIdentity.h:57-70` in the installed iPhoneOS SDK), so the library can hand over bytes and the app can store them (handoff `:48`).

Things that cut the other way, or that you should know:

- **Client certificates become mandatory on the phone.** `sec_protocol_options_set_peer_authentication_optional` is `API_UNAVAILABLE(macos, ios, …)` (`SecProtocolOptions.h:593-595`); only `…_required` exists (`:573-575`). A sender with no certificate fails the handshake instead of being asked. Every first-party engine presents one (`C/net.rs:290-302`, `C/lib.rs:703`, `P/mod.rs:489`), and LocalSend compatibility on the phone is out of scope (handoff `:70`). This is stricter than handoff `:65`; Pulse owns the trust rules and should acknowledge it.
- **The Pulse-Mac premise does not add weight.** The Mac notch is Swift, but the engine runs in the Tauri hub (`/Volumes/D/claude/pulse/docs/decisions.md:6`, `/Volumes/D/claude/pulse/hub/src-tauri/src/share.rs:39`). The Swift product's consumers are the iOS apps; macOS support is for tests.
- **This reverses a three-chat agreement.** The design doc (`:23-30`), ledger #14 (line 20) and handoff (`:40,54`) chose the C surface. What Adrian approved still holds: RightKit owns the crate and the iPhone packaging, and Pulse moves onto the crate (`decisions.md:31`). The Pulse-owned wording needs an update by the Pulse chat.
- **Two knock-on effects.** `rightkit-automation`'s Swift surface was to follow the send C-ABI pattern (ledger line 69) and now needs its own ruling. The Windows-to-iOS key transfer plan (`/Volumes/D/claude/rightkit/docs/plans/2026-10-11-desktop-keys-to-ios-ruling.md:14`) will run on this Swift receiver, so it needs an Oracle security review before shipping.
- **Row 120 does not say what the brief attributes to it.** Ledger line 126 records only the three drift items. The trust-list question appears in row 135 item 7 (line 141) and design doc `:29`. In the Rust crate the trust store stays in-crate by the parity requirement (design doc `:21`).

### Pulse engine vs crate: behavioural differences

I compared by reading both sides in full, not with a diff tool. `proto.rs` and `discovery.rs` read identical; the crate compiles `discovery` only under `multicast` (`C/lib.rs:12-13`).

| # | Pulse | Crate | Difference |
|---|---|---|---|
| 1 | `P/mod.rs:762-773`, used at `share.rs:754-759` | absent; only `respond` at `C/lib.rs:1035-1041` | `respond_and_allow` missing. Pulse ahead. |
| 2 | `/Volumes/D/claude/pulse/core/src/send_cmd.rs:95-113,140-156,392-444` | `C/net.rs:90,108,194`, `C/send.rs:80,210` | CLI hands sends to the hub, or sends alone under the stored identity. Host code; the crate's public pieces suffice. |
| 3 | `P/net.rs:586` | `C/net.rs:106-119,633-636`, set only at `C/lib.rs:704` | User-Agent is literal "Pulse" vs process-global. A hubless sender on the crate sends an empty one unless it calls `set_user_agent`. |
| 4 | none in `P/` (only comments at `mod.rs:151`, `receive.rs:234`) | none | "Held/queued receipts" are Chat states (`decisions.md:14,39`), not the send engine. Nothing to port. |
| 5 | `P/receive.rs:482-491,632-638` | `C/receive.rs:493-512,659-696` | F01: Pulse truncates a deterministic `.name.pulse-part`; the crate reserves with `create_new`. Crate ahead. |
| 6 | `P/mod.rs:458-463`, `P/net.rs:52-84` | `C/lib.rs:657-664`, `C/net.rs:70-102` | Identity loaded from `state_dir` and silently regenerated when unusable, vs host-supplied and a typed error. |
| 7 | `P/mod.rs:99,127,443-450` | `C/error.rs:8-89`, `C/lib.rs:132,162` | English strings vs typed `Error`. The CLI reads `t["error"].as_str()` (`send_cmd.rs:508-512`), which breaks on a typed object. |
| 8 | `P/send.rs:167` | `C/net.rs:123-132`, `C/error.rs:144-156` | Pin mismatch is its own error in the crate. |
| 9 | `P/receive.rs:24-32,235-240,353` | `C/policy.rs:23-33`, `C/receive.rs:240-248,361` | Constants vs `Policy`; same defaults. The 413 body text differs. |
| 10 | `P/mod.rs:30-39,495` | `C/lib.rs:44-69,710` | Config: `device_type` hardcoded "desktop" vs configurable; plus product name, quarantine agent, temp suffix, log prefix, `trust_path`. |
| 11 | `P/mod.rs:469-475,497,524` | `C/lib.rs:670,689-701,712,744,763-766` | Port 0 and `bound_port` exist only in the crate. |
| 12 | `P/mod.rs:523-532,565-571` | `C/lib.rs:742-752,842-851,1395-1398` | Multicast always on vs feature-gated. With it off, `refresh` is a no-op and own-IP filtering is loopback-only. |
| 13 | none (`_pulse-send` appears only in the handoff) | `C/bonjour.rs`, `C/lib.rs:82-84,772-794,1467-1499` | Bonjour, `px`, and host-fed peers exist only in the crate, macOS only. |
| 14 | `P/mod.rs:180-184`, `/Volumes/D/claude/pulse/core/src/atomic.rs:55-75` | `C/lib.rs:213-265` | Trust-store writes via `rightkit-fs` with a stale-temp sweep vs a private writer (audit A03). |
| 15 | `P/net.rs:30` | `C/net.rs:28-33` | SHA-256 from `rightkit_fs` vs in-crate `sha2`. |
| 16 | `P/trust.rs:29,182-191,219` | `C/trust.rs:183-196,224` | Trust file fixed under `state_dir` vs caller path. |
| 17 | none | `C/lib.rs:624-644,797-810` | `SendStatus` and `status()` exist only in the crate. |
| 18 | 2 tests (`/Volumes/D/claude/pulse/core/tests/localsend.rs:87,117`) | 16 (`rightkit-send/tests/localsend.rs:140-602`) | The parity suite lives in the crate. |

## 2. Public Swift API

Product `RightKitSend`, iOS 17 / macOS 14, SDK frameworks only.

```swift
public enum SendDeviceType: String, Sendable, Codable, CaseIterable { case desktop, mobile, web, headless, server }

public struct SendIdentity: Sendable, Equatable {
    public let certificateDER: Data          // X.509 v3, P-256, self-signed
    public let privateKeyPKCS8DER: Data      // same encoding as the crate's Identity::key_der
    public let fingerprint: String           // 64 lowercase hex, SHA-256 of certificateDER
    public init(certificateDER: Data, privateKeyPKCS8DER: Data) throws(SendError)
    public init(storageBlob: Data) throws(SendError)
    public var storageBlob: Data { get }
    public static func generate(commonName: String) throws(SendError) -> SendIdentity
    public static func loadOrCreate(commonName: String,
                                    load: () throws -> Data?,
                                    save: (Data) throws -> Void) throws(SendError) -> SendIdentity
}

public struct SendPolicy: Sendable, Equatable {
    public var unverifiedFolderName: String  // "Received (unverified)"
    public var unprovenMaxFiles: Int         // 200
    public var unprovenMaxBytes: UInt64      // 2 GiB
    public var maxFileBytes: UInt64          // 64 GiB
    public var maxMessageCharacters: Int     // 65_536
    public var answerWait: Duration          // 60 s
    public init()
}

public struct SendConfiguration: Sendable {
    public var alias: String
    public var deviceType: SendDeviceType
    public var deviceModel: String?
    public var port: UInt16                  // 53317; 0 = system-chosen
    public var receiveDirectory: URL
    public var userAgent: String
    public var temporarySuffix: String
    public var policy: SendPolicy
    public init(alias: String, deviceType: SendDeviceType, deviceModel: String? = nil,
                port: UInt16 = 53317, receiveDirectory: URL, userAgent: String,
                temporarySuffix: String = ".rightkit-part", policy: SendPolicy = SendPolicy())
}

public struct SendPeer: Identifiable, Hashable, Sendable {
    public let fingerprint: String
    public let alias: String
    public let deviceType: SendDeviceType
    public let deviceModel: String?
    public let host: String
    public let port: UInt16
    public let trusted: Bool
    public var id: String { fingerprint }
}

public struct SendProgress: Sendable, Equatable {
    public enum Phase: Sendable { case waitingForAcceptance, sending }
    public let phase: Phase
    public let bytesSent: UInt64
    public let bytesTotal: UInt64
    public let filesSent: Int
    public let filesTotal: Int
    public let currentFileName: String?
    public var fraction: Double { get }
}

public struct ReceivedFile: Hashable, Sendable {
    public let url: URL; public let name: String; public let size: UInt64; public let mimeType: String
}

public struct ReceivedItem: Identifiable, Hashable, Sendable {
    public enum Content: Hashable, Sendable { case text(String), link(URL), file(ReceivedFile), picture(ReceivedFile) }
    public let id: UUID
    public let senderAlias: String
    public let senderFingerprint: String
    public let content: Content
    public let receivedAt: Date
    public let fromTrustedProvenSender: Bool
    public let clipboardRequested: Bool
}

public struct IncomingOffer: Identifiable, Hashable, Sendable {
    public let id: UUID
    public let senderAlias: String
    public let senderFingerprint: String
    public let certificateProven: Bool
    public let trusted: Bool
    public let isMessage: Bool
    public let fileCount: Int
    public let totalBytes: UInt64
    public let expiresAt: Date
}
public enum IncomingOfferEvent: Sendable { case offered(IncomingOffer), withdrawn(UUID) }

public enum LocalNetworkAccess: Sendable { case unknown, granted, denied }
public enum SendServiceState: Sendable, Equatable {
    case stopped, running(port: UInt16, localNetwork: LocalNetworkAccess), suspended, failed(SendError)
}

public enum SendError: Error, Sendable, Equatable {
    case notRunning, peerNotFound, notTrusted(fingerprint: String), refused, timedOut, cancelled
    case network(String), identityMismatch, peerBusy, tooLarge, peerRequiresPIN, peerAnswered(status: Int)
    case nothingToSend, unreadable(path: String), tooManyFiles(max: Int)
    case identityUnusable(String), identityStorage(String)
    case portInUse(UInt16), listenerFailed(String), localNetworkDenied
    public var code: String { get }          // stable snake_case, pinned by the fixture
}

public actor SendService {
    public init(configuration: SendConfiguration, identity: SendIdentity)
    public nonisolated let fingerprint: String
    public func start() async throws(SendError)
    public func stop() async
    public func suspend() async
    public func resume() async throws(SendError)
    public nonisolated func states() -> AsyncStream<SendServiceState>
    public nonisolated func peers() -> AsyncStream<[SendPeer]>
    public nonisolated func receivedItems() -> AsyncStream<ReceivedItem>
    public nonisolated func offerEvents() -> AsyncStream<IncomingOfferEvent>
    public func accept(_ offerID: UUID) async -> Bool
    public func decline(_ offerID: UUID) async -> Bool
    public func setTrustedFingerprints(_ fingerprints: Set<String>) async
    public func sendText(_ text: String, to peer: SendPeer, clipboard: Bool,
                         progress: (@Sendable (SendProgress) -> Void)? = nil) async throws(SendError)
    public func sendFiles(_ urls: [URL], to peer: SendPeer, clipboard: Bool,
                          progress: (@Sendable (SendProgress) -> Void)? = nil) async throws(SendError)
}
```

Contracts the signatures do not show:

- **Identity.** `loadOrCreate` generates and saves only when `load` returns nil. A stored blob that cannot be used throws `identityUnusable`; it never regenerates. The app stores the blob device-only and non-synchronizable; `RightKitAppleCredentials`' `write(_:service:account:scope:)` fits (`RightKitAppleCredentials.swift:32-37`).
- **Lifecycle.** `start` opens the TLS listener and publishes `_pulse-send._tcp` on it with TXT `fp`, `alias` (80 chars max), `v=2.2`, `type`, `model` when set, and `px=1` (`C/bonjour.rs:182-199`). `suspend` withdraws the advert, declines pending offers with 403, cancels transfers and removes their partials, and emits `[]` on `peers()`. `resume` rebuilds with the same identity and port.
- **Peers and proof.** A browse result is listed only if TXT `fp` is 64 hex, `px=1`, it is not this device, and a TLS handshake pinned to that `fp` succeeded on `POST /register`. So every listed peer has proven its key; `host` and `port` come from that connection. The register call also puts the phone on the computer's device list (`C/receive.rs:119-135`).
- **Offers.** `offerEvents()` never emits an auto-accepted offer. `IncomingOffer` carries no text, preview or file names. `withdrawn` fires on timeout, sender cancel, `suspend` or `stop`.
- **Items.** `link` means the trimmed text is a single http(s) URL with a host. `picture` means the sanitized name maps to an `image/*` type other than SVG in the `mime_for` table (`C/proto.rs:219-242`).
- **Send.** Text goes as one `text/plain` file with the text in `preview`; 204 means delivered (`C/send.rs:84-94,250-253,287`). Cancelling the awaiting `Task` cancels the transfer, throws `.cancelled`, and sends a best-effort `POST /cancel`. Progress is called at most ten times a second.
- **Trust-list ownership.** The app owns the trusted-device list: its persistence, its UI and the "don't ask again" choice. `RightKitSend` holds only the last set passed to `setTrustedFingerprints`, ignores entries that are not 64 hex, and applies it only to certificate-proven senders and as the gate for sending.
- **Naming.** ScrapeRight's stub declares `SendPeer`, `ReceivedItem` and `IncomingOffer` with the same names (`SendEngine.swift:10,23,48`). It should delete its copies or qualify the imports; its `SendEngine` seam stays as an adapter.

## 3. Protocol contract: one shared fixture

- **Location.** Canonical file at `/Volumes/D/claude/rightkit/crates/rightkit-send/test-vectors/send-conformance-v1.json`. Byte-identical copies at `/Volumes/D/claude/rightkit/swift/contracts/send-conformance-v1.json` and `/Volumes/D/claude/rightkit/swift/RightKitSwift/Tests/RightKitSendTests/Contracts/send-conformance-v1.json`. Extend `/Volumes/D/claude/rightkit/swift/contracts/contracts.test.mjs` with the equality assertions, as it does for the license vector (`:9-30`).
- **Who generates.** The Rust side, because it is the deployed reference and Pulse holds protocol authority (handoff `:10,54`). A golden test `tests/conformance_vectors.rs` regenerates derived values from the engine and fails on any difference; an environment switch rewrites the file. Swift only consumes.
- **Versioning.** New vectors are a minor bump. A changed expectation is a major bump and needs Pulse's sign-off.

Sections of the fixture:

| Section | Contents |
|---|---|
| `meta` | API prefix `/api/localsend/v2`, version "2.0", port 53317, service type, TXT `v` "2.2" (`C/proto.rs:13-16`, `C/bonjour.rs:25,192`) |
| `identities` | Three test identities (cert DER, PKCS#8 key, fingerprint): receiver, sender, impostor |
| `fingerprint` | Normalize and 64-hex validity cases (`C/trust.rs:40-42`, `C/net.rs:24-26`) |
| `txt` | Encode inputs with the expected decoded map, compared as a map since order is not guaranteed in Swift; decode byte cases (`C/bonjour.rs:808-875`) |
| `wire` | JSON for `/info`, `/register`, `prepare-upload` (text, files, `pulse.clipboard`) and its response, with defaults and `isMessage` (`C/proto.rs:18-100`) |
| `sanitize`, `uniqueNames` | File name in, components out; `name (1).ext` and `.name.N<suffix>` sequences (`C/proto.rs:106-217`, `C/receive.rs:659-669`). Windows device names tagged windows-only (`C/proto.rs:121-124`) |
| `http` | Raw request bytes with the expected parse or rejection; line, header and body caps (`C/net.rs:475-481,516-578`, `C/receive.rs:125,147,516`) |
| `receiveScenarios` | One per rule ID in section 5: sender identity, request, user answer, expected status, offer shape, saved paths with hashes, clipboard flag |
| `sendScenarios` | Scripted peer answers mapped to outcome or error code (`C/send.rs:281-315,424-438`), plus pin mismatch |
| `errorCodes`, `policyDefaults` | The shared code table (`C/error.rs:8-89`) and limits (`C/policy.rs:23-33`) |

Each scenario carries an `engines` tag. Two deltas are deliberate: a sender with no certificate (Rust asks, Swift refuses at handshake), and chunked uploads (Rust accepts, `C/net.rs:525-555`; Swift answers 400 because first-party senders always send `Content-Length`, `C/net.rs:635`).

## 4. Rust crate: changes before publishing, and the decision callback

**Decision callback API.** It already exists as event-out, answer-in; one method is missing.

```rust
pub fn start(config: Config, identity: Identity,
             on_event: Arc<dyn Fn(Event) + Send + Sync>) -> Result<Service, Error>;  // C/lib.rs:657
// Event::Incoming(Incoming) asks; Incoming carries fingerprint and verified, never text. C/lib.rs:93-112,170-183
pub fn respond(&self, id: &str, accept: bool) -> bool;                               // C/lib.rs:1035
pub fn respond_and_allow(&self, id: &str) -> bool;                                   // NEW, port of P/mod.rs:762-773
```

`respond` may be called from inside the `Event::Incoming` callback: no lock is held across `emit` and the wait re-checks its predicate (`C/receive.rs:305-316`). Pin that with a test. `respond_and_allow` sets Allow only for the fingerprint on that pending request, returns false and changes nothing for an unknown id, and Allow still needs proof on every request (`C/receive.rs:257`).

Required changes:

1. Add `respond_and_allow` with three tests: only that fingerprint is promoted; unknown id is a no-op; a later uncertified request still asks.
2. Add the re-entrancy test above.
3. Hubless sender: no new API. Add a test that `set_client_identity` + `set_user_agent` + `send::deliver` is seen as `verified` by a receiving `Service`, and give `user_agent()` a non-empty default.
4. Fix `rust-version`. The manifest says 1.90 (`Cargo.toml:5`) but depends on `rightkit-process`, which requires 1.98.0 (`rightkit-process/Cargo.toml:5`). Pulse is on 1.98.0 (`/Volumes/D/claude/pulse/Cargo.toml:8`). Set 1.98 and delete the stale comment at `C/lib.rs:219-220`.
5. A03: delegate `write_atomic` to `rightkit-fs` as Pulse does (`atomic.rs:55-75`), after comparing parent creation, 0600, temp cleanup and symlink handling. Keep the six tests at `C/lib.rs:267-356` as acceptance.
6. F01: the working tree already has the fix and four tests (`tests/localsend.rs:448-602`); the audit cited the pre-fix line. I could not check whether it is committed. Confirm, then close F01 in ledger #97.
7. Runtime budget: declare ceilings and `requiredScale`, point `status` at `Service::status` (`C/lib.rs:797`), measure on macOS and Windows (`RUNTIME-BUDGETS.md:21-24,115,133`). Replace the accept and liveness polling first, or a wake-up ceiling will be hard to meet.
8. Add the fixture, its golden test, a scenario runner and `examples/conformance_peer.rs`; add `test-vectors/**` to `include` (`Cargo.toml:13`).
9. Update the README (`:7-8,39-41,58-59`) and mark the design doc's C-surface section superseded.

Not publish blockers, but on the path to the ScrapeRight outcome:

- **Windows DNS-SD.** The design doc requires it (`:11`) and it is not built (`README.md:37`). Without it the phone cannot discover the Dell. The phone's `/register` makes the Dell list the phone, but with `px: false` (`C/lib.rs:485`).
- **Pulse adoption.** No Pulse build advertises Bonjour today, so the phone finds no computer until Pulse re-pins onto the crate with `bonjour` enabled.
- **Catalog.** `device-send` goes through Adrian-authenticated trust-sync (ledger line 149).

## 5. Security requirements as testable rules

Each ID is one `receiveScenarios` or `sendScenarios` entry and one named test per engine.

| ID | Rule | Source |
|---|---|---|
| S-01 | Fingerprint is the lowercase hex SHA-256 of the certificate DER; all comparisons normalize first. | `C/net.rs:28-33`, `C/trust.rs:40-42` |
| S-02 | Every outbound connection completes TLS only if the server's leaf certificate matches the peer's 64-hex fingerprint. A mismatch yields `peer_identity_mismatch` with zero request bytes sent. No plain HTTP; a malformed fingerprint is refused before connecting. Swift has no unpinned mode. | `C/net.rs:228-250`, `C/send.rs:219-229` |
| S-03 | Every outbound connection presents the device certificate. | `C/net.rs:290-302` |
| S-04 | A sender is certificate-proven only if the certificate presented in that handshake hashes to the fingerprint the request claims. TXT, alias, IP and earlier sessions never count. | `C/receive.rs:171-172` |
| S-05 | Swift only: the listener requires a client certificate. A peer with none reaches no route handler. | `SecProtocolOptions.h:573-595` |
| S-06 | Auto-accept only when trusted and certificate-proven. Otherwise an offer is raised. No callback tier in Swift. | `C/receive.rs:257,297-321` |
| S-07 | Before accept, nothing a sender wrote is exposed: no text, no preview, and in Swift no file names. The alias is stripped of control characters and cut to 80. | `C/receive.rs:218-224,278`, `tests/localsend.rs:252-275` |
| S-08 | Before accept, no byte is written and no upload token exists. | `C/receive.rs:368-381` |
| S-09 | Decline, a 60 s timeout, `stop` and `suspend` all answer 403. | `C/receive.rs:307-325`, `C/lib.rs:1068-1072` |
| S-10 | Zero files, more than 20,000 files, a file over 64 GiB, or a size sum that overflows answers 400 before any offer. | `C/receive.rs:153-164` |
| S-11 | An unproven sender naming more than 200 files or 2 GiB gets 413 before any offer. | `C/receive.rs:240-248` |
| S-12 | One session or pending offer at a time; others get 409. A session idle for 120 s is reaped. | `C/receive.rs:194-214` |
| S-13 | An upload needs the session id, file id, a constant-time token match and the same remote IP as the prepare call; otherwise 403. Tokens are 16 CSPRNG bytes. | `C/receive.rs:410-475` |
| S-14 | A body over the declared size plus 1 MiB aborts; the transfer's own partial and reservation are removed. | `C/receive.rs:516-555` |
| S-15 | Every written path is the receive root plus sanitized components: at most 8, no `..`, `.`, separators or control characters, 200 bytes each. The result must be inside the root. Swift re-checks containment after resolving the parent's symlinks. | `C/proto.rs:106-170,201-206` |
| S-16 | The final name is reserved with exclusive create; a collision becomes `name (1).ext`. Nothing is overwritten. | `C/proto.rs:194-212` |
| S-17 | F01: bytes go to an exclusively created partial. The transfer writes, renames and deletes only files it created. A pre-existing partial is never adopted, truncated or removed. | `C/receive.rs:493-512,659-696` |
| S-18 | Files are 0600 while partial and 0644 when whole, never executable. macOS adds the quarantine mark; iOS adds none. If marking fails for an unproven sender, the file is deleted. | `C/receive.rs:585-655` |
| S-19 | When the sender declared a SHA-256, a mismatch answers 422 and removes the files. | `C/receive.rs:556-570` |
| S-20 | The clipboard flag is true only for a trusted, certificate-proven sender whose request set `pulse.clipboard`. | `C/receive.rs:343-346` |
| S-21 | Files from an unproven sender go under the unverified folder. | `C/receive.rs:258-263` |
| S-22 | Swift only: sending to a fingerprint not in the trusted set throws `notTrusted` and opens no connection. | row 135 item 5 |
| S-23 | Swift only: an unusable stored identity is a typed error, never a silent new fingerprint. No log line contains message text, tokens, key bytes or names from an unaccepted offer. | `C/net.rs:94-102` vs `P/net.rs:69-83` |

What a hostile sender can and cannot do on the phone:

- **No certificate:** a failed handshake only, bounded by a 64-connection cap and 30 s timeouts (`C/lib.rs:1108`, `C/receive.rs:54-55`).
- **Wrong certificate for the claimed fingerprint, or proven but untrusted:** one pending offer at a time, showing a sanitized alias, a count and a byte total. It can hold the slot for up to 60 s, during which a trusted sender gets 409. That nuisance is inherited from Rust and not fixed here.
- **Never:** a disk write before accept, auto-accept, the clipboard flag, a path outside the root, an overwrite, or more than the unproven caps.

## 6. Test plan

Commands for you to run; I ran none. Cargo goes through `rightkit cargo` per `/Volumes/D/claude/docs/rules/rightkit.md`.

- Crate: `rightkit cargo test -p rightkit-send --all-features`
- Swift: `swift test --package-path /Volumes/D/claude/rightkit/swift/RightKitSwift --filter RightKitSendTests`
- Fixture equality: `node --test /Volumes/D/claude/rightkit/swift/contracts/contracts.test.mjs`
- Cross-engine: `rightkit cargo build -p rightkit-send --example conformance_peer`, then the Swift command with `RIGHTKIT_SEND_RUST_PEER=<built binary>` and `--filter RightKitSendInteropTests`

Without a second device, on one Mac:

1. **Pure units, fixture-driven:** JSON, HTTP parsing, TXT, sanitize, naming, error codes, the certificate builder.
2. **Swift loopback pair:** two `SendService` instances on 127.0.0.1 with in-memory fixture identities and an injected peer list. Runs every S-rule: send text and files, progress, Task cancel, accept, decline, timeout, suspend during a pending offer, and the F01 cases.
3. **Swift against Rust on loopback.** The peer example prints `READY port=… fingerprint=…`; Swift tests skip when the variable is unset. It uses `send::deliver` to send, because `Service::send` drops loopback devices (`C/lib.rs:448-453`).
   - Swift to Rust: Rust reports `verified: true`; an Allow record auto-accepts with clipboard.
   - Rust to Swift: the offer shows `certificateProven`; a trusted sender is auto-accepted.
   - A Swift-generated identity passes `Identity::load` and a pinned handshake.
   - The impostor identity yields `identityMismatch` in both directions.
4. **Real Bonjour on macOS,** opt-in by `RIGHTKIT_SEND_BONJOUR_TESTS=1`, since it touches mDNSResponder and may prompt for Local Network access.

Needs two physical devices:

- the iOS Local Network prompt and its denial state;
- mDNS across real Wi-Fi, including access-point isolation;
- backgrounding mid-transfer, then resume;
- the identity blob surviving relaunch and not reappearing on a restored device;
- interop with the installed Pulse builds on the Mac and the Dell;
- large-file throughput.

## 7. Implementation plan (Sonnet lanes; security and spec-bound throughout)

**Gate 0, one spike lane on macOS loopback against the crate.** Prove all five, or stop and fall back to (b):

- a `SecIdentityCreate` identity from in-memory key and certificate works as the Network.framework local identity;
- a Swift client with that identity and a pinning verify block is seen as `verified` by the rustls server;
- an `NWListener` with required peer authentication and an accept-any verify block takes the rustls client, and the presented fingerprint is readable (`SecProtocolMetadata.h:204-206`);
- a hand-built X.509 v3 P-256 certificate is accepted by webpki (`C/net.rs:224-226`);
- a client with no certificate fails the handshake cleanly.

Then, in dependency order:

| Step | Lane | Owns | Needs |
|---|---|---|---|
| 1 (parallel with Gate 0) | Rust parity | Section 4 items 1-6, 8, 9 | nothing |
| 2 | API freeze | `Package.swift` product and targets, `API.swift` with section 2 verbatim, the fixture copy | Gate 0 |
| 3a | Codec | `Wire.swift`, `HTTPCodec.swift`, `FileNames.swift`, `TXTRecord.swift` and fixture unit tests | 2 |
| 3b | Identity and TLS | `Identity.swift`, `CertificateDER.swift`, `TLS.swift` | 2 |
| 4a | Receiver | `Receiver.swift`: listener, routes, offers, policy, file writes; S-04 to S-21 | 3a, 3b |
| 4b | Sender | `Sender.swift`: register, prepare, upload, cancel, progress; S-02, S-03, S-22 | 3a, 3b |
| 5 | Service | `Discovery.swift`, `SendService.swift`: browse, advert, peer registry, lifecycle, streams | 4a, 4b |
| 6 | Conformance | Swift scenario runner, interop tests, `contracts.test.mjs`, README, `VERSION` | 1, 5 |
| 7 | Review and publish | Oracle review of the receiver; `right-release publish swift`; `PLATFORM-MATRIX` and ledger rows 14, 120, 135 | 6 |
| 8 | ScrapeRight chat | Adapter conforming to `SendEngine`, identity blob storage, trusted-list store and UI, new pin | 7 |
| 9 (separate) | Crate release | Runtime budget, Windows DNS-SD, Pulse re-pin, device QA | 1 |

Steps 3a and 3b run in parallel, as do 4a and 4b. One owner per file; `Package.swift` and `API.swift` are written only in step 2.

Not settled here: how a computer's fingerprint first gets onto the phone's trusted list. The handoff does not say (`:58`). The engine supports any flow through `setTrustedFingerprints`; the UX belongs to ScrapeRight and Pulse.

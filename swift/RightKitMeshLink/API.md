# MeshLink public API

For RightKit's leakage check before any commit. MeshLink is app-neutral: it contains no application
names, no application protocol and no policy. Platform: iOS 18, Swift 6. Module `MeshLink`; native
module `RightKitMeshFFI` (C header `rightkit_mesh_ffi.h`).

## Swift

```swift
public actor MeshLink {
    public static let maxBodyBytes: Int                       // 262144
    public init(stateDirectory: URL, relayURL: URL?, deviceName: String,
                keychainService: String, pairingALPN: String, uriSchemes: [String] = [],
                events: EventSink? = nil)
    public typealias EventSink = @Sendable (_ event: String, _ detail: String, _ failed: Bool) -> Void
    public func beginPairing(uri: String) async throws -> PairingPrompt
    public func completePairing(id: String, code: String) async throws -> Peer
    public func cancelPairing(id: String) async
    public func peers() async throws -> [Peer]
    public func unpair(_ peer: Peer) async throws
    public func request(peer: Peer, alpn: String, body: Data, timeout: TimeInterval = 30) async throws -> Data
    public nonisolated func stream(peer: Peer, alpn: String, reconnect: Bool = true,
                                   open: @escaping @Sendable () async -> Data) -> AsyncThrowingStream<Data, Error>
    public nonisolated func stream(peer: Peer, alpn: String, open: Data, reconnect: Bool = true) -> AsyncThrowingStream<Data, Error>
    public nonisolated func peerState(_ peer: Peer) -> AsyncStream<PeerState>
    public func stop() async
}
public struct Peer: Sendable, Hashable, Codable, Identifiable   // id (device id), endpointID, name, platform, relayURL, pairedAt
public struct PairingPrompt: Sendable, Hashable, Identifiable    // id, hostName, hostDeviceID, hostPlatform, fingerprint, expiresAt
public enum PeerState: String, Sendable, Hashable               // connecting, direct, relayed, offline
public struct MeshLinkError: Error, LocalizedError, Sendable, Equatable   // code, message, retryable
```

Notes
* The identity seed is created and read in the Keychain through RightKitSwift's `SecretsStore` (`keychainService`, account `identity-seed-v1`,
  `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`, not synchronisable) and passed to the library; never a file.
* `events` receives `keychain.seed` (`found`, `created`, `created_while_paired` when no seed exists but `hosts.json`
  does, or the Keychain error code) and `mesh.start` (ok or the library's error code, with duration). Details hold
  codes and durations only. A read failure other than "absent" throws `keychain` and never mints a new identity.
* The endpoint starts lazily on first use. `relayURL` nil uses the relay stored with the latest paired computer.
* `uriSchemes`: pairing codes are `<scheme>://pair?v=1&e=..&p=..&x=..&r=..&n=..`; `meshlink` is always accepted,
  the app adds its own. The library checks only the scheme and `pair?`; the fields are checked natively.
* `request`: body is written as is, the sending side is finished, the response is read to the end of the stream.
  One request per stream, one stream per connection. Each direction at most 262144 bytes.
* `stream`: one long-lived stream per call; `open` is the first frame (called again on every reconnect, so the app
  can resume from its own cursor); the computer then pushes frames. Frames are a 4-byte big-endian length plus
  payload in each direction, each at most 262144 bytes; the API yields payloads. `reconnect` (default on) reopens
  after a stream ends or fails retryably, 1, 2, 4 ... 30 s, on the same `AsyncThrowingStream`; a non-retryable error
  throws. Cancelling the consumer closes the stream.
* `peerState`: current state at once, then each change. `connecting`, `direct` (local network or hole-punched),
  `relayed` (through the relay; a path the library cannot prove direct counts as relayed), `offline` (no live
  connection: last attempt failed, stream ended, or never contacted). Requests set it; an open stream re-reads the
  path every 3 s, so a move between direct and relayed shows. Reconnect of the QUIC connection itself and path
  migration are iroh's; the library adds the stream reopen above.
* Several computers: `peers()` is a list and every call names its `Peer`; nothing is global to one computer.
* Error codes: `invalid_uri`, `expired`, `timeout`, `connect`, `stream`, `io`, `closed`, `no_code`, `code_mismatch`,
  `pin_mismatch`, `signature_invalid`, `schema_version`, `malformed`, `not_paired`, `not_started`, `too_large`,
  `no_answer`, `store`, `keychain`, `mesh`, `identity`, `random`, `internal`, `panic`, `invalid_request`.

## C (rightkit_mesh_ffi.h)

`rkmesh_start`, `rkmesh_pair_begin`, `rkmesh_pair_code`, `rkmesh_pair_cancel`, `rkmesh_peers`, `rkmesh_request`, `rkmesh_stream_open`,
`rkmesh_stream_next`, `rkmesh_stream_close`, `rkmesh_peer_state`, `rkmesh_circle_sync`, `rkmesh_unpair`, `rkmesh_stop`,
`rkmesh_free_string`, `rkmesh_free_bytes`. JSON results `{"ok":true,..}` or `{"ok":false,"code","message","retryable"}`.

## Product-named strings that are wire protocol, not API

These sit in the Rust library (`rightkit-mesh-ffi/src/pairing.rs`) because the desktop Host uses them and a
different string makes every typed code fail. They are not exposed in the Swift or C API. RightKit renames them
only together with the desktop side (`core/src/devicelink`), as one protocol version:

* Domain strings: `pulse-link/pair-commit/v2`, `pulse-link/pair-transcript/v2`, `pulse-link/pair-code/v1`,
  `pulse-link/pair-confirm/v1`, `pulse-link/key-fingerprint/v1`, `pulse-link/device-id/v1`.
* The joiner platform value `ios` and the host link port `1` (the mesh's stand-in for the old TLS pin and port).

The pairing protocol name (`pulse/pair/1` on the desktop) is NOT in the library: the app passes it as `pairingALPN`.
Also `rightkit-mesh` (the dependency) carries its own `pulse-link` feature, off here.

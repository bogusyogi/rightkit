import Foundation
import Security
import RightKitCredentials
import RightKitMeshFFI

/// A paired computer.
public struct Peer: Sendable, Hashable, Codable, Identifiable {
    /// The computer's device id (`dev_` plus 32 hex). Stable; use it as the identity.
    public let id: String
    /// The computer's endpoint id: its public key, 64 hex.
    public let endpointID: String
    public let name: String
    /// The platform the computer declared when it paired (for example `mac`, `windows`).
    public let platform: String
    public let relayURL: URL?
    public let pairedAt: Date
}

/// What to show the user between scanning a code and typing the number the computer displays.
public struct PairingPrompt: Sendable, Hashable, Identifiable {
    public let id: String
    public let hostName: String
    public let hostDeviceID: String
    public let hostPlatform: String
    /// Grouped hex of the computer's key (`a1b2 c3d4 ...`). The computer's owner sees the same
    /// string for this phone's key when approving.
    public let fingerprint: String
    public let expiresAt: Date
    /// The scanned code carried a single-use secret: the app sends no typed number. Call
    /// `completePairing` at once (any text) to wait for the result.
    public let autoJoin: Bool
}

/// The last known path to a paired computer.
public enum PeerState: String, Sendable, Hashable {
    /// A connection is being made.
    case connecting
    /// Connected on a direct path (local network or hole-punched).
    case direct
    /// Connected through the relay.
    case relayed
    /// No live connection: the last attempt failed, the stream ended, or none was made yet.
    case offline
}

public struct MeshLinkError: Error, LocalizedError, Sendable, Equatable {
    /// A stable machine word: `invalid_uri`, `not_paired`, `code_mismatch`, `expired`, `timeout`,
    /// `connect`, `closed`, `too_large`, `keychain`, and any code the library returns.
    public let code: String
    public let message: String
    /// True when the same call can succeed later.
    public let retryable: Bool

    public var errorDescription: String? { message }
}

/// The phone's side of the device mesh. Pairing is the desktop's short-code protocol with this
/// phone as the Joiner; after it, `request` sends one request on one stream to a paired computer.
/// The identity seed lives in the Keychain (this device only, after first unlock) and is never
/// written to a file.
public actor MeshLink {
    /// Largest request or response body, in bytes.
    public static let maxBodyBytes = 256 * 1024

    private let stateDirectory: URL
    private let relayURL: URL?
    private let deviceName: String
    private let keychainService: String
    private let pairingALPN: String
    private let schemes: Set<String>
    private let events: EventSink?
    private var started = false

    /// Receives startup outcomes: an event name and a detail made only of codes, counts and durations,
    /// never the seed, a key or message text. Called on the actor; keep it cheap.
    public typealias EventSink = @Sendable (_ event: String, _ detail: String, _ failed: Bool) -> Void

    /// - Parameters:
    ///   - stateDirectory: where paired computers are remembered (public data only).
    ///   - relayURL: the relay to use; nil uses the one stored with the latest paired computer
    ///     (none: direct and local network only).
    ///   - deviceName: how this phone introduces itself to a computer.
    ///   - keychainService: the Keychain service that holds the identity seed.
    ///   - pairingALPN: the protocol name the computer serves its pairing on.
    ///   - uriSchemes: URI schemes of pairing codes this app accepts, besides `meshlink`.
    ///   - events: optional sink for startup outcomes (identity read, endpoint start), for app diagnostics.
    public init(
        stateDirectory: URL,
        relayURL: URL?,
        deviceName: String,
        keychainService: String,
        pairingALPN: String,
        uriSchemes: [String] = [],
        events: EventSink? = nil
    ) {
        self.stateDirectory = stateDirectory
        self.relayURL = relayURL
        self.deviceName = deviceName
        self.keychainService = keychainService
        self.pairingALPN = pairingALPN
        self.schemes = Set(uriSchemes.map { $0.lowercased() })
        self.events = events
    }

    /// Dial the computer named by a scanned pairing code and run the pairing up to the point where
    /// the user must type the number the computer shows.
    public func beginPairing(uri: String) async throws -> PairingPrompt {
        try PairingURI.check(uri, accepting: schemes)
        try await start()
        let reply = try await Native.run(PromptReply.self) {
            uri.withCString { rkmesh_pair_begin($0) }
        }
        return PairingPrompt(
            id: reply.pairingId,
            hostName: reply.hostName,
            hostDeviceID: reply.hostDeviceId,
            hostPlatform: reply.hostPlatform,
            fingerprint: reply.fingerprint,
            expiresAt: Date(timeIntervalSince1970: Double(reply.expiresAtMs) / 1000),
            autoJoin: reply.autoJoin ?? false
        )
    }

    /// Send the typed number and wait for the computer's owner to approve this phone. A wrong
    /// number ends the pairing (`code_mismatch`); begin again from a new code.
    public func completePairing(id: String, code: String) async throws -> Peer {
        try await start()
        let reply = try await Native.run(PeerReply.self) {
            id.withCString { id in code.withCString { rkmesh_pair_code(id, $0) } }
        }
        return reply.peer.value
    }

    /// Abandon a pairing that is waiting for its number.
    public func cancelPairing(id: String) async {
        guard started else { return }
        _ = try? await Native.run(Empty.self) { id.withCString { rkmesh_pair_cancel($0) } }
    }

    /// The paired computers.
    public func peers() async throws -> [Peer] {
        try await start()
        let reply = try await Native.run(PeersReply.self) { rkmesh_peers() }
        return reply.peers.map(\.value)
    }

    /// Pull `peer`'s signed circle member list and the signed removals it holds for this phone,
    /// verified with the key this phone paired with. A computer is added only when an owner-check
    /// endorsement chain the phone trusts carries it; one is removed only by a valid signed removal
    /// (the list never removes what it omits). Returns how many were added and removed; retry-safe.
    public func syncCircle(with peer: Peer) async throws -> (added: Int, removed: Int) {
        try await start()
        let reply = try await Native.run(CircleReply.self) { peer.id.withCString { rkmesh_circle_sync($0) } }
        return (reply.added, reply.removed)
    }

    /// Forget a computer on this phone. Its owner revokes this phone on the computer itself.
    public func unpair(_ peer: Peer) async throws {
        try await start()
        _ = try await Native.run(Empty.self) { peer.id.withCString { rkmesh_unpair($0) } }
    }

    /// One request, one response, on one stream to a paired computer. `body` is sent as is and the
    /// sending side is then finished; the response is read to the end of the stream. Both are at
    /// most `maxBodyBytes`. `timeout` bounds the wait for the response (connecting waits 15 s).
    public func request(peer: Peer, alpn: String, body: Data, timeout: TimeInterval = 30) async throws -> Data {
        guard body.count <= Self.maxBodyBytes else {
            throw MeshLinkError(code: "too_large", message: "The request is too large.", retryable: false)
        }
        try await start()
        let seconds = UInt32(max(1, min(timeout.rounded(.up), 600)))
        return try await Native.request(peer: peer.id, alpn: alpn, body: body, timeoutSeconds: seconds)
    }

    /// A long-lived stream to a paired computer on `alpn`: `open` is sent as the first frame, then the
    /// computer pushes frames until the stream ends. Frames are a 4-byte big-endian length and the
    /// payload, each at most `maxBodyBytes`; this API hands you the payloads.
    ///
    /// With `reconnect` (the default) a stream that ends or fails with a retryable error is opened
    /// again after 1, 2, 4 ... 30 seconds, calling `open` again each time so the app can resume from
    /// its own cursor; frames keep arriving on the same `AsyncThrowingStream`. A path change inside a
    /// live connection (direct to relayed and back) is handled by the transport and needs nothing
    /// here; watch it with `peerState`. Without `reconnect`, the sequence finishes when the stream
    /// ends. A non-retryable error finishes it by throwing. Cancelling the consumer closes the stream.
    public nonisolated func stream(
        peer: Peer,
        alpn: String,
        reconnect: Bool = true,
        open: @escaping @Sendable () async -> Data
    ) -> AsyncThrowingStream<Data, Error> {
        AsyncThrowingStream { continuation in
            let task = Task {
                var delay: UInt64 = 1
                while !Task.isCancelled {
                    do {
                        let id = try await self.openStream(peer: peer, alpn: alpn, body: await open())
                        delay = 1
                        try await Native.pump(streamID: id) { continuation.yield($0) }
                    } catch let error as MeshLinkError where !error.retryable || !reconnect {
                        continuation.finish(throwing: error)
                        return
                    } catch is CancellationError {
                        break
                    } catch {
                        if !reconnect { continuation.finish(throwing: error); return }
                    }
                    if !reconnect { break }
                    try? await Task.sleep(for: .seconds(delay))
                    delay = min(delay * 2, 30)
                }
                continuation.finish()
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    /// `stream(peer:alpn:reconnect:open:)` with a fixed opening frame.
    public nonisolated func stream(peer: Peer, alpn: String, open: Data, reconnect: Bool = true) -> AsyncThrowingStream<Data, Error> {
        stream(peer: peer, alpn: alpn, reconnect: reconnect) { open }
    }

    /// The state of the path to a paired computer: the current value at once, then each change.
    /// Requests and open streams update it; an open stream notices a path change within 3 seconds.
    public nonisolated func peerState(_ peer: Peer) -> AsyncStream<PeerState> {
        AsyncStream { continuation in
            let task = Task {
                var last: PeerState?
                while !Task.isCancelled {
                    let now = (try? await self.currentState(of: peer)) ?? .offline
                    if now != last { continuation.yield(now); last = now }
                    try? await Task.sleep(for: .milliseconds(1500))
                }
                continuation.finish()
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    fileprivate func openStream(peer: Peer, alpn: String, body: Data) async throws -> UInt64 {
        guard body.count <= Self.maxBodyBytes else {
            throw MeshLinkError(code: "too_large", message: "The opening frame is too large.", retryable: false)
        }
        try await start()
        let reply = try await Native.run(StreamOpened.self) {
            peer.id.withCString { peer in
                alpn.withCString { alpn in
                    body.withUnsafeBytes { rkmesh_stream_open(peer, alpn, $0.bindMemory(to: UInt8.self).baseAddress, $0.count) }
                }
            }
        }
        return reply.streamId
    }

    /// Whether the current direct path is on the local network (same subnet, private/link-local
    /// address), as named by the native `path` field of the peer-state reply. False for a
    /// hole-punched path, for relayed/offline, and when the native layer does not report it.
    public func isNearby(_ peer: Peer) async -> Bool {
        try? await start()
        guard let reply = try? await Native.run(StateReply.self, { peer.id.withCString { rkmesh_peer_state($0) } }) else { return false }
        return reply.state == "direct" && reply.path == "nearby"
    }

    fileprivate func currentState(of peer: Peer) async throws -> PeerState {
        try await start()
        let reply = try await Native.run(StateReply.self) { peer.id.withCString { rkmesh_peer_state($0) } }
        return PeerState(rawValue: reply.state) ?? .offline
    }

    /// Stop the endpoint and wipe the seed from memory.
    public func stop() async {
        guard started else { return }
        _ = try? await Native.run(Empty.self) { rkmesh_stop() }
        started = false
    }

    private func start() async throws {
        if started { return }
        let began = Date()
        try FileManager.default.createDirectory(at: stateDirectory, withIntermediateDirectories: true)
        let pairedState = FileManager.default.fileExists(atPath: stateDirectory.appendingPathComponent("hosts.json").path)
        var seed: [UInt8]
        do {
            let loaded = try Keychain.loadOrCreateSeed(service: keychainService)
            seed = loaded.seed
            if loaded.created && pairedState {
                // The identity item is absent while paired computers are remembered: the old identity is gone
                // (restore to a new phone, Keychain reset). The paired computers will not know the new one.
                events?("keychain.seed", "outcome=created_while_paired", true)
            } else {
                events?("keychain.seed", "outcome=\(loaded.created ? "created" : "found")", false)
            }
        } catch let error as MeshLinkError {
            events?("keychain.seed", "outcome=error code=\(Self.plainCode(error.code)) retryable=\(error.retryable)", true)
            throw error
        }
        defer { seed.withUnsafeMutableBytes { _ = memset_s($0.baseAddress, $0.count, 0, $0.count) } }
        let directory = stateDirectory.path
        let relay = relayURL?.absoluteString
        let name = deviceName
        let alpn = pairingALPN
        let copy = seed
        let reply: StartReply
        do {
            reply = try await Native.run(StartReply.self) {
                copy.withUnsafeBufferPointer { seedPointer in
                    directory.withCString { directory in
                        name.withCString { name in
                            alpn.withCString { alpn in
                                if let relay {
                                    return relay.withCString { rkmesh_start(directory, $0, name, seedPointer.baseAddress, alpn) }
                                }
                                return rkmesh_start(directory, nil, name, seedPointer.baseAddress, alpn)
                            }
                        }
                    }
                }
            }
        } catch let error as MeshLinkError {
            events?("mesh.start", "outcome=error code=\(Self.plainCode(error.code)) retryable=\(error.retryable) ms=\(Self.ms(since: began))", true)
            throw error
        }
        if reply.relayRefused == true {
            events?("mesh.relay", "outcome=refused code=invalid_relay source=\(relay == nil ? "stored" : "given")", true)
        }
        events?("mesh.start", "outcome=ok relay=\(relay == nil ? "stored" : "given") ms=\(Self.ms(since: began))", false)
        started = true
    }
}

extension MeshLink {
    /// An event's error code when it is one of the mesh library's own codes, else `other`: no string from
    /// the library is trusted to be free of secrets.
    fileprivate static func plainCode(_ text: String) -> String {
        let known: Set<String> = ["clock_rolled_back", "internal", "read_failed", "too_big_over_relay", "unsupported", "write_failed", "closed", "code_mismatch", "connect", "expired", "identity",
                                  "invalid_relay", "invalid_request", "invalid_uri", "keychain", "malformed", "no_answer", "no_code",
                                  "not_authorized", "not_found", "not_paired", "not_started", "offline", "panic",
                                  "pin_mismatch", "refused", "signature_invalid", "store", "timeout", "too_large"]
        return known.contains(text) ? text : "other"
    }

    fileprivate static func ms(since start: Date) -> Int { Int(Date().timeIntervalSince(start) * 1000) }
}

// MARK: - Pairing URI

enum PairingURI {
    /// The scheme must be `meshlink` or one the app named, and the host `pair`. The rest of the
    /// code is checked by the library when pairing begins.
    static func check(_ uri: String, accepting schemes: Set<String>) throws {
        let invalid = MeshLinkError(code: "invalid_uri", message: "That is not a pairing code.", retryable: false)
        let trimmed = uri.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed.utf8.count <= 2048, let range = trimmed.range(of: "://") else { throw invalid }
        let scheme = trimmed[..<range.lowerBound].lowercased()
        guard scheme == "meshlink" || schemes.contains(scheme) else { throw invalid }
        guard trimmed[range.upperBound...].hasPrefix("pair?") else { throw invalid }
    }
}

// MARK: - Keychain

enum Keychain {
    private static let account = "identity-seed-v1"

    /// The 32-byte identity seed, stored by RightKitSwift's `SecretsStore` (the same generic-password item as before:
    /// service `service`, account `identity-seed-v1`, not synchronizable, after first unlock this device only). A
    /// read failure other than "absent" is an error: an unreadable Keychain must not mint a new identity over the
    /// old one, and a tripped `RIGHTKIT_NO_KEYCHAIN` guard refuses (`.deny`) instead of keeping a seed in memory.
    /// `created` is true when no identity existed and this call stored a new one.
    static func loadOrCreateSeed(service: String) throws -> (seed: [UInt8], created: Bool) {
        do {
            let store = try SecretsStore(namespace: service, guardMode: .deny)
            if let data = try store.get(account) {
                guard data.count == 32 else {
                    throw MeshLinkError(code: "keychain", message: "The stored identity is damaged.", retryable: false)
                }
                return ([UInt8](data), false)
            }
            var seed = [UInt8](repeating: 0, count: 32)
            guard SecRandomCopyBytes(kSecRandomDefault, seed.count, &seed) == errSecSuccess else {
                throw MeshLinkError(code: "keychain", message: "No random bytes are available.", retryable: true)
            }
            if try store.setIfAbsent(account, Data(seed)) { return (seed, true) }
            // Another writer created it first: read theirs.
            seed.withUnsafeMutableBytes { _ = memset_s($0.baseAddress, $0.count, 0, $0.count) }
            return try loadOrCreateSeed(service: service)
        } catch let error as SecretsError {
            switch error {
            case .keychain(let status):
                throw MeshLinkError(code: "keychain", message: "The Keychain could not be used (\(status)).", retryable: status == errSecInteractionNotAllowed)
            case .guarded, .invalid:
                throw MeshLinkError(code: "keychain", message: "The identity could not be stored (\(error)).", retryable: false)
            }
        }
    }
}

// MARK: - The C library

private struct Status: Decodable {
    let ok: Bool
    let code: String?
    let message: String?
    let retryable: Bool?
}

private struct Empty: Decodable, Sendable {}

private struct PromptReply: Decodable, Sendable {
    let pairingId: String
    let hostName: String
    let hostDeviceId: String
    let hostPlatform: String
    let fingerprint: String
    let expiresAtMs: UInt64
    let autoJoin: Bool?
}

private struct PeerRecord: Decodable, Sendable {
    let deviceId: String
    let endpointId: String
    let name: String?
    let platform: String
    let relay: String?
    let pairedAtMs: UInt64

    var value: Peer {
        Peer(
            id: deviceId,
            endpointID: endpointId,
            name: name ?? deviceId,
            platform: platform,
            relayURL: relay.flatMap(URL.init(string:)),
            pairedAt: Date(timeIntervalSince1970: Double(pairedAtMs) / 1000)
        )
    }
}

private struct StartReply: Decodable, Sendable { let relayRefused: Bool? }
private struct StreamOpened: Decodable, Sendable { let streamId: UInt64 }
private struct StateReply: Decodable, Sendable { let state: String; let path: String? }
private struct CircleReply: Decodable, Sendable { let added: Int; let removed: Int }
private struct EventReply: Decodable, Sendable { let event: String }

private struct PeerReply: Decodable, Sendable { let peer: PeerRecord }
private struct PeersReply: Decodable, Sendable { let peers: [PeerRecord] }

private enum Native {
    /// Run a blocking library call off the cooperative pool, take ownership of its JSON answer
    /// and decode it.
    static func run<T: Decodable & Sendable>(
        _ type: T.Type,
        _ work: @escaping @Sendable () -> UnsafeMutablePointer<CChar>?
    ) async throws -> T {
        let data: Data = await withCheckedContinuation { continuation in
            DispatchQueue.global(qos: .userInitiated).async {
                continuation.resume(returning: take(work()))
            }
        }
        return try decode(type, from: data)
    }

    static func request(peer: String, alpn: String, body: Data, timeoutSeconds: UInt32) async throws -> Data {
        let result: (Data, Data?) = await withCheckedContinuation { continuation in
            DispatchQueue.global(qos: .userInitiated).async {
                var out: UnsafeMutablePointer<UInt8>?
                var length = 0
                let status = peer.withCString { peer in
                    alpn.withCString { alpn in
                        body.withUnsafeBytes { raw in
                            rkmesh_request(
                                peer, alpn,
                                raw.bindMemory(to: UInt8.self).baseAddress, raw.count,
                                timeoutSeconds, &out, &length
                            )
                        }
                    }
                }
                var answer: Data?
                if let out {
                    answer = Data(bytes: out, count: length)
                    rkmesh_free_bytes(out, length)
                }
                continuation.resume(returning: (take(status), answer))
            }
        }
        _ = try decode(Empty.self, from: result.0)
        guard let answer = result.1 else {
            throw MeshLinkError(code: "no_answer", message: "The computer sent no answer.", retryable: true)
        }
        return answer
    }

    /// Deliver each pushed frame to `deliver` until the stream ends. Cancelling the task closes the
    /// stream, which wakes the blocked wait.
    static func pump(streamID: UInt64, deliver: @escaping @Sendable (Data) -> Void) async throws {
        try await withTaskCancellationHandler {
            while !Task.isCancelled {
                let (status, frame): (Data, Data?) = await withCheckedContinuation { continuation in
                    DispatchQueue.global(qos: .userInitiated).async {
                        var out: UnsafeMutablePointer<UInt8>?
                        var length = 0
                        let status = rkmesh_stream_next(streamID, 1000, &out, &length)
                        var frame: Data?
                        if let out {
                            frame = Data(bytes: out, count: length)
                            rkmesh_free_bytes(out, length)
                        }
                        continuation.resume(returning: (take(status), frame))
                    }
                }
                let event = try decode(EventReply.self, from: status).event
                if event == "end" { return }
                if event == "frame", let frame { deliver(frame) }
            }
            throw CancellationError()
        } onCancel: {
            if let text = Optional(rkmesh_stream_close(streamID)) { rkmesh_free_string(text) }
        }
    }

    private static func take(_ pointer: UnsafeMutablePointer<CChar>?) -> Data {
        guard let pointer else { return Data() }
        defer { rkmesh_free_string(pointer) }
        return Data(bytes: pointer, count: strlen(pointer))
    }

    private static func decode<T: Decodable>(_ type: T.Type, from data: Data) throws -> T {
        let decoder = JSONDecoder()
        guard let status = try? decoder.decode(Status.self, from: data) else {
            throw MeshLinkError(code: "internal", message: "The mesh library gave an unreadable answer.", retryable: false)
        }
        guard status.ok else {
            throw MeshLinkError(
                code: status.code ?? "failed",
                message: status.message ?? "The request failed.",
                retryable: status.retryable ?? false
            )
        }
        do { return try decoder.decode(type, from: data) }
        catch { throw MeshLinkError(code: "internal", message: "The mesh library gave an unexpected answer.", retryable: false) }
    }
}

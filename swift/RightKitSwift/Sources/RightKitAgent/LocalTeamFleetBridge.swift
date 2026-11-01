import Foundation

/// User-owned iOS entry point for ENG-96/97 peer fleets.
///
/// This bridge accepts only an app-supplied manifest and shared brief. Agents
/// never receive a mutable bridge/configuration; dispatch can occur only from
/// `dispatchAfterUserApproval`, which binds one user confirmation to one peer
/// contact in shared Rust runtime code.
public actor LocalTeamFleetBridge {
    public struct Configuration: Codable, Sendable {
        public var storageDir: String
        public var manifestJson: String
        public var sharedContext: [String: String]

        public init(storageDir: String, manifestJson: String, sharedContext: [String: String] = [:]) {
            self.storageDir = storageDir
            self.manifestJson = manifestJson
            self.sharedContext = sharedContext
        }
    }

    public struct Summary: Codable, Sendable, Equatable {
        public var teamId: String
        public var peerIds: [String]
        public var sharedContextPrompt: String?
    }

    public struct DispatchReceipt: Codable, Sendable, Equatable {
        public var messageId: String
        public var teamId: String
        public var requesterPeerId: String
        public var targetPeerId: String
        public var depth: UInt8
        public var nonBlocking: Bool
    }

    public struct ReceivedTask: Codable, Sendable, Equatable {
        public var messageId: String
        public var requesterPeerId: String
        public var targetPeerId: String
        public var task: String
        public var depth: UInt8
        public var sharedContextPrompt: String?
    }

    /// Prompt projected from a queued mailbox item. It remains pending until
    /// iOS's local peer runtime actually executes and acknowledges it.
    public struct ExecutionResult: Codable, Sendable, Equatable {
        public var messageId: String
        public var prepared: Bool
        public var status: String
        public var prompt: String
    }

    public enum Failure: Error, LocalizedError {
        case encoding
        case unavailable(String)
        case malformed

        public var errorDescription: String? {
            switch self {
            case .encoding: return "Could not encode fleet request."
            case .unavailable(let message): return message
            case .malformed: return "Fleet runtime returned a malformed response."
            }
        }
    }

    private struct Envelope<Value: Decodable>: Decodable {
        let ok: Bool
        let value: Value?
        let error: String?
    }

    private let configuration: Configuration

    public init(configuration: Configuration) {
        self.configuration = configuration
    }

    public func importFleet() async throws -> Summary {
        try await call(configuration, coderight_mobile_team_fleet_import)
    }

    /// Call only after the app has shown its human approval control. This is
    /// deliberately absent from agent-facing tools; Rust rechecks exact peer
    /// identities and chain depth before it writes any mailbox item.
    public func dispatchAfterUserApproval(
        requesterPeerId: String,
        targetPeerId: String,
        task: String,
        depth: UInt8
    ) async throws -> DispatchReceipt {
        struct Request: Encodable {
            let storageDir: String
            let manifestJson: String
            let sharedContext: [String: String]
            let requesterPeerId: String
            let targetPeerId: String
            let task: String
            let depth: UInt8
            let userApproved = true
        }
        return try await call(
            Request(
                storageDir: configuration.storageDir,
                manifestJson: configuration.manifestJson,
                sharedContext: configuration.sharedContext,
                requesterPeerId: requesterPeerId,
                targetPeerId: targetPeerId,
                task: task,
                depth: depth
            ),
            coderight_mobile_team_fleet_dispatch_after_user_approval
        )
    }

    public func receive(for peerId: String) async throws -> [ReceivedTask] {
        struct Request: Encodable {
            let storageDir: String
            let manifestJson: String
            let sharedContext: [String: String]
            let peerId: String
        }
        return try await call(
            Request(
                storageDir: configuration.storageDir,
                manifestJson: configuration.manifestJson,
                sharedContext: configuration.sharedContext,
                peerId: peerId
            ),
            coderight_mobile_team_fleet_receive
        )
    }

    /// Project one queued delegation into its target prompt without consuming
    /// work. Immutable user-owned context is injected into that visible input.
    public func executeNext(for peerId: String) async throws -> ExecutionResult? {
        struct Request: Encodable {
            let storageDir: String
            let manifestJson: String
            let sharedContext: [String: String]
            let peerId: String
        }
        return try await call(
            Request(
                storageDir: configuration.storageDir,
                manifestJson: configuration.manifestJson,
                sharedContext: configuration.sharedContext,
                peerId: peerId
            ),
            coderight_mobile_team_fleet_execute_next
        )
    }

    private func call<Request: Encodable, Value: Decodable>(
        _ request: Request,
        _ operation: @escaping @convention(c) (UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>?
    ) async throws -> Value {
        let payload: Data
        do {
            payload = try JSONEncoder().encode(request)
        } catch {
            throw Failure.encoding
        }
        let raw: String? = await withCheckedContinuation { continuation in
            DispatchQueue.global(qos: .userInitiated).async {
                guard let request = String(data: payload, encoding: .utf8) else {
                    continuation.resume(returning: nil)
                    return
                }
                let result = request.withCString { pointer -> String? in
                    guard let response = operation(pointer) else {
                        return nil
                    }
                    defer { coderight_mobile_string_free(response) }
                    return String(cString: response)
                }
                continuation.resume(returning: result)
            }
        }
        guard let raw, let data = raw.data(using: .utf8) else {
            throw Failure.malformed
        }
        let envelope = try JSONDecoder().decode(Envelope<Value>.self, from: data)
        guard envelope.ok, let value = envelope.value else {
            throw Failure.unavailable(envelope.error ?? "Fleet request failed.")
        }
        return value
    }
}

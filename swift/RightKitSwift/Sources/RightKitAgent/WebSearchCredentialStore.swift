import Foundation
import Observation

/// TinyFish is preferred when configured. Without a key, Rust retains its
/// keyless DuckDuckGo default.
@Observable
@MainActor
public final class WebSearchCredentialStore {
    public static let tinyFishID = "tinyfish"

    public private(set) var tinyFishConfigured: Bool
    private let secrets: SecretStore

    public init(secrets: SecretStore = SecretStore()) {
        self.secrets = secrets
        tinyFishConfigured = secrets.isStored(kind: .toolAPIKey, id: Self.tinyFishID)
    }

    public func saveTinyFish(_ apiKey: String) throws {
        let key = apiKey.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !key.isEmpty else { return }
        try secrets.store(key, kind: .toolAPIKey, id: Self.tinyFishID)
        tinyFishConfigured = true
    }

    public func removeTinyFish() {
        try? secrets.remove(kind: .toolAPIKey, id: Self.tinyFishID)
        tinyFishConfigured = false
    }

    public func tinyFishAPIKey(gate: AppLockGate) async throws -> String? {
        guard tinyFishConfigured else { return nil }
        try await gate.unlockIfNeeded(reason: "Use your TinyFish search key")
        return try secrets.read(kind: .toolAPIKey, id: Self.tinyFishID)
    }
}

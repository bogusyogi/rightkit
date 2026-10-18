import Foundation
import RightKitCredentials

/// Where provider API keys come from, asked on every call so a rotated key applies at once.
public protocol LLMCredentialSource: Sendable {
    /// The key for provider id `provider`, or nil when none is stored. Throws on a storage failure.
    func credential(provider: String) throws -> String?
}

/// Keys held in a RightKitCredentials ``SecretsStore``: namespace = service, account = provider id
/// (`groq`, `openrouter`, ...), the same layout `rightkit-llm`'s `SecretStoreCredentials` reads.
public struct SecretsStoreCredentials: LLMCredentialSource {
    public let store: SecretsStore
    public init(store: SecretsStore) { self.store = store }

    public func credential(provider: String) throws -> String? {
        guard let value = try store.getString(provider) else { return nil }
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }
}

/// Fixed keys for one-shot tools. Not a storage mechanism; do not persist what you put here.
public struct StaticLLMCredentials: LLMCredentialSource {
    private let keys: [String: String]
    public init(_ keys: [String: String]) { self.keys = keys }
    public func credential(provider: String) throws -> String? { keys[provider] }
}

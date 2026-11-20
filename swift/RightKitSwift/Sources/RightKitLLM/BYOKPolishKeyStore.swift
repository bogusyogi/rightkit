import Foundation

/// Reuses host credential handles without creating Keychain items or changing sync policy.
/// Provider IDs, preference keys & migration identity must match existing persisted values.
@MainActor
public struct BYOKPolishKeyStore {
    public struct CredentialHandles {
        public let read: (String) -> String?
        public let write: (String, String) throws -> Void
        public let removeEverywhere: (String) -> Void
        public let hasCompletedMigration: () -> Bool
        public let migrateLegacyItem: (_ service: String, _ provider: String) -> Void
        public let markMigrationComplete: () -> Void

        public init(
            read: @escaping (String) -> String?,
            write: @escaping (_ key: String, _ provider: String) throws -> Void,
            removeEverywhere: @escaping (String) -> Void,
            hasCompletedMigration: @escaping () -> Bool,
            migrateLegacyItem: @escaping (_ service: String, _ provider: String) -> Void,
            markMigrationComplete: @escaping () -> Void
        ) {
            self.read = read
            self.write = write
            self.removeEverywhere = removeEverywhere
            self.hasCompletedMigration = hasCompletedMigration
            self.migrateLegacyItem = migrateLegacyItem
            self.markMigrationComplete = markMigrationComplete
        }
    }

    public let providerIDs: [String]
    private let defaults: UserDefaults
    private let selectedProviderKey: String
    private let lastOutcomeKey: String
    private let defaultProvider: String
    private let legacyService: String
    private let credentials: CredentialHandles

    public init(
        providerIDs: [String], defaults: UserDefaults = .standard,
        selectedProviderKey: String, lastOutcomeKey: String, defaultProvider: String,
        legacyService: String, credentials: CredentialHandles
    ) {
        self.providerIDs = providerIDs
        self.defaults = defaults
        self.selectedProviderKey = selectedProviderKey
        self.lastOutcomeKey = lastOutcomeKey
        self.defaultProvider = defaultProvider
        self.legacyService = legacyService
        self.credentials = credentials
    }

    public func lastPolishOutcome() -> String { defaults.string(forKey: lastOutcomeKey) ?? "" }
    public func saveLastPolishOutcome(_ outcome: String) { defaults.set(outcome, forKey: lastOutcomeKey) }

    /// `validProviderIDs` includes local/on-device choices, which need no saved key.
    public func selectedProvider(validProviderIDs: [String]) -> String {
        guard let raw = defaults.string(forKey: selectedProviderKey), validProviderIDs.contains(raw) else {
            return defaultProvider
        }
        return raw
    }
    public func saveSelectedProvider(_ provider: String) { defaults.set(provider, forKey: selectedProviderKey) }
    public func hasKey(for provider: String) -> Bool { key(for: provider)?.isEmpty == false }
    public func key(for provider: String) -> String? { credentials.read(provider) }
    public func save(_ key: String, for provider: String) throws {
        guard !key.isEmpty else { deleteKey(for: provider); return }
        try credentials.write(key, provider)
    }
    /// Only explicit user deletion calls this; disabling sync must never use it.
    public func deleteKey(for provider: String) { credentials.removeEverywhere(provider) }
    public func savedProviders() -> Set<String> { Set(providerIDs.filter { hasKey(for: $0) }) }

    /// Returns credentials through RightKitLLM's existing Sendable one-shot source.
    /// Map persisted IDs to transport IDs explicitly (for example mixed-case legacy IDs).
    public func credentialSource(providerID: (String) -> String) -> StaticLLMCredentials {
        var values: [String: String] = [:]
        for provider in providerIDs {
            if let key = key(for: provider), !key.isEmpty { values[providerID(provider)] = key }
        }
        return StaticLLMCredentials(values)
    }

    public func migrateLegacyKeysIfNeeded() {
        guard !credentials.hasCompletedMigration() else { return }
        for provider in providerIDs { credentials.migrateLegacyItem(legacyService, provider) }
        credentials.markMigrationComplete()
    }
}

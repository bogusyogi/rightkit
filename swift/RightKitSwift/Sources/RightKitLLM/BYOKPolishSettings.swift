#if canImport(Combine)
import Combine
import Foundation

/// Observable editing state. App supplies provider IDs, display wording & credential handles.
@MainActor
public final class BYOKPolishSettings: ObservableObject {
    @Published public private(set) var savedProviders: Set<String> = []
    @Published public private(set) var selectedProvider: String
    @Published public var lastMessage = ""

    public enum Outcome { case saved, cleared, failed(Error) }
    private let store: BYOKPolishKeyStore
    private let message: (String, Outcome) -> String

    public init(store: BYOKPolishKeyStore, validProviderIDs: [String], message: @escaping (String, Outcome) -> String) {
        self.store = store
        self.message = message
        store.migrateLegacyKeysIfNeeded()
        self.selectedProvider = store.selectedProvider(validProviderIDs: validProviderIDs)
        reload()
    }

    public func reload() { savedProviders = store.savedProviders() }
    public func save(_ key: String, for provider: String) {
        do {
            try store.save(key.trimmingCharacters(in: .whitespacesAndNewlines), for: provider)
            reload()
            lastMessage = message(provider, .saved)
        } catch { lastMessage = message(provider, .failed(error)) }
    }
    public func clear(_ provider: String) {
        store.deleteKey(for: provider)
        reload()
        lastMessage = message(provider, .cleared)
    }
    public func select(_ provider: String) {
        selectedProvider = provider
        store.saveSelectedProvider(provider)
    }
}
#endif

import Foundation
#if canImport(Security)
import Security
#endif

/// Minimal persistence the module needs. `UserDefaults` for ordinary use, in-memory for tests.
public protocol KeyValueStore: Sendable {
    func string(forKey key: String) -> String?
    func set(_ value: String?, forKey key: String)
}

public struct UserDefaultsStore: KeyValueStore, @unchecked Sendable {
    private let defaults: UserDefaults
    public init(_ defaults: UserDefaults = .standard) { self.defaults = defaults }
    public func string(forKey key: String) -> String? { defaults.string(forKey: key) }
    public func set(_ value: String?, forKey key: String) {
        if let value { defaults.set(value, forKey: key) } else { defaults.removeObject(forKey: key) }
    }
}

public final class InMemoryStore: KeyValueStore, @unchecked Sendable {
    private let lock = NSLock()
    private var values: [String: String] = [:]
    public init() {}
    public func string(forKey key: String) -> String? { lock.withLock { values[key] } }
    public func set(_ value: String?, forKey key: String) { lock.withLock { values[key] = value } }
}

/// Last store-verified access, so a cold launch can report access before StoreKit has finished
/// re-reading entitlements. A hint, never authority: the first refresh replaces or clears it.
public struct AccessCache: Equatable, Sendable {
    public var productID: String?
    public var verifiedAt: Date?
    public var isActive: Bool { productID != nil }
}

struct AccessCacheStore {
    let store: any KeyValueStore
    let namespace: String

    private var productKey: String { "\(namespace).access.productID" }
    private var verifiedKey: String { "\(namespace).access.verifiedAt" }

    func load() -> AccessCache {
        let verified = store.string(forKey: verifiedKey).flatMap(Double.init).map(Date.init(timeIntervalSince1970:))
        return AccessCache(productID: store.string(forKey: productKey), verifiedAt: verified)
    }

    func save(productID: String, at date: Date) {
        store.set(productID, forKey: productKey)
        store.set(String(date.timeIntervalSince1970), forKey: verifiedKey)
    }

    func clear() {
        store.set(nil, forKey: productKey)
        store.set(nil, forKey: verifiedKey)
    }
}

/// Holds the stable `appAccountToken` for this buyer.
public protocol AccountTokenStore: Sendable {
    func load() -> UUID?
    func save(_ token: UUID)
}

public extension AccountTokenStore {
    func loadOrCreate() -> UUID {
        if let existing = load() { return existing }
        let token = UUID()
        save(token)
        return token
    }
}

public final class InMemoryAccountTokenStore: AccountTokenStore, @unchecked Sendable {
    private let lock = NSLock()
    private var token: UUID?
    public init(_ token: UUID? = nil) { self.token = token }
    public func load() -> UUID? { lock.withLock { token } }
    public func save(_ token: UUID) { lock.withLock { self.token = token } }
}

#if canImport(Security)
/// Keychain-backed token: survives app deletion and restore, so an upgrade bought after a
/// reinstall still links to the original purchase.
public struct KeychainAccountTokenStore: AccountTokenStore {
    private let service: String
    private let account: String

    public init(service: String, account: String = "app-account-token") {
        self.service = service
        self.account = account
    }

    public func load() -> UUID? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]
        var result: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &result) == errSecSuccess,
              let data = result as? Data,
              let text = String(data: data, encoding: .utf8)
        else { return nil }
        return UUID(uuidString: text)
    }

    public func save(_ token: UUID) {
        let data = Data(token.uuidString.utf8)
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
        let status = SecItemUpdate(query as CFDictionary, [kSecValueData as String: data] as CFDictionary)
        if status == errSecItemNotFound {
            var insert = query
            insert[kSecValueData as String] = data
            insert[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlock
            SecItemAdd(insert as CFDictionary, nil)
        }
    }
}
#endif

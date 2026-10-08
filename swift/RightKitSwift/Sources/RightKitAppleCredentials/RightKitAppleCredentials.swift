import Foundation

#if canImport(Security)
import Security
import LocalAuthentication
#endif

/// The two Keychain locations used by RightKit credential continuity.
public enum AppleCredentialScope: String, Codable, CaseIterable, Sendable {
    case synced
    case device
}

public enum AppleCredentialError: Error, LocalizedError, Equatable, Sendable {
    case invalidAccount
    case invalidValue
    case unavailable(String)
    case keychainStatus(Int32)

    public var errorDescription: String? {
        switch self {
        case .invalidAccount: return "Credential account is invalid."
        case .invalidValue: return "Credential value is empty."
        case let .unavailable(detail): return detail
        case let .keychainStatus(status): return "Keychain operation failed (status \(status))."
        }
    }
}

/// Narrow storage seam so tests can use an isolated in-memory namespace.
/// Backend operations are synchronous and must be safe when called by concurrent store users.
public protocol AppleCredentialBackend: AnyObject, Sendable {
    func status() -> Result<String?, AppleCredentialError>
    func read(service: String, account: String, scope: AppleCredentialScope) -> Result<Data?, AppleCredentialError>
    func exists(service: String, account: String, scope: AppleCredentialScope) -> Result<Bool, AppleCredentialError>
    func write(_ data: Data, service: String, account: String, scope: AppleCredentialScope) -> Result<Void, AppleCredentialError>
    func delete(service: String, account: String, scope: AppleCredentialScope) -> Result<Void, AppleCredentialError>
}

/// Deterministic backend for unit tests in an isolated in-memory namespace.
public final class InMemoryAppleCredentialBackend: AppleCredentialBackend, @unchecked Sendable {
    private var values: [String: Data] = [:]
    private let lock = NSLock()
    public let statusDetail: String?

    public init(statusDetail: String? = nil) { self.statusDetail = statusDetail }

    private func key(_ service: String, _ account: String, _ scope: AppleCredentialScope) -> String {
        "\(service)\u{1f}\(account)\u{1f}\(scope.rawValue)"
    }

    public func status() -> Result<String?, AppleCredentialError> { .success(statusDetail) }

    public func read(service: String, account: String, scope: AppleCredentialScope) -> Result<Data?, AppleCredentialError> {
        lock.lock(); defer { lock.unlock() }
        return .success(values[key(service, account, scope)])
    }

    public func exists(service: String, account: String, scope: AppleCredentialScope) -> Result<Bool, AppleCredentialError> {
        lock.lock(); defer { lock.unlock() }
        return .success(values[key(service, account, scope)] != nil)
    }

    public func write(_ data: Data, service: String, account: String, scope: AppleCredentialScope) -> Result<Void, AppleCredentialError> {
        lock.lock(); values[key(service, account, scope)] = data; lock.unlock()
        return .success(())
    }

    public func delete(service: String, account: String, scope: AppleCredentialScope) -> Result<Void, AppleCredentialError> {
        lock.lock(); values.removeValue(forKey: key(service, account, scope)); lock.unlock()
        return .success(())
    }
}

/// Per-app Keychain storage, used in-process by the owning app. There is deliberately no
/// out-of-process helper: any same-user process could drive an unauthenticated one.
public final class AppleCredentialStore: Sendable {
    public static let defaultService = "org.rightkit.shared.credentials"
    public static let shared = AppleCredentialStore()

    private let backend: AppleCredentialBackend
    public let service: String
    public let accessGroup: String?

    public init(
        service: String = AppleCredentialStore.defaultService,
        accessGroup: String? = nil,
        backend: AppleCredentialBackend? = nil
    ) {
        self.service = service
        self.accessGroup = accessGroup
        self.backend = backend ?? SecurityAppleCredentialBackend(accessGroup: accessGroup)
    }

    public func status() -> Result<(ready: Bool, detail: String?), AppleCredentialError> {
        switch backend.status() {
        case let .success(detail): return .success((detail == nil, detail))
        case let .failure(error): return .failure(error)
        }
    }

    /// Reads data only on explicit request. `exists` below is the passive path.
    public func read(_ account: String, scope: AppleCredentialScope) -> Result<String?, AppleCredentialError> {
        guard Self.validAccount(account) else { return .failure(.invalidAccount) }
        switch backend.read(service: service, account: account, scope: scope) {
        case let .success(data):
            guard let data else { return .success(nil) }
            return .success(String(data: data, encoding: .utf8))
        case let .failure(error): return .failure(error)
        }
    }

    /// Presence check never asks Keychain for secret bytes and never prompts.
    public func exists(_ account: String, scope: AppleCredentialScope) -> Result<Bool, AppleCredentialError> {
        guard Self.validAccount(account) else { return .failure(.invalidAccount) }
        return backend.exists(service: service, account: account, scope: scope)
    }

    public func write(_ value: String, account: String, scope: AppleCredentialScope) -> Result<Void, AppleCredentialError> {
        guard Self.validAccount(account) else { return .failure(.invalidAccount) }
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return .failure(.invalidValue) }
        return backend.write(Data(trimmed.utf8), service: service, account: account, scope: scope)
    }

    /// Explicit user deletion removes both copies. Callers must not use this for a sync toggle.
    public func deleteEverywhere(_ account: String) -> Result<Void, AppleCredentialError> {
        guard Self.validAccount(account) else { return .failure(.invalidAccount) }
        for scope in AppleCredentialScope.allCases {
            if case let .failure(error) = backend.delete(service: service, account: account, scope: scope) {
                return .failure(error)
            }
        }
        return .success(())
    }

    /// Turns sync off while preserving this device's copy and leaving synced data untouched.
    public func copySyncedToDeviceOnly(_ account: String) -> Result<Bool, AppleCredentialError> {
        switch read(account, scope: .synced) {
        case .success(nil): return .success(false)
        case let .success(value?):
            switch write(value, account: account, scope: .device) {
            case .success: return .success(true)
            case let .failure(error): return .failure(error)
            }
        case let .failure(error): return .failure(error)
        }
    }

    private static func validAccount(_ account: String) -> Bool {
        guard !account.isEmpty, account.utf8.count <= 200 else { return false }
        return account.unicodeScalars.allSatisfy { scalar in
            (scalar.value >= 48 && scalar.value <= 57) ||
            (scalar.value >= 65 && scalar.value <= 90) ||
            (scalar.value >= 97 && scalar.value <= 122) ||
            scalar == "." || scalar == "_" || scalar == "-"
        }
    }
}

#if canImport(Security)
/// Raw result of a passive Keychain query. `.notFound` still proves the query was usable.
public enum CredentialProbe: Equatable, Sendable {
    case available
    case notFound
    case missingEntitlement
    case interactionNotAllowed
    case failure(OSStatus)

    public init(rawStatus: OSStatus) {
        switch rawStatus {
        case errSecSuccess: self = .available
        case errSecItemNotFound: self = .notFound
        case errSecMissingEntitlement: self = .missingEntitlement
        case errSecInteractionNotAllowed: self = .interactionNotAllowed
        default: self = .failure(rawStatus)
        }
    }

    public var rawStatus: OSStatus {
        switch self {
        case .available: return errSecSuccess
        case .notFound: return errSecItemNotFound
        case .missingEntitlement: return errSecMissingEntitlement
        case .interactionNotAllowed: return errSecInteractionNotAllowed
        case let .failure(status): return status
        }
    }
}

/// Keychain locations swept by `deleteAnyScope`; synchronizable and local items share one sweep.
public enum CredentialDeletionScope: String, Sendable {
    case dataProtection
    case legacyFile
}

public struct CredentialDeletionOutcome: Equatable, Sendable {
    public let scope: CredentialDeletionScope
    public let rawStatus: OSStatus

    public init(scope: CredentialDeletionScope, rawStatus: OSStatus) {
        self.scope = scope
        self.rawStatus = rawStatus
    }

    /// Deleting an absent item is successful, but its original status remains available.
    public var succeeded: Bool { rawStatus == errSecSuccess || rawStatus == errSecItemNotFound }
}

/// Security.framework implementation. Queries explicitly disable UI interaction.
public final class SecurityAppleCredentialBackend: AppleCredentialBackend, @unchecked Sendable {
    private let accessGroup: String?

    public init(accessGroup: String? = nil) { self.accessGroup = accessGroup }

    func base(_ service: String, _ account: String) -> [String: Any] {
        var query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecUseDataProtectionKeychain as String: true,
            kSecUseAuthenticationUI as String: kSecUseAuthenticationUIFail,
        ]
        if let accessGroup { query[kSecAttrAccessGroup as String] = accessGroup }
        return query
    }

    /// Explicit scope: `.synced` matches only synchronizable items, `.device` only local ones.
    /// Reads, presence checks, writes and deletes never widen to `kSecAttrSynchronizableAny`.
    func scoped(_ service: String, _ account: String, _ scope: AppleCredentialScope) -> [String: Any] {
        var query = base(service, account)
        query[kSecAttrSynchronizable as String] = scope == .synced
        return query
    }

    static func accessibility(for scope: AppleCredentialScope) -> CFString {
        scope == .synced ? kSecAttrAccessibleAfterFirstUnlock : kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
    }

    public func status() -> Result<String?, AppleCredentialError> {
        var query = base(AppleCredentialStore.defaultService, "status")
        query[kSecAttrSynchronizable as String] = kSecAttrSynchronizableAny
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        query[kSecReturnData as String] = false
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        if status == errSecSuccess || status == errSecItemNotFound { return .success(nil) }
        return .success(SecCopyErrorMessageString(status, nil) as String? ?? "Keychain unavailable.")
    }

    public func read(service: String, account: String, scope: AppleCredentialScope) -> Result<Data?, AppleCredentialError> {
        var query = scoped(service, account, scope)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        if status == errSecItemNotFound { return .success(nil) }
        guard status == errSecSuccess else { return .failure(.keychainStatus(status)) }
        return .success(result as? Data)
    }

    public func exists(service: String, account: String, scope: AppleCredentialScope) -> Result<Bool, AppleCredentialError> {
        var query = scoped(service, account, scope)
        query[kSecReturnData as String] = false
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        if status == errSecSuccess { return .success(true) }
        if status == errSecItemNotFound { return .success(false) }
        return .failure(.keychainStatus(status))
    }

    func probeQuery(_ service: String, _ account: String, _ scope: AppleCredentialScope?) -> [String: Any] {
        var query = base(service, account)
        if let scope {
            query[kSecAttrSynchronizable as String] = scope == .synced
        } else {
            query[kSecAttrSynchronizable as String] = kSecAttrSynchronizableAny
        }
        query[kSecUseAuthenticationUI as String] = kSecUseAuthenticationUISkip
        let context = LAContext()
        context.interactionNotAllowed = true
        query[kSecUseAuthenticationContext as String] = context
        query[kSecReturnData as String] = false
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        return query
    }

    /// Probes the caller's service/account without requesting secret bytes or presenting UI.
    /// A nil scope matches either synchronizable or device-only items in the data-protection Keychain.
    /// Items requiring authentication are skipped, so `.notFound` does not exclude protected items.
    public func probeStatus(service: String, account: String, scope: AppleCredentialScope? = nil) -> CredentialProbe {
        let query = probeQuery(service, account, scope)
        return CredentialProbe(rawStatus: SecItemCopyMatching(query as CFDictionary, nil))
    }

    public func write(_ data: Data, service: String, account: String, scope: AppleCredentialScope) -> Result<Void, AppleCredentialError> {
        var item = scoped(service, account, scope)
        item[kSecValueData as String] = data
        item[kSecAttrAccessible as String] = Self.accessibility(for: scope)
        let status = SecItemAdd(item as CFDictionary, nil)
        if status == errSecSuccess { return .success(()) }
        if status == errSecDuplicateItem {
            let update = SecItemUpdate(scoped(service, account, scope) as CFDictionary, [kSecValueData as String: data] as CFDictionary)
            return update == errSecSuccess ? .success(()) : .failure(.keychainStatus(update))
        }
        return .failure(.keychainStatus(status))
    }

    /// Legacy, intentionally broad lookup (either scope, no isolation). Never used by the scoped
    /// operations above; exists only for one-time migration code that must find an item of unknown scope.
    public func legacyReadAnyScope(service: String, account: String) -> Result<Data?, AppleCredentialError> {
        var query = base(service, account)
        query[kSecAttrSynchronizable as String] = kSecAttrSynchronizableAny
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        if status == errSecItemNotFound { return .success(nil) }
        guard status == errSecSuccess else { return .failure(.keychainStatus(status)) }
        return .success(result as? Data)
    }

    public func delete(service: String, account: String, scope: AppleCredentialScope) -> Result<Void, AppleCredentialError> {
        let query = scoped(service, account, scope)
        let status = SecItemDelete(query as CFDictionary)
        return status == errSecSuccess || status == errSecItemNotFound ? .success(()) : .failure(.keychainStatus(status))
    }

    func deletionQuery(_ service: String, _ account: String, _ scope: CredentialDeletionScope) -> [String: Any] {
        var query = base(service, account)
        switch scope {
        case .dataProtection:
            query[kSecAttrSynchronizable as String] = kSecAttrSynchronizableAny
        case .legacyFile:
            query[kSecUseDataProtectionKeychain as String] = false
            // Legacy file Keychains use ACLs, not access groups or synchronizable attributes.
            query.removeValue(forKey: kSecAttrAccessGroup as String)
        }
        let context = LAContext()
        context.interactionNotAllowed = true
        query[kSecUseAuthenticationContext as String] = context
        return query
    }

    /// Explicit deletion sweeps both sync states, plus legacy file Keychains on macOS.
    /// Every location is attempted even if another fails; absent items count as success.
    /// Legacy items match service/account only because file Keychains do not support access groups.
    public func deleteAnyScope(service: String, account: String) -> [CredentialDeletionOutcome] {
        deleteAnyScope(service: service, account: account, deleteItem: { SecItemDelete($0) })
    }

    func deleteAnyScope(service: String, account: String, deleteItem: (CFDictionary) -> OSStatus) -> [CredentialDeletionOutcome] {
        var scopes: [CredentialDeletionScope] = [.dataProtection]
#if os(macOS)
        scopes.append(.legacyFile)
#endif
        return scopes.map { scope in
            let query = deletionQuery(service, account, scope)
            return CredentialDeletionOutcome(scope: scope, rawStatus: deleteItem(query as CFDictionary))
        }
    }
}
#else
public final class SecurityAppleCredentialBackend: AppleCredentialBackend, @unchecked Sendable {
    public init(accessGroup: String? = nil) {}
    public func status() -> Result<String?, AppleCredentialError> { .success("Security.framework unavailable") }
    public func read(service: String, account: String, scope: AppleCredentialScope) -> Result<Data?, AppleCredentialError> { .failure(.unavailable("Apple Keychain unavailable on this platform.")) }
    public func exists(service: String, account: String, scope: AppleCredentialScope) -> Result<Bool, AppleCredentialError> { .failure(.unavailable("Apple Keychain unavailable on this platform.")) }
    public func write(_ data: Data, service: String, account: String, scope: AppleCredentialScope) -> Result<Void, AppleCredentialError> { .failure(.unavailable("Apple Keychain unavailable on this platform.")) }
    public func delete(service: String, account: String, scope: AppleCredentialScope) -> Result<Void, AppleCredentialError> { .failure(.unavailable("Apple Keychain unavailable on this platform.")) }
}
#endif

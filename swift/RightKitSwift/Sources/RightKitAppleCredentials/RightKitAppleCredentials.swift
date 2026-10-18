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
    /// The raw `errSecMissingEntitlement` (-34018): the process lacks the Keychain access-group entitlement.
    /// Exposed separately so the app words its own message.
    case missingEntitlement
    case keychainStatus(Int32)

    public var errorDescription: String? {
        switch self {
        case .invalidAccount: return "Credential account is invalid."
        case .invalidValue: return "Credential value is empty."
        case let .unavailable(detail): return detail
        case .missingEntitlement: return "The Keychain access-group entitlement is missing."
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
    /// Account names (no data) stored in `service` for one scope. Default: unavailable.
    func accounts(service: String, scope: AppleCredentialScope) -> Result<[String], AppleCredentialError>
    /// Synced read that also finds a synchronizable item whose sync attribute cache has not settled
    /// (`kSecAttrSynchronizableAny` in the Keychain backend). Must never return a device-only item.
    /// Default: the plain `.synced` read.
    func readBroadSynced(service: String, account: String) -> Result<Data?, AppleCredentialError>
    /// Passive status probe of `service`. Default: ``status()``.
    func status(service: String) -> Result<String?, AppleCredentialError>
}

public extension AppleCredentialBackend {
    func accounts(service: String, scope: AppleCredentialScope) -> Result<[String], AppleCredentialError> {
        .failure(.unavailable("This credential backend cannot enumerate accounts."))
    }
    func readBroadSynced(service: String, account: String) -> Result<Data?, AppleCredentialError> {
        read(service: service, account: account, scope: .synced)
    }
    func status(service: String) -> Result<String?, AppleCredentialError> { status() }
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

    public func accounts(service: String, scope: AppleCredentialScope) -> Result<[String], AppleCredentialError> {
        let prefix = "\(service)\u{1f}"
        let suffix = "\u{1f}\(scope.rawValue)"
        lock.lock(); defer { lock.unlock() }
        let names = values.keys.compactMap { k -> String? in
            guard k.hasPrefix(prefix), k.hasSuffix(suffix), k.count > prefix.count + suffix.count else { return nil }
            return String(k.dropFirst(prefix.count).dropLast(suffix.count))
        }
        return .success(names.sorted())
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
    private let accountValidator: (@Sendable (String) -> Bool)?

    /// Accepts any non-empty account of at most 1024 UTF-8 bytes without control characters, such as
    /// `daemon-token:<url>`. Pass as `accountValidator` to lift the default `[A-Za-z0-9._-]` restriction.
    public static let arbitraryAccounts: @Sendable (String) -> Bool = { account in
        !account.isEmpty && account.utf8.count <= 1024 && account.unicodeScalars.allSatisfy { $0.value >= 0x20 && $0.value != 0x7f }
    }

    /// - Parameter accountValidator: replaces the default account rule (`[A-Za-z0-9._-]`, at most 200 bytes).
    ///   An empty account is always invalid. Use ``arbitraryAccounts`` for URL-shaped accounts.
    public init(
        service: String = AppleCredentialStore.defaultService,
        accessGroup: String? = nil,
        backend: AppleCredentialBackend? = nil,
        accountValidator: (@Sendable (String) -> Bool)? = nil
    ) {
        self.service = service
        self.accessGroup = accessGroup
        self.accountValidator = accountValidator
        self.backend = backend ?? SecurityAppleCredentialBackend(accessGroup: accessGroup)
    }

    /// Passive probe of this store's own `service`. A missing access-group entitlement is returned as
    /// ``AppleCredentialError/missingEntitlement`` so the app can word its own message.
    public func status() -> Result<(ready: Bool, detail: String?), AppleCredentialError> {
        switch backend.status(service: service) {
        case let .success(detail): return .success((detail == nil, detail))
        case let .failure(error): return .failure(error)
        }
    }

    /// Reads data only on explicit request. `exists` below is the passive path.
    /// - Parameter broadSynced: for `.synced` only, opt in to the broad synchronizable lookup that finds an
    ///   item whose sync attribute cache has not settled (right after another device minted it). It still
    ///   never returns a device-only item. Ignored for `.device`.
    public func read(_ account: String, scope: AppleCredentialScope, broadSynced: Bool = false) -> Result<String?, AppleCredentialError> {
        switch readData(account, scope: scope, broadSynced: broadSynced) {
        case let .success(data):
            guard let data else { return .success(nil) }
            return .success(String(data: data, encoding: .utf8))
        case let .failure(error): return .failure(error)
        }
    }

    /// Raw bytes exactly as stored: no decoding, no trimming. See ``read(_:scope:broadSynced:)``.
    public func readData(_ account: String, scope: AppleCredentialScope, broadSynced: Bool = false) -> Result<Data?, AppleCredentialError> {
        guard validAccount(account) else { return .failure(.invalidAccount) }
        if broadSynced && scope == .synced { return backend.readBroadSynced(service: service, account: account) }
        return backend.read(service: service, account: account, scope: scope)
    }

    /// Raw bytes written exactly as given: no trimming or re-encoding. Empty data is `invalidValue`.
    public func writeData(_ data: Data, account: String, scope: AppleCredentialScope) -> Result<Void, AppleCredentialError> {
        guard validAccount(account) else { return .failure(.invalidAccount) }
        guard !data.isEmpty else { return .failure(.invalidValue) }
        return backend.write(data, service: service, account: account, scope: scope)
    }

    /// Deletes one scope's copy only (`deleteEverywhere` removes both). Absent items succeed.
    public func delete(_ account: String, scope: AppleCredentialScope) -> Result<Void, AppleCredentialError> {
        guard validAccount(account) else { return .failure(.invalidAccount) }
        return backend.delete(service: service, account: account, scope: scope)
    }

    /// Account names (no data) held in this store's service for `scope`, sorted. Reads no secret bytes.
    public func accounts(scope: AppleCredentialScope) -> Result<[String], AppleCredentialError> {
        backend.accounts(service: service, scope: scope)
    }

    /// Presence check never asks Keychain for secret bytes and never prompts.
    public func exists(_ account: String, scope: AppleCredentialScope) -> Result<Bool, AppleCredentialError> {
        guard validAccount(account) else { return .failure(.invalidAccount) }
        return backend.exists(service: service, account: account, scope: scope)
    }

    public func write(_ value: String, account: String, scope: AppleCredentialScope) -> Result<Void, AppleCredentialError> {
        guard validAccount(account) else { return .failure(.invalidAccount) }
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return .failure(.invalidValue) }
        return backend.write(Data(trimmed.utf8), service: service, account: account, scope: scope)
    }

    /// Explicit user deletion removes both copies. Callers must not use this for a sync toggle.
    public func deleteEverywhere(_ account: String) -> Result<Void, AppleCredentialError> {
        guard validAccount(account) else { return .failure(.invalidAccount) }
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

    private func validAccount(_ account: String) -> Bool {
        if let accountValidator { return !account.isEmpty && accountValidator(account) }
        return Self.defaultValidAccount(account)
    }

    private static func defaultValidAccount(_ account: String) -> Bool {
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
        // Non-interactive: a protected item fails with `errSecInteractionNotAllowed` instead of
        // prompting. `kSecUseAuthenticationUIFail` is deprecated; this is its documented replacement.
        let context = LAContext()
        context.interactionNotAllowed = true
        var query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecUseDataProtectionKeychain as String: true,
            kSecUseAuthenticationContext as String: context,
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

    static func error(forStatus status: OSStatus) -> AppleCredentialError {
        status == errSecMissingEntitlement ? .missingEntitlement : .keychainStatus(status)
    }

    public func status() -> Result<String?, AppleCredentialError> { status(service: AppleCredentialStore.defaultService) }

    /// Passive probe of `service` (account `status`, either scope). A missing entitlement is a failure
    /// (``AppleCredentialError/missingEntitlement``); any other unusable status is a detail string.
    public func status(service: String) -> Result<String?, AppleCredentialError> {
        var query = base(service, "status")
        query[kSecAttrSynchronizable as String] = kSecAttrSynchronizableAny
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        query[kSecReturnData as String] = false
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        if status == errSecSuccess || status == errSecItemNotFound { return .success(nil) }
        if status == errSecMissingEntitlement { return .failure(.missingEntitlement) }
        return .success(SecCopyErrorMessageString(status, nil) as String? ?? "Keychain unavailable.")
    }

    public func read(service: String, account: String, scope: AppleCredentialScope) -> Result<Data?, AppleCredentialError> {
        var query = scoped(service, account, scope)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        if status == errSecItemNotFound { return .success(nil) }
        guard status == errSecSuccess else { return .failure(Self.error(forStatus: status)) }
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
        return .failure(Self.error(forStatus: status))
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
            return update == errSecSuccess ? .success(()) : .failure(Self.error(forStatus: update))
        }
        return .failure(Self.error(forStatus: status))
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
        guard status == errSecSuccess else { return .failure(Self.error(forStatus: status)) }
        return .success(result as? Data)
    }

    /// Broad synced read: `kSecAttrSynchronizableAny` so an item whose sync attribute cache has not settled
    /// is still found, then only a row whose `kSecAttrSynchronizable` is true is accepted. A same-account
    /// device-only item can therefore never be returned for a synced read.
    public func readBroadSynced(service: String, account: String) -> Result<Data?, AppleCredentialError> {
        var query = base(service, account)
        query[kSecAttrSynchronizable as String] = kSecAttrSynchronizableAny
        query[kSecReturnData as String] = true
        query[kSecReturnAttributes as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitAll
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        if status == errSecItemNotFound { return .success(nil) }
        guard status == errSecSuccess else { return .failure(Self.error(forStatus: status)) }
        guard let rows = result as? [[String: Any]] else { return .failure(.keychainStatus(errSecDecode)) }
        for row in rows where Self.isSynchronizable(row[kSecAttrSynchronizable as String]) {
            if let data = row[kSecValueData as String] as? Data { return .success(data) }
        }
        return .success(nil)
    }

    static func isSynchronizable(_ value: Any?) -> Bool {
        if let flag = value as? Bool { return flag }
        if let number = value as? NSNumber { return number.boolValue }
        return false
    }

    /// Attributes-only enumeration of account names for one scope; no secret bytes are requested.
    public func accounts(service: String, scope: AppleCredentialScope) -> Result<[String], AppleCredentialError> {
        var query = base(service, "")
        query.removeValue(forKey: kSecAttrAccount as String)
        query[kSecAttrSynchronizable as String] = scope == .synced
        query[kSecReturnAttributes as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitAll
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        if status == errSecItemNotFound { return .success([]) }
        guard status == errSecSuccess else { return .failure(Self.error(forStatus: status)) }
        guard let rows = result as? [[String: Any]] else { return .failure(.keychainStatus(errSecDecode)) }
        return .success(rows.compactMap { $0[kSecAttrAccount as String] as? String }.sorted())
    }

    public func delete(service: String, account: String, scope: AppleCredentialScope) -> Result<Void, AppleCredentialError> {
        let query = scoped(service, account, scope)
        let status = SecItemDelete(query as CFDictionary)
        return status == errSecSuccess || status == errSecItemNotFound ? .success(()) : .failure(Self.error(forStatus: status))
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

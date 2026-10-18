import Foundation
import Security

// Keychain-backed secret store: the Swift counterpart of the Rust owner `rightkit-credentials`.
// namespace = kSecAttrService, key = kSecAttrAccount. There is no plaintext or file fallback:
// a store that cannot reach the Keychain throws, or (only when a guard variable trips) uses a
// process-wide in-memory store that never touches disk.

public enum SecretsError: Error, Equatable, Sendable {
    /// Empty namespace/key, empty value, or a key that names a rotation marker.
    case invalid(String)
    /// The Keychain refused or failed the operation (raw `OSStatus`).
    case keychain(OSStatus)
    /// The store is guarded in `.deny` mode by the named environment variable.
    case guarded(String)
}

/// What a tripped guard variable does. Neither touches the Keychain.
public enum SecretsGuardMode: Sendable, Equatable {
    /// Swap in a process-wide in-memory store.
    case memory
    /// Every operation throws ``SecretsError/guarded(_:)``.
    case deny
}

public enum SecretsAccessibility: Sendable, Equatable {
    case afterFirstUnlockThisDeviceOnly
    case whenUnlockedThisDeviceOnly
    case afterFirstUnlock

    var attribute: CFString {
        switch self {
        case .afterFirstUnlockThisDeviceOnly: return kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        case .whenUnlockedThisDeviceOnly: return kSecAttrAccessibleWhenUnlockedThisDeviceOnly
        case .afterFirstUnlock: return kSecAttrAccessibleAfterFirstUnlock
        }
    }
}

/// Storage seam. `add` is create-only and returns `false` when the item exists; the Keychain
/// implementation maps that onto `errSecDuplicateItem`, which is what makes it atomic.
public protocol SecretsBackend: Sendable {
    func get(namespace: String, key: String) throws -> Data?
    func add(_ value: Data, namespace: String, key: String) throws -> Bool
    func update(_ value: Data, namespace: String, key: String) throws -> Bool
    func delete(namespace: String, key: String) throws -> Bool
    /// Update-only rewrite of the accessibility attribute. `false` when the item is absent.
    func reapplyAccessibility(namespace: String, key: String) throws -> Bool
    /// Account names (no data) stored in `namespace`, in no particular order. Includes rotation markers;
    /// ``SecretsStore/keys(prefix:)`` hides them. The default implementation throws, so existing
    /// third-party conformers keep compiling.
    func list(namespace: String) throws -> [String]
    /// Create-only write that also stores a small non-secret `tag` beside the value.
    func add(_ value: Data, namespace: String, key: String, tag: String) throws -> Bool
    /// Update-only rewrite of the value and the non-secret `tag`.
    func update(_ value: Data, namespace: String, key: String, tag: String) throws -> Bool
    /// The non-secret tag stored with the item, read without the secret value. `nil` when the item has no tag or is absent.
    func tag(namespace: String, key: String) throws -> String?
}

public extension SecretsBackend {
    func list(namespace: String) throws -> [String] { throw SecretsError.invalid("backend does not support listing") }
    func add(_ value: Data, namespace: String, key: String, tag: String) throws -> Bool {
        throw SecretsError.invalid("backend does not support tags")
    }
    func update(_ value: Data, namespace: String, key: String, tag: String) throws -> Bool {
        throw SecretsError.invalid("backend does not support tags")
    }
    func tag(namespace: String, key: String) throws -> String? { throw SecretsError.invalid("backend does not support tags") }
}

public struct KeychainSecretsBackend: SecretsBackend {
    public let accessGroup: String?
    public let accessibility: SecretsAccessibility

    public init(accessGroup: String? = nil, accessibility: SecretsAccessibility = .afterFirstUnlockThisDeviceOnly) {
        self.accessGroup = accessGroup
        self.accessibility = accessibility
    }

    /// Identity query: never synchronizable, scoped to the access group when one is configured, and
    /// always on the data-protection keychain. On macOS the legacy file keychain ignores access groups and
    /// accessibility classes, so the flag is required for both to take effect. Every platform this package
    /// supports (macOS 14+, iOS 17+) has the flag, so it is set unconditionally.
    public func identity(namespace: String, key: String) -> [String: Any] {
        var q: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: namespace,
            kSecAttrAccount as String: key,
            kSecAttrSynchronizable as String: false,
            kSecUseDataProtectionKeychain as String: true,
        ]
        if let accessGroup { q[kSecAttrAccessGroup as String] = accessGroup }
        return q
    }

    /// Pre-M11 items were written without the data-protection flag, so they live in the legacy file keychain
    /// (macOS only) where the flagged query cannot see them. Same attributes, flag removed.
    func legacyIdentity(namespace: String, key: String) -> [String: Any] {
        var q = identity(namespace: namespace, key: key)
        q.removeValue(forKey: kSecUseDataProtectionKeychain as String)
        return q
    }

    /// One-time migration of a legacy file-keychain item into the data-protection keychain (macOS only).
    /// Runs before every operation, so a legacy copy can never shadow a newer value or come back after a
    /// delete. The legacy item is removed only after the data-protection copy exists, or already held a value.
    /// On iOS there is no separate legacy store, and the legacy query would hit the same items, so it is skipped.
    func migrateLegacy(namespace: String, key: String) throws {
        #if os(macOS)
        var legacy = legacyIdentity(namespace: namespace, key: key)
        legacy[kSecReturnData as String] = true
        legacy[kSecReturnAttributes as String] = true
        legacy[kSecMatchLimit as String] = kSecMatchLimitOne
        var item: CFTypeRef?
        let found = SecItemCopyMatching(legacy as CFDictionary, &item)
        switch found {
        case errSecItemNotFound: return
        case errSecSuccess: break
        default: throw SecretsError.keychain(found)
        }
        guard let row = item as? [String: Any], let data = row[kSecValueData as String] as? Data else {
            throw SecretsError.keychain(errSecDecode)
        }
        var copy = identity(namespace: namespace, key: key)
        copy[kSecValueData as String] = data
        if let generic = row[kSecAttrGeneric as String] as? Data { copy[kSecAttrGeneric as String] = generic }
        copy[kSecAttrAccessible as String] = accessibility.attribute
        let added = SecItemAdd(copy as CFDictionary, nil)
        guard added == errSecSuccess || added == errSecDuplicateItem else { throw SecretsError.keychain(added) }
        let removed = SecItemDelete(legacyIdentity(namespace: namespace, key: key) as CFDictionary)
        guard removed == errSecSuccess || removed == errSecItemNotFound else { throw SecretsError.keychain(removed) }
        #endif
    }

    public func get(namespace: String, key: String) throws -> Data? {
        try migrateLegacy(namespace: namespace, key: key)
        var q = identity(namespace: namespace, key: key)
        q[kSecReturnData as String] = true
        q[kSecMatchLimit as String] = kSecMatchLimitOne
        var item: CFTypeRef?
        let status = SecItemCopyMatching(q as CFDictionary, &item)
        switch status {
        case errSecSuccess:
            guard let data = item as? Data else { throw SecretsError.keychain(errSecDecode) }
            return data
        case errSecItemNotFound: return nil
        default: throw SecretsError.keychain(status)   // a read failure is never "absent"
        }
    }

    public func add(_ value: Data, namespace: String, key: String) throws -> Bool {
        try migrateLegacy(namespace: namespace, key: key)
        var item = identity(namespace: namespace, key: key)
        item[kSecValueData as String] = value
        item[kSecAttrAccessible as String] = accessibility.attribute
        let status = SecItemAdd(item as CFDictionary, nil)
        switch status {
        case errSecSuccess: return true
        case errSecDuplicateItem: return false
        default: throw SecretsError.keychain(status)
        }
    }

    public func update(_ value: Data, namespace: String, key: String) throws -> Bool {
        try migrateLegacy(namespace: namespace, key: key)
        let attrs: [String: Any] = [
            kSecValueData as String: value,
            kSecAttrAccessible as String: accessibility.attribute,
        ]
        let status = SecItemUpdate(identity(namespace: namespace, key: key) as CFDictionary, attrs as CFDictionary)
        switch status {
        case errSecSuccess: return true
        case errSecItemNotFound: return false
        default: throw SecretsError.keychain(status)
        }
    }

    public func delete(namespace: String, key: String) throws -> Bool {
        try migrateLegacy(namespace: namespace, key: key)
        let status = SecItemDelete(identity(namespace: namespace, key: key) as CFDictionary)
        switch status {
        case errSecSuccess: return true
        case errSecItemNotFound: return false
        default: throw SecretsError.keychain(status)
        }
    }

    public func reapplyAccessibility(namespace: String, key: String) throws -> Bool {
        try migrateLegacy(namespace: namespace, key: key)
        let attrs: [String: Any] = [kSecAttrAccessible as String: accessibility.attribute]
        let status = SecItemUpdate(identity(namespace: namespace, key: key) as CFDictionary, attrs as CFDictionary)
        switch status {
        case errSecSuccess: return true
        case errSecItemNotFound: return false
        default: throw SecretsError.keychain(status)
        }
    }

    public func add(_ value: Data, namespace: String, key: String, tag: String) throws -> Bool {
        try migrateLegacy(namespace: namespace, key: key)
        var item = identity(namespace: namespace, key: key)
        item[kSecValueData as String] = value
        item[kSecAttrGeneric as String] = Data(tag.utf8)
        item[kSecAttrAccessible as String] = accessibility.attribute
        let status = SecItemAdd(item as CFDictionary, nil)
        switch status {
        case errSecSuccess: return true
        case errSecDuplicateItem: return false
        default: throw SecretsError.keychain(status)
        }
    }

    public func update(_ value: Data, namespace: String, key: String, tag: String) throws -> Bool {
        try migrateLegacy(namespace: namespace, key: key)
        let attrs: [String: Any] = [
            kSecValueData as String: value,
            kSecAttrGeneric as String: Data(tag.utf8),
            kSecAttrAccessible as String: accessibility.attribute,
        ]
        let status = SecItemUpdate(identity(namespace: namespace, key: key) as CFDictionary, attrs as CFDictionary)
        switch status {
        case errSecSuccess: return true
        case errSecItemNotFound: return false
        default: throw SecretsError.keychain(status)
        }
    }

    /// Reads `kSecAttrGeneric` through an attributes-only query, so the secret value is never fetched.
    public func tag(namespace: String, key: String) throws -> String? {
        try migrateLegacy(namespace: namespace, key: key)
        var q = identity(namespace: namespace, key: key)
        q[kSecReturnAttributes as String] = true
        q[kSecMatchLimit as String] = kSecMatchLimitOne
        var item: CFTypeRef?
        let status = SecItemCopyMatching(q as CFDictionary, &item)
        switch status {
        case errSecSuccess:
            guard let row = item as? [String: Any] else { throw SecretsError.keychain(errSecDecode) }
            guard let generic = row[kSecAttrGeneric as String] as? Data else { return nil }
            return String(data: generic, encoding: .utf8)
        case errSecItemNotFound: return nil
        default: throw SecretsError.keychain(status)
        }
    }

    /// Attributes-only enumeration of the account names in `namespace` (no secret bytes are read). On
    /// macOS the legacy file keychain is listed too, so items not yet migrated by a per-key operation appear.
    public func list(namespace: String) throws -> [String] {
        var names = try listAccounts(identity(namespace: namespace, key: ""), namespace: namespace)
        #if os(macOS)
        var legacy = identity(namespace: namespace, key: "")
        legacy.removeValue(forKey: kSecUseDataProtectionKeychain as String)
        names.formUnion(try listAccounts(legacy, namespace: namespace))
        #endif
        return names.sorted()
    }

    private func listAccounts(_ base: [String: Any], namespace: String) throws -> Set<String> {
        var q = base
        q.removeValue(forKey: kSecAttrAccount as String)
        q[kSecReturnAttributes as String] = true
        q[kSecMatchLimit as String] = kSecMatchLimitAll
        var item: CFTypeRef?
        let status = SecItemCopyMatching(q as CFDictionary, &item)
        switch status {
        case errSecItemNotFound: return []
        case errSecSuccess:
            guard let rows = item as? [[String: Any]] else { throw SecretsError.keychain(errSecDecode) }
            return Set(rows.compactMap { $0[kSecAttrAccount as String] as? String })
        default: throw SecretsError.keychain(status)
        }
    }
}

/// In-memory backend: tests and the guard `.memory` mode. Never persists.
public final class MemorySecretsBackend: SecretsBackend, @unchecked Sendable {
    private let lock = NSLock()
    private var values: [String: Data] = [:]
    private var tags: [String: String] = [:]
    public init() {}
    /// Shared by every store whose guard trips in `.memory` mode, like the Rust process-wide backend.
    public static let processWide = MemorySecretsBackend()

    private func k(_ n: String, _ key: String) -> String { "\(n)\u{1f}\(key)" }
    public func get(namespace: String, key: String) throws -> Data? { lock.withLock { values[k(namespace, key)] } }
    public func add(_ value: Data, namespace: String, key: String) throws -> Bool {
        lock.withLock {
            if values[k(namespace, key)] != nil { return false }
            values[k(namespace, key)] = value; return true
        }
    }
    public func update(_ value: Data, namespace: String, key: String) throws -> Bool {
        lock.withLock {
            guard values[k(namespace, key)] != nil else { return false }
            values[k(namespace, key)] = value; return true
        }
    }
    public func delete(namespace: String, key: String) throws -> Bool {
        lock.withLock { tags.removeValue(forKey: k(namespace, key)); return values.removeValue(forKey: k(namespace, key)) != nil }
    }
    public func reapplyAccessibility(namespace: String, key: String) throws -> Bool {
        lock.withLock { values[k(namespace, key)] != nil }
    }
    public func add(_ value: Data, namespace: String, key: String, tag: String) throws -> Bool {
        lock.withLock {
            if values[k(namespace, key)] != nil { return false }
            values[k(namespace, key)] = value; tags[k(namespace, key)] = tag; return true
        }
    }
    public func update(_ value: Data, namespace: String, key: String, tag: String) throws -> Bool {
        lock.withLock {
            guard values[k(namespace, key)] != nil else { return false }
            values[k(namespace, key)] = value; tags[k(namespace, key)] = tag; return true
        }
    }
    public func tag(namespace: String, key: String) throws -> String? { lock.withLock { tags[k(namespace, key)] } }
    public func list(namespace: String) throws -> [String] {
        let prefix = "\(namespace)\u{1f}"
        return lock.withLock { values.keys.filter { $0.hasPrefix(prefix) }.map { String($0.dropFirst(prefix.count)) }.sorted() }
    }
}

private struct DeniedBackend: SecretsBackend {
    let variable: String
    func get(namespace: String, key: String) throws -> Data? { throw SecretsError.guarded(variable) }
    func add(_ value: Data, namespace: String, key: String) throws -> Bool { throw SecretsError.guarded(variable) }
    func update(_ value: Data, namespace: String, key: String) throws -> Bool { throw SecretsError.guarded(variable) }
    func delete(namespace: String, key: String) throws -> Bool { throw SecretsError.guarded(variable) }
    func reapplyAccessibility(namespace: String, key: String) throws -> Bool { throw SecretsError.guarded(variable) }
    func list(namespace: String) throws -> [String] { throw SecretsError.guarded(variable) }
    func add(_ value: Data, namespace: String, key: String, tag: String) throws -> Bool { throw SecretsError.guarded(variable) }
    func update(_ value: Data, namespace: String, key: String, tag: String) throws -> Bool { throw SecretsError.guarded(variable) }
    func tag(namespace: String, key: String) throws -> String? { throw SecretsError.guarded(variable) }
}

/// Universal guard variable honoured by every app, like the Rust owner.
public let secretsUniversalGuardEnv = "RIGHTKIT_NO_KEYCHAIN"
/// Suffix of the pending-rotation marker item; identical to the Rust owner.
public let secretsRotationPendingSuffix = ".rotation-pending"

public struct SecretsStore: Sendable {
    public let namespace: String
    public let accessGroup: String?
    /// Name of the guard variable that tripped, if any.
    public let guardActive: String?
    private let backend: SecretsBackend

    /// - Parameters:
    ///   - namespace: Keychain service name.
    ///   - accessGroup: Keychain access group (App Group / shared keychain group), or nil for the app's own.
    ///   - guardEnv: app-specific guard variables, checked in addition to `RIGHTKIT_NO_KEYCHAIN`.
    ///   - backend: replaces the Keychain (tests). When set, guards still apply.
    public init(
        namespace: String,
        accessGroup: String? = nil,
        accessibility: SecretsAccessibility = .afterFirstUnlockThisDeviceOnly,
        guardEnv: [String] = [],
        guardMode: SecretsGuardMode = .memory,
        environment: [String: String] = ProcessInfo.processInfo.environment,
        backend: SecretsBackend? = nil
    ) throws {
        guard !namespace.isEmpty else { throw SecretsError.invalid("namespace is empty") }
        self.namespace = namespace
        self.accessGroup = accessGroup
        let tripped = ([secretsUniversalGuardEnv] + guardEnv).first { name in
            guard let v = environment[name] else { return false }
            return !v.isEmpty && v != "0"
        }
        if let tripped {
            guardActive = tripped
            self.backend = guardMode == .memory ? MemorySecretsBackend.processWide : DeniedBackend(variable: tripped)
        } else {
            guardActive = nil
            self.backend = backend ?? KeychainSecretsBackend(accessGroup: accessGroup, accessibility: accessibility)
        }
    }

    /// True when values survive the process (the Keychain, not a guard's memory store).
    public var isDurable: Bool { guardActive == nil && !(backend is MemorySecretsBackend) }

    private func check(_ key: String) throws {
        guard !key.isEmpty else { throw SecretsError.invalid("key is empty") }
    }

    /// `nil` only on authoritative absence; any other failure throws.
    public func get(_ key: String) throws -> Data? {
        try check(key); return try backend.get(namespace: namespace, key: key)
    }

    public func getString(_ key: String) throws -> String? {
        guard let data = try get(key) else { return nil }
        guard let s = String(data: data, encoding: .utf8) else { throw SecretsError.invalid("value is not UTF-8") }
        return s
    }

    public func exists(_ key: String) throws -> Bool { try get(key) != nil }

    /// Create or overwrite. Update-then-add; when a concurrent writer wins the add, update again.
    public func set(_ key: String, _ value: Data) throws {
        try check(key)
        guard !value.isEmpty else { throw SecretsError.invalid("value is empty") }
        for _ in 0..<3 {
            if try backend.update(value, namespace: namespace, key: key) { return }
            if try backend.add(value, namespace: namespace, key: key) { return }
        }
        throw SecretsError.keychain(errSecDuplicateItem)
    }

    public func setString(_ key: String, _ value: String) throws { try set(key, Data(value.utf8)) }

    /// Create or overwrite, storing a small non-secret `tag` (Keychain `kSecAttrGeneric`) beside the value.
    /// Read it back without the value through ``tag(_:)``. Overwriting with the untagged ``set(_:_:)``
    /// leaves an existing tag unchanged.
    public func set(_ key: String, _ value: Data, tag: String) throws {
        try check(key)
        guard !value.isEmpty else { throw SecretsError.invalid("value is empty") }
        for _ in 0..<3 {
            if try backend.update(value, namespace: namespace, key: key, tag: tag) { return }
            if try backend.add(value, namespace: namespace, key: key, tag: tag) { return }
        }
        throw SecretsError.keychain(errSecDuplicateItem)
    }

    public func setString(_ key: String, _ value: String, tag: String) throws { try set(key, Data(value.utf8), tag: tag) }

    /// The non-secret tag stored with `key`, read without the secret value; `nil` when absent or untagged.
    public func tag(_ key: String) throws -> String? {
        try check(key); return try backend.tag(namespace: namespace, key: key)
    }

    /// The keys (account names, never values) stored in this namespace, sorted, optionally only those
    /// starting with `prefix`. Pending-rotation markers are not listed.
    public func keys(prefix: String? = nil) throws -> [String] {
        try backend.list(namespace: namespace)
            .filter { name in
                guard !name.hasSuffix(secretsRotationPendingSuffix) else { return false }
                guard let prefix else { return true }
                return name.hasPrefix(prefix)
            }
            .sorted()
    }

    /// Removes the item; returns whether it existed.
    @discardableResult
    public func delete(_ key: String) throws -> Bool {
        try check(key); return try backend.delete(namespace: namespace, key: key)
    }

    /// Create-only and atomic (`SecItemAdd` fails with `errSecDuplicateItem` for an existing item).
    /// `false` when the key already holds a value; the existing value is untouched.
    public func setIfAbsent(_ key: String, _ value: Data) throws -> Bool {
        try check(key)
        guard !value.isEmpty else { throw SecretsError.invalid("value is empty") }
        return try backend.add(value, namespace: namespace, key: key)
    }

    public func setIfAbsent(_ key: String, string: String) throws -> Bool { try setIfAbsent(key, Data(string.utf8)) }

    /// Update-only rewrite of accessibility so older records migrate in place; never resurrects a deleted item.
    @discardableResult
    public func reapplyAccessibility(_ key: String) throws -> Bool {
        try check(key); return try backend.reapplyAccessibility(namespace: namespace, key: key)
    }

    // MARK: Pending-rotation recovery (same sequence as the Rust owner)

    private func marker(_ key: String) -> String { key + secretsRotationPendingSuffix }

    /// Record `new` as the pending value. `false` when a rotation is already pending.
    public func beginRotation(_ key: String, new: Data) throws -> Bool {
        if key.hasSuffix(secretsRotationPendingSuffix) { throw SecretsError.invalid("key names a rotation marker") }
        return try setIfAbsent(marker(key), new)
    }

    public func pendingRotation(_ key: String) throws -> Data? {
        guard let d = try get(marker(key)), !d.isEmpty else { return nil }
        return d
    }

    /// Make the pending value the credential, then remove the marker. `false` when nothing was pending.
    public func commitRotation(_ key: String) throws -> Bool {
        guard let new = try pendingRotation(key) else { return false }
        try set(key, new)
        try delete(marker(key))
        return true
    }

    /// Discard the pending value, leaving the credential unchanged. `true` if a marker was removed.
    @discardableResult
    public func abortRotation(_ key: String) throws -> Bool { try delete(marker(key)) }
}

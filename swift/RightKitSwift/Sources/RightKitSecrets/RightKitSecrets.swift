import Foundation
import Security

// Keychain-backed secret store: the Swift counterpart of the Rust owner `rightkit-secrets`.
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
}

public struct KeychainSecretsBackend: SecretsBackend {
    public let accessGroup: String?
    public let accessibility: SecretsAccessibility

    public init(accessGroup: String? = nil, accessibility: SecretsAccessibility = .afterFirstUnlockThisDeviceOnly) {
        self.accessGroup = accessGroup
        self.accessibility = accessibility
    }

    /// Identity query: never synchronizable, scoped to the access group when one is configured.
    public func identity(namespace: String, key: String) -> [String: Any] {
        var q: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: namespace,
            kSecAttrAccount as String: key,
            kSecAttrSynchronizable as String: false,
        ]
        if let accessGroup { q[kSecAttrAccessGroup as String] = accessGroup }
        return q
    }

    public func get(namespace: String, key: String) throws -> Data? {
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
        let status = SecItemDelete(identity(namespace: namespace, key: key) as CFDictionary)
        switch status {
        case errSecSuccess: return true
        case errSecItemNotFound: return false
        default: throw SecretsError.keychain(status)
        }
    }

    public func reapplyAccessibility(namespace: String, key: String) throws -> Bool {
        let attrs: [String: Any] = [kSecAttrAccessible as String: accessibility.attribute]
        let status = SecItemUpdate(identity(namespace: namespace, key: key) as CFDictionary, attrs as CFDictionary)
        switch status {
        case errSecSuccess: return true
        case errSecItemNotFound: return false
        default: throw SecretsError.keychain(status)
        }
    }
}

/// In-memory backend: tests and the guard `.memory` mode. Never persists.
public final class MemorySecretsBackend: SecretsBackend, @unchecked Sendable {
    private let lock = NSLock()
    private var values: [String: Data] = [:]
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
        lock.withLock { values.removeValue(forKey: k(namespace, key)) != nil }
    }
    public func reapplyAccessibility(namespace: String, key: String) throws -> Bool {
        lock.withLock { values[k(namespace, key)] != nil }
    }
}

private struct DeniedBackend: SecretsBackend {
    let variable: String
    func get(namespace: String, key: String) throws -> Data? { throw SecretsError.guarded(variable) }
    func add(_ value: Data, namespace: String, key: String) throws -> Bool { throw SecretsError.guarded(variable) }
    func update(_ value: Data, namespace: String, key: String) throws -> Bool { throw SecretsError.guarded(variable) }
    func delete(namespace: String, key: String) throws -> Bool { throw SecretsError.guarded(variable) }
    func reapplyAccessibility(namespace: String, key: String) throws -> Bool { throw SecretsError.guarded(variable) }
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

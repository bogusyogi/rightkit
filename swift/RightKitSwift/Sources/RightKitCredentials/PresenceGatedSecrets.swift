import Foundation
import Security
#if canImport(LocalAuthentication)
import LocalAuthentication

/// When a presence-gated Keychain item can be read. Both are this-device-only and never in a backup.
public enum SecretsPresenceAccessibility: Sendable, Equatable {
    /// Only while a passcode is set (an iPhone with no passcode cannot hold the item). The default.
    case whenPasscodeSetThisDeviceOnly
    case whenUnlockedThisDeviceOnly

    var attribute: CFString {
        switch self {
        case .whenPasscodeSetThisDeviceOnly: kSecAttrAccessibleWhenPasscodeSetThisDeviceOnly
        case .whenUnlockedThisDeviceOnly: kSecAttrAccessibleWhenUnlockedThisDeviceOnly
        }
    }
}

/// A ``SecretsBackend`` that can keep an item behind the owner's presence (biometry or the device
/// passcode): the item carries `kSecAttrAccessControl` (`.userPresence`) instead of
/// `kSecAttrAccessible`, is never synchronizable, and a read goes through the caller's `LAContext`,
/// so one prompt can cover the item and a Secure Enclave key (``OwnerKey``) used with the same context.
/// Only the Keychain backend conforms: a memory or denied backend has no presence check to offer.
public protocol PresenceGatedSecretsBackend: SecretsBackend {
    /// Creates or replaces the item behind `.userPresence`. Replacing removes the old item first
    /// (removing needs no prompt).
    func setPresenceGated(_ value: Data, namespace: String, key: String,
                          accessibility: SecretsPresenceAccessibility) throws
    /// Reads the item with `context`; the system asks the owner unless the context already
    /// authenticated. `nil` only when the item is absent. A refusal or a cancelled prompt throws
    /// ``SecretsError/keychain(_:)`` with the status (`errSecUserCanceled`, `errSecAuthFailed`,
    /// `errSecInteractionNotAllowed`).
    func getPresenceGated(namespace: String, key: String, context: LAContext) throws -> Data?
}

extension KeychainSecretsBackend: PresenceGatedSecretsBackend {
    public func setPresenceGated(_ value: Data, namespace: String, key: String,
                                 accessibility presenceAccessibility: SecretsPresenceAccessibility) throws {
        try migrateLegacy(namespace: namespace, key: key)
        var error: Unmanaged<CFError>?
        guard let control = SecAccessControlCreateWithFlags(
            nil, presenceAccessibility.attribute, .userPresence, &error)
        else {
            error?.release()
            throw SecretsError.invalid("a presence-gated item cannot be made on this device (is a passcode set?)")
        }
        var item = identity(namespace: namespace, key: key)
        item[kSecValueData as String] = value
        item[kSecAttrAccessControl as String] = control
        var status = SecItemAdd(item as CFDictionary, nil)
        if status == errSecDuplicateItem {
            let removed = SecItemDelete(identity(namespace: namespace, key: key) as CFDictionary)
            guard removed == errSecSuccess || removed == errSecItemNotFound else {
                throw SecretsError.keychain(removed)
            }
            status = SecItemAdd(item as CFDictionary, nil)
        }
        guard status == errSecSuccess else { throw SecretsError.keychain(status) }
    }

    public func getPresenceGated(namespace: String, key: String, context: LAContext) throws -> Data? {
        try migrateLegacy(namespace: namespace, key: key)
        var q = identity(namespace: namespace, key: key)
        q[kSecReturnData as String] = true
        q[kSecMatchLimit as String] = kSecMatchLimitOne
        q[kSecUseAuthenticationContext as String] = context
        var item: CFTypeRef?
        let status = SecItemCopyMatching(q as CFDictionary, &item)
        switch status {
        case errSecSuccess:
            guard let data = item as? Data else { throw SecretsError.keychain(errSecDecode) }
            return data
        case errSecItemNotFound: return nil
        default: throw SecretsError.keychain(status)   // a refusal is never "absent"
        }
    }
}

extension SecretsStore {
    private func presenceBackend() throws -> PresenceGatedSecretsBackend {
        if let name = guardActive { throw SecretsError.guarded(name) }
        guard let gated = backend as? PresenceGatedSecretsBackend else {
            throw SecretsError.invalid("this backend cannot keep a presence-gated item")
        }
        return gated
    }

    /// Create or overwrite. With `requiringPresence` the item sits behind the owner's presence
    /// (biometry or the device passcode, so it works on a passcode-only phone): `kSecAttrAccessControl`
    /// in place of `kSecAttrAccessible`, never synchronizable. Read it with ``get(_:context:)``;
    /// ``exists(_:)`` answers without prompting. Without it this is ``set(_:_:)``.
    public func set(_ key: String, _ value: Data, requiringPresence: Bool,
                    accessibility: SecretsPresenceAccessibility = .whenPasscodeSetThisDeviceOnly) throws {
        guard requiringPresence else { return try set(key, value) }
        try check(key)
        guard !value.isEmpty else { throw SecretsError.invalid("value is empty") }
        try presenceBackend().setPresenceGated(value, namespace: namespace, key: key, accessibility: accessibility)
    }

    /// Reads an item with `context`, which the system uses to ask the owner (or to skip the question
    /// when it already authenticated). `nil` only on authoritative absence; a cancelled or refused
    /// prompt throws ``SecretsError/keychain(_:)``. Works for ungated items too.
    public func get(_ key: String, context: LAContext) throws -> Data? {
        try check(key)
        return try presenceBackend().getPresenceGated(namespace: namespace, key: key, context: context)
    }
}
#endif

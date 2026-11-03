import Foundation
#if canImport(CryptoKit) && canImport(LocalAuthentication) && !os(watchOS) // Secure Enclave owner key: not on watchOS
import CryptoKit
import LocalAuthentication
import Security

/// The owner's approver key: a P-256 key made in the Secure Enclave. Every signature needs the
/// owner, by the `Policy` the key was made with: `.biometryCurrentSet` (the enrolled fingerprint;
/// adding or removing a fingerprint makes the key unusable, the default) or `.userPresence`
/// (biometry or the device passcode, so it works on a passcode-only device).
///
/// Where the key lives is the `Storage`:
/// - `.file(URL)`: CryptoKit's opaque `dataRepresentation` in a file (mode 0600). It holds no key
///   material and works only on this device's Secure Enclave.
/// - `.keychainItem(service:account:)`: the same opaque blob as a generic-password Keychain item
///   (never synchronizable), so the app's Keychain access list, not a loose file another process of
///   the user could swap, guards it.
/// - `.permanentKey(applicationTag:)`: a permanent Secure Enclave key in the Keychain
///   (`kSecAttrIsPermanent`, found by its application tag). Its public key and enrolment are read
///   without ever prompting. Signatures from this mode are produced by `SecKey`, so they are DER
///   first (`signDER`) and `sign` converts them to the raw `r | s` form.
///
/// A key whose `protection()` probe says it signs without asking is never used and is replaced.
/// When the stored key cannot be opened (biometry set changed, file or item damaged) `publicKey` and
/// `sign` make a NEW key and replace the stored one, so the public key changes: the verifier must
/// enrol it again (Pulse asks for its PIN for that). Whether a Developer ID build may create the key
/// without a keychain access group is unverified; when it cannot, `isAvailable` is true but creation
/// throws `Failure.unavailable`, and the caller falls back to its PIN path.
///
/// The Rust verify side is `verify_p256` and `owner_key_id` in `rightkit-credentials`.
public struct OwnerKey: Sendable {
    public enum Failure: Error, Equatable, Sendable {
        /// The user dismissed the prompt, or the biometry set changed under the key.
        case cancelled
        /// No Secure Enclave, no enrolled biometry or passcode, or the key could not be made or stored.
        case unavailable
    }

    /// What every signature of the key demands.
    public enum Policy: Sendable, Equatable {
        /// The enrolled biometry; a change of the enrolled set invalidates the key. Needs enrolled biometry.
        case biometryCurrentSet
        /// Biometry or the device passcode. Needs only a Secure Enclave and a passcode.
        case userPresence

        var flags: SecAccessControlCreateFlags {
            switch self {
            case .biometryCurrentSet: [.privateKeyUsage, .biometryCurrentSet]
            case .userPresence: [.privateKeyUsage, .userPresence]
            }
        }
    }

    /// When the key can be used, and whether it may exist without a device passcode.
    public enum Accessibility: Sendable, Equatable {
        case whenUnlockedThisDeviceOnly
        /// Only while a passcode is set; removing the passcode makes the key unusable. This device only.
        case whenPasscodeSetThisDeviceOnly

        var attribute: CFString {
            switch self {
            case .whenUnlockedThisDeviceOnly: kSecAttrAccessibleWhenUnlockedThisDeviceOnly
            case .whenPasscodeSetThisDeviceOnly: kSecAttrAccessibleWhenPasscodeSetThisDeviceOnly
            }
        }
    }

    /// Where the key is kept.
    public enum Storage: Sendable, Equatable {
        case file(URL)
        case keychainItem(service: String, account: String)
        case permanentKey(applicationTag: Data)
    }

    /// What `protection()` found out by trying to sign with a context that may not prompt.
    public enum Protection: Sendable, Equatable {
        /// The signature was refused without the owner: the key demands user presence or biometry.
        case userPresence
        /// The key signed without asking (made without access control, or swapped in). Never used.
        case unprotected
        /// There is no key, or it could not be opened.
        case unreadable
    }

    public let storage: Storage
    public let policy: Policy
    public let accessibility: Accessibility

    /// The key file, for `.file` storage; nil for the Keychain storages.
    public var fileURL: URL? {
        if case .file(let url) = storage { return url }
        return nil
    }

    public init(fileURL: URL) {
        self.init(storage: .file(fileURL))
    }

    public init(storage: Storage, policy: Policy = .biometryCurrentSet,
                accessibility: Accessibility = .whenUnlockedThisDeviceOnly) {
        self.storage = storage
        self.policy = policy
        self.accessibility = accessibility
    }

    /// Whether this device has a Secure Enclave and enrolled biometry.
    public static var isAvailable: Bool { isAvailable(policy: .biometryCurrentSet) }

    /// Whether this device can hold a key of `policy`: a Secure Enclave, and for `.biometryCurrentSet`
    /// enrolled biometry, for `.userPresence` a set passcode or enrolled biometry (a passcode-only
    /// device qualifies). Asks nothing.
    public static func isAvailable(policy: Policy) -> Bool {
        guard SecureEnclave.isAvailable else { return false }
        switch policy {
        case .biometryCurrentSet:
            return LAContext().canEvaluatePolicy(.deviceOwnerAuthenticationWithBiometrics, error: nil)
        case .userPresence:
            return LAContext().canEvaluatePolicy(.deviceOwnerAuthentication, error: nil)
        }
    }

    /// A context whose one successful biometric check covers every signature made with it in the
    /// next `reuseDuration` seconds (an enrolment proof and the approval of one click).
    public static func makeContext(reason: String, reuseDuration: TimeInterval = 10) -> LAContext {
        let context = LAContext()
        context.localizedReason = reason
        context.touchIDAuthenticationAllowableReuseDuration = reuseDuration
        return context
    }

    /// Lowercase hex.
    public static func hex(_ data: Data) -> String {
        data.map { String(format: "%02x", $0) }.joined()
    }

    /// The key's identifier: the first 16 bytes of SHA-256 over the public key, as hex. Equals
    /// `owner_key_id` in `rightkit-credentials`.
    public static func keyId(of publicKey: Data) -> String {
        hex(Data(SHA256.hash(data: publicKey)).prefix(16))
    }

    /// Whether a stored key exists (it may still fail to open). Never prompts.
    public var isEnrolled: Bool {
        switch storage {
        case .file, .keychainItem:
            return (try? readBlob()) != nil
        case .permanentKey(let tag):
            return (try? Self.loadPermanentKey(tag: tag, context: Self.probeContext())) != nil
        }
    }

    /// Same as `isEnrolled`.
    public var exists: Bool { isEnrolled }

    /// Removes the stored key; it can no longer be used and the next use makes a new one.
    public func delete() throws {
        switch storage {
        case .file(let url):
            guard FileManager.default.fileExists(atPath: url.path) else { return }
            try FileManager.default.removeItem(at: url)
        case .keychainItem:
            try deleteBlobItem()
        case .permanentKey(let tag):
            let status = SecItemDelete(Self.permanentKeyQuery(tag: tag) as CFDictionary)
            guard status == errSecSuccess || status == errSecItemNotFound else { throw Failure.unavailable }
        }
    }

    /// Tries a signature with a context that may not prompt: a key that demands the owner refuses
    /// (`.userPresence`), a key that signs silently is `.unprotected`, a missing or damaged key is
    /// `.unreadable`. Never prompts.
    public func protection() -> Protection {
        switch storage {
        case .file, .keychainItem:
            guard let data = try? readBlob() else { return .unreadable }
            return Self.protection(ofBlob: data)
        case .permanentKey(let tag):
            return Self.protection(ofPermanentTag: tag)
        }
    }

    /// The public key (65-byte uncompressed X9.63 point) of the stored key, read without prompting
    /// and without creating anything. nil when there is no usable key.
    public func enrolledPublicKey() -> Data? {
        let probe = Self.probeContext()
        switch storage {
        case .file, .keychainItem:
            guard let data = try? readBlob(),
                  let key = try? SecureEnclave.P256.Signing.PrivateKey(dataRepresentation: data,
                                                                       authenticationContext: probe)
            else { return nil }
            return key.publicKey.x963Representation
        case .permanentKey(let tag):
            guard let ref = try? Self.loadPermanentKey(tag: tag, context: probe) else { return nil }
            return Self.publicKeyData(of: ref)
        }
    }

    /// The public key as the 65-byte uncompressed X9.63 point, creating the key when there is none.
    public func publicKey(context: LAContext) throws -> Data {
        switch storage {
        case .file, .keychainItem:
            return try loadOrCreateBlobKey(context: context).publicKey.x963Representation
        case .permanentKey:
            guard let data = Self.publicKeyData(of: try loadOrCreatePermanentKey(context: context)) else {
                throw Failure.unavailable
            }
            return data
        }
    }

    /// Signs `message` (ECDSA P-256 / SHA-256) after the owner check. Returns the 64-byte raw
    /// `r | s` form, which `verify_p256` accepts. Any failure of the signing call, a dismissed
    /// prompt or a changed biometry set, is `Failure.cancelled`: the caller offers its PIN.
    public func sign(_ message: Data, context: LAContext) throws -> Data {
        try signed(message, context: context).raw
    }

    /// `sign(_:context:)` with a fresh context for `reason`.
    public func sign(_ message: Data, reason: String) throws -> Data {
        try sign(message, context: Self.makeContext(reason: reason))
    }

    /// Like `sign(_:context:)`, returning the DER encoding of the signature (the form `SecKey` and
    /// most verifiers take).
    public func signDER(_ message: Data, context: LAContext) throws -> Data {
        try signed(message, context: context).der
    }

    // MARK: Signing

    private func signed(_ message: Data, context: LAContext) throws -> (raw: Data, der: Data) {
        switch storage {
        case .file, .keychainItem:
            let key = try loadOrCreateBlobKey(context: context)
            do {
                let signature = try key.signature(for: message)
                return (signature.rawRepresentation, signature.derRepresentation)
            } catch {
                throw Failure.cancelled
            }
        case .permanentKey:
            let key = try loadOrCreatePermanentKey(context: context)
            var error: Unmanaged<CFError>?
            guard let der = SecKeyCreateSignature(key, .ecdsaSignatureMessageX962SHA256,
                                                  message as CFData, &error) as Data?
            else {
                error?.release()
                throw Failure.cancelled
            }
            guard let raw = Self.rawSignature(fromDER: der) else { throw Failure.unavailable }
            return (raw, der)
        }
    }

    /// `r | s`, each left-padded to 32 bytes, from an ASN.1 DER `SEQUENCE { INTEGER r, INTEGER s }`.
    static func rawSignature(fromDER der: Data) -> Data? {
        let bytes = [UInt8](der)
        var index = 0
        func byte() -> Int? {
            guard index < bytes.count else { return nil }
            defer { index += 1 }
            return Int(bytes[index])
        }
        func length() -> Int? {
            guard let first = byte() else { return nil }
            if first < 0x80 { return first }
            let count = first & 0x7f
            guard count >= 1, count <= 2 else { return nil }
            var value = 0
            for _ in 0..<count {
                guard let next = byte() else { return nil }
                value = (value << 8) | next
            }
            return value
        }
        func integer() -> [UInt8]? {
            guard byte() == 0x02, let size = length(), size > 0, index + size <= bytes.count else { return nil }
            var value = Array(bytes[index..<(index + size)])
            index += size
            while value.count > 32, value.first == 0 { value.removeFirst() }
            guard value.count <= 32 else { return nil }
            return [UInt8](repeating: 0, count: 32 - value.count) + value
        }
        guard byte() == 0x30, let total = length(), total == bytes.count - index,
              let r = integer(), let s = integer(), index == bytes.count
        else { return nil }
        return Data(r + s)
    }

    // MARK: Blob storages (file, keychain item)

    private static let probeMessage = Data("rightkit.owner-key.probe".utf8)

    private static func probeContext() -> LAContext {
        let context = LAContext()
        context.interactionNotAllowed = true
        return context
    }

    private static func protection(ofBlob data: Data) -> Protection {
        guard let key = try? SecureEnclave.P256.Signing.PrivateKey(dataRepresentation: data,
                                                                  authenticationContext: probeContext())
        else { return .unreadable }
        return (try? key.signature(for: probeMessage)) == nil ? .userPresence : .unprotected
    }

    private static func blobQuery(service: String, account: String) -> [String: Any] {
        [kSecClass as String: kSecClassGenericPassword,
         kSecAttrService as String: service,
         kSecAttrAccount as String: account,
         kSecAttrSynchronizable as String: false]
    }

    /// The stored blob, or nil when there is none. A Keychain failure other than "not found" throws,
    /// so a transient failure is never taken for an absent key (which would replace a good key).
    private func readBlob() throws -> Data? {
        switch storage {
        case .file(let url):
            return try? Data(contentsOf: url)
        case .keychainItem(let service, let account):
            var query = Self.blobQuery(service: service, account: account)
            query[kSecReturnData as String] = true
            query[kSecMatchLimit as String] = kSecMatchLimitOne
            var item: CFTypeRef?
            let status = SecItemCopyMatching(query as CFDictionary, &item)
            switch status {
            case errSecSuccess: return item as? Data
            case errSecItemNotFound: return nil
            default: throw Failure.unavailable
            }
        case .permanentKey:
            return nil
        }
    }

    private func writeBlob(_ data: Data) throws {
        switch storage {
        case .file(let url):
            do {
                try FileManager.default.createDirectory(at: url.deletingLastPathComponent(),
                                                        withIntermediateDirectories: true)
                try data.write(to: url, options: .atomic)
                try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: url.path)
            } catch {
                throw Failure.unavailable
            }
        case .keychainItem(let service, let account):
            let query = Self.blobQuery(service: service, account: account)
            let removed = SecItemDelete(query as CFDictionary)
            guard removed == errSecSuccess || removed == errSecItemNotFound else { throw Failure.unavailable }
            var item = query
            item[kSecValueData as String] = data
            item[kSecAttrAccessible as String] = accessibility.attribute
            guard SecItemAdd(item as CFDictionary, nil) == errSecSuccess else { throw Failure.unavailable }
        case .permanentKey:
            throw Failure.unavailable
        }
    }

    private func deleteBlobItem() throws {
        guard case .keychainItem(let service, let account) = storage else { return }
        let status = SecItemDelete(Self.blobQuery(service: service, account: account) as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else { throw Failure.unavailable }
    }

    private func makeAccessControl() throws -> SecAccessControl {
        var error: Unmanaged<CFError>?
        guard let access = SecAccessControlCreateWithFlags(nil, accessibility.attribute, policy.flags, &error) else {
            error?.release()
            throw Failure.unavailable
        }
        return access
    }

    private func loadOrCreateBlobKey(context: LAContext) throws -> SecureEnclave.P256.Signing.PrivateKey {
        if let data = try readBlob() {
            if Self.protection(ofBlob: data) == .userPresence,
               let key = try? SecureEnclave.P256.Signing.PrivateKey(dataRepresentation: data,
                                                                    authenticationContext: context) {
                return key
            }
            // Unreadable, or signs without asking: never used. A Keychain item is removed before
            // the new one is written; a file is overwritten below.
            try deleteBlobItem()
        }
        let access = try makeAccessControl()
        let key: SecureEnclave.P256.Signing.PrivateKey
        do {
            key = try SecureEnclave.P256.Signing.PrivateKey(accessControl: access, authenticationContext: context)
        } catch {
            throw Failure.unavailable
        }
        guard Self.protection(ofBlob: key.dataRepresentation) == .userPresence else { throw Failure.unavailable }
        try writeBlob(key.dataRepresentation)
        return key
    }

    // MARK: Permanent Keychain key

    private static func permanentKeyQuery(tag: Data) -> [String: Any] {
        [kSecClass as String: kSecClassKey,
         kSecAttrApplicationTag as String: tag,
         kSecAttrKeyType as String: kSecAttrKeyTypeECSECPrimeRandom,
         kSecAttrTokenID as String: kSecAttrTokenIDSecureEnclave,
         kSecUseDataProtectionKeychain as String: true]
    }

    /// The key reference, or nil when absent. Obtaining the reference does not prompt; `context`
    /// is the one the later signature uses.
    private static func loadPermanentKey(tag: Data, context: LAContext) throws -> SecKey? {
        var query = permanentKeyQuery(tag: tag)
        query[kSecReturnRef as String] = true
        query[kSecUseAuthenticationContext as String] = context
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        switch status {
        case errSecSuccess:
            guard let item, CFGetTypeID(item) == SecKeyGetTypeID() else { throw Failure.unavailable }
            return (item as! SecKey) // the type id was checked on the line above
        case errSecItemNotFound:
            return nil
        default:
            throw Failure.unavailable
        }
    }

    private static func publicKeyData(of key: SecKey) -> Data? {
        guard let publicKey = SecKeyCopyPublicKey(key),
              let data = SecKeyCopyExternalRepresentation(publicKey, nil) as Data? else { return nil }
        return data
    }

    private static func protection(ofPermanentTag tag: Data) -> Protection {
        guard let key = try? loadPermanentKey(tag: tag, context: probeContext()) else { return .unreadable }
        var error: Unmanaged<CFError>?
        let signature = SecKeyCreateSignature(key, .ecdsaSignatureMessageX962SHA256,
                                              probeMessage as CFData, &error)
        error?.release()
        return signature == nil ? .userPresence : .unprotected
    }

    private func loadOrCreatePermanentKey(context: LAContext) throws -> SecKey {
        guard case .permanentKey(let tag) = storage else { throw Failure.unavailable }
        if let key = try Self.loadPermanentKey(tag: tag, context: context) {
            if Self.protection(ofPermanentTag: tag) == .userPresence { return key }
            try delete()
        }
        var attributes: [String: Any] = [
            kSecAttrKeyType as String: kSecAttrKeyTypeECSECPrimeRandom,
            kSecAttrKeySizeInBits as String: 256,
            kSecAttrTokenID as String: kSecAttrTokenIDSecureEnclave,
            kSecUseDataProtectionKeychain as String: true,
        ]
        attributes[kSecPrivateKeyAttrs as String] = [
            kSecAttrIsPermanent as String: true,
            kSecAttrApplicationTag as String: tag,
            kSecAttrAccessControl as String: try makeAccessControl(),
        ] as [String: Any]
        var error: Unmanaged<CFError>?
        guard let key = SecKeyCreateRandomKey(attributes as CFDictionary, &error) else {
            error?.release()
            throw Failure.unavailable
        }
        guard Self.protection(ofPermanentTag: tag) == .userPresence else {
            try? delete()
            throw Failure.unavailable
        }
        return key
    }
}
#endif

import Foundation
#if canImport(CryptoKit) && canImport(LocalAuthentication) && !os(watchOS) // biometric Secure Enclave key: not on watchOS
import CryptoKit
import LocalAuthentication
import Security

/// The owner's approver key: a P-256 key made in the Secure Enclave with `.biometryCurrentSet`.
/// Every signature needs the enrolled fingerprint, and adding or removing a fingerprint makes the
/// key unusable. CryptoKit's opaque `dataRepresentation` is kept in `fileURL` (mode 0600); it holds
/// no key material and works only on this device's Secure Enclave.
///
/// When the stored key cannot be opened (biometry set changed, file damaged) `loadOrCreate` makes
/// a NEW key and replaces the file, so the public key changes: the verifier must enrol it again
/// (Pulse asks for its PIN for that). Whether a Developer ID build may create the key without a
/// keychain access group is unverified; when it cannot, `isAvailable` is true but creation throws
/// `Failure.unavailable`, and the caller falls back to its PIN path.
///
/// The Rust verify side is `verify_p256` and `owner_key_id` in `rightkit-credentials`.
public struct OwnerKey: Sendable {
    public enum Failure: Error, Equatable, Sendable {
        /// The user dismissed the prompt, or the biometry set changed under the key.
        case cancelled
        /// No Secure Enclave, no enrolled biometry, or the key could not be made or stored.
        case unavailable
    }

    public let fileURL: URL

    public init(fileURL: URL) {
        self.fileURL = fileURL
    }

    /// Whether this device has a Secure Enclave and enrolled biometry.
    public static var isAvailable: Bool {
        guard SecureEnclave.isAvailable else { return false }
        return LAContext().canEvaluatePolicy(.deviceOwnerAuthenticationWithBiometrics, error: nil)
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

    /// Whether a key file exists (it may still fail to open).
    public var exists: Bool {
        FileManager.default.fileExists(atPath: fileURL.path)
    }

    /// Removes the key file; the key can no longer be used and the next use makes a new one.
    public func delete() throws {
        guard exists else { return }
        try FileManager.default.removeItem(at: fileURL)
    }

    private func loadOrCreate(context: LAContext) throws -> SecureEnclave.P256.Signing.PrivateKey {
        if let data = try? Data(contentsOf: fileURL),
           let key = try? SecureEnclave.P256.Signing.PrivateKey(dataRepresentation: data,
                                                                authenticationContext: context) {
            return key
        }
        var error: Unmanaged<CFError>?
        guard let access = SecAccessControlCreateWithFlags(
            nil, kSecAttrAccessibleWhenUnlockedThisDeviceOnly,
            [.privateKeyUsage, .biometryCurrentSet], &error)
        else { throw Failure.unavailable }
        let key: SecureEnclave.P256.Signing.PrivateKey
        do {
            key = try SecureEnclave.P256.Signing.PrivateKey(accessControl: access,
                                                            authenticationContext: context)
        } catch {
            throw Failure.unavailable
        }
        do {
            try FileManager.default.createDirectory(at: fileURL.deletingLastPathComponent(),
                                                    withIntermediateDirectories: true)
            try key.dataRepresentation.write(to: fileURL, options: .atomic)
            try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: fileURL.path)
        } catch {
            throw Failure.unavailable
        }
        return key
    }

    /// The public key as the 65-byte uncompressed X9.63 point, creating the key when there is none.
    public func publicKey(context: LAContext) throws -> Data {
        try loadOrCreate(context: context).publicKey.x963Representation
    }

    /// Signs `message` (ECDSA P-256 / SHA-256) after the biometric check. Returns the 64-byte raw
    /// `r | s` form, which `verify_p256` accepts. Any failure of the signing call, a dismissed
    /// prompt or a changed biometry set, is `Failure.cancelled`: the caller offers its PIN.
    public func sign(_ message: Data, context: LAContext) throws -> Data {
        let key = try loadOrCreate(context: context)
        do {
            return try key.signature(for: message).rawRepresentation
        } catch {
            throw Failure.cancelled
        }
    }

    /// `sign(_:context:)` with a fresh context for `reason`.
    public func sign(_ message: Data, reason: String) throws -> Data {
        try sign(message, context: Self.makeContext(reason: reason))
    }
}
#endif

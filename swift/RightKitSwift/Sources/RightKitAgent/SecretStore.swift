import Foundation
import LocalAuthentication
import RightKitCredentials
import Security

/// Storage for the credentials the local runtime needs: model/tool API keys
/// and forge (GitHub) tokens.
///
/// Deliberately separate from `KeychainStore`, which holds daemon pairing
/// metadata. These are different risk classes: losing a pairing token costs
/// a re-pair, a leaked provider key is someone else's money.
///
/// # Why there is no per-item `.userPresence` here
///
/// An earlier version gated every Keychain item with `SecAccessControl`
/// `.userPresence`, on the theory that the Secure Enclave enforcing it is
/// stronger than app code that could be patched around. In practice this
/// backfired: `.userPresence` re-evaluates on **every**
/// `SecItemCopyMatching`, and neither a shared `LAContext` nor
/// `touchIDAuthenticationAllowableReuseDuration` suppresses that — that
/// property only governs `LAContext.evaluatePolicy()` reuse, not Keychain ACL
/// evaluation (confirmed against Apple Developer Forums thread 812733: even a
/// reused context with max reuse duration set re-prompts per
/// `SecItemCopyMatching` call). The result was a Face ID prompt on nearly
/// every screen interaction.
///
/// The fix used by real biometric-gated apps (1Password, banking apps) is an
/// **app-level** unlock gate with a time-based grace window — see
/// `AppLockGate` — sitting in front of storage that is merely
/// `WhenUnlockedThisDeviceOnly` (device-only, off backups, requires the
/// device passcode to be unlocked, but no per-read biometric evaluation).
/// `AppLockGate` is the actual enforcement point and the actual place
/// "require Face ID" toggles.
///
/// The honest trade-off: an on-device forensic read of raw Keychain storage
/// no longer requires biometrics at the OS layer, only device-unlock. That is
/// the price of the reuse-window UX Apple's Keychain ACL cannot provide.
public struct SecretStore: Sendable {
    public enum Failure: Error, LocalizedError, Equatable {
        /// The device has no passcode set, so the OS cannot protect anything.
        case deviceNotSecured
        /// The user cancelled, or authentication failed.
        case notAuthenticated
        /// Biometry is locked out after repeated failures; a passcode is required.
        case biometryLockedOut
        case keychain(OSStatus)

        public var errorDescription: String? {
            switch self {
            case .deviceNotSecured:
                return "Set a device passcode before storing API keys — without one, iOS cannot protect them."
            case .notAuthenticated:
                return "Authentication is required to use this credential."
            case .biometryLockedOut:
                return "Too many failed attempts. Unlock the device with your passcode, then try again."
            case let .keychain(status):
                return "Keychain error \(status)."
            }
        }
    }

    /// What kind of secret a record holds. The kind is part of the account key,
    /// so a provider key and a forge token can never be returned for one another.
    public enum Kind: String, Sendable {
        case providerAPIKey = "provider-api-key"
        case toolAPIKey = "tool-api-key"
        case forgeToken = "forge-token"
    }

    private let service: String
    /// Keychain access (create/replace/read/delete) is RightKitCredentials' `SecretsStore`; this
    /// type only adds CodeRight's kind-scoped account key and its error vocabulary.
    private let keychain: SecretsStore?

    public init(service: String = "app.coderight.CodeRight.secrets") {
        self.service = service
        keychain = try? SecretsStore(namespace: service, accessibility: .whenUnlockedThisDeviceOnly)
    }

    private func backing() throws -> SecretsStore {
        guard let keychain else { throw Failure.keychain(errSecParam) }
        return keychain
    }

    private static func map(_ error: Error) -> Error {
        guard let error = error as? SecretsError else { return error }
        switch error {
        case let .keychain(status): return Failure.keychain(status)
        case .invalid: return Failure.keychain(errSecParam)
        case .guarded: return Failure.keychain(errSecNotAvailable)
        }
    }

    // MARK: - Availability

    /// What the device can actually enforce right now. Callers show this
    /// before offering to store anything, because storing a secret we cannot
    /// protect is worse than refusing.
    public struct Availability: Sendable, Equatable {
        public let canEvaluate: Bool
        public let biometryKind: LABiometryType
        /// Nil when `canEvaluate` is true.
        public let reason: String?
    }

    public static func availability() -> Availability {
        let context = LAContext()
        var error: NSError?
        // `.deviceOwnerAuthentication` — biometry with passcode fallback.
        // Passcode-only devices must still be able to use the app; refusing
        // them would push users toward pasting keys somewhere worse.
        let ok = context.canEvaluatePolicy(.deviceOwnerAuthentication, error: &error)
        return Availability(
            canEvaluate: ok,
            biometryKind: context.biometryType,
            reason: ok ? nil : (error?.localizedDescription ?? "Authentication is unavailable.")
        )
    }

    // MARK: - Write

    /// Store a secret. Device-only, off backups. The biometric gate lives in
    /// `AppLockGate`, in front of this — see the type documentation above.
    /// `SecretsStore.set` rewrites the accessibility class on update, so a record written under the
    /// old per-item `.userPresence` policy does not keep that access control.
    public func store(_ secret: String, kind: Kind, id: String) throws {
        do {
            try backing().setString(account(kind: kind, id: id), secret)
        } catch {
            throw Self.map(error)
        }
    }

    // MARK: - Read

    /// Read a secret. Callers that need a biometric gate on this call
    /// `AppLockGate.unlockIfNeeded(reason:)` first — this method itself never
    /// prompts, matching the write path's plain `WhenUnlockedThisDeviceOnly`
    /// storage.
    public func read(kind: Kind, id: String) throws -> String? {
        do {
            return try backing().getString(account(kind: kind, id: id))
        } catch {
            throw Self.map(error)
        }
    }

    // MARK: - Delete / enumerate

    public func remove(kind: Kind, id: String) throws {
        do {
            try backing().delete(account(kind: kind, id: id))
        } catch {
            throw Self.map(error)
        }
    }

    /// Whether a secret is saved under `kind` and `id`. Never authenticates: items are plain
    /// `WhenUnlockedThisDeviceOnly` (see the type documentation), so Settings can show
    /// "Anthropic: key saved" without a Face ID prompt, and a read failure is "not saved".
    public func isStored(kind: Kind, id: String) -> Bool {
        (try? backing().exists(account(kind: kind, id: id))) ?? false
    }

    private func account(kind: Kind, id: String) -> String {
        "\(kind.rawValue):\(id)"
    }
}

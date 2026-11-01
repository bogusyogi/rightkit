import Foundation

/// APP-15: secret-key backup & guided restore, plus optional phone-number linking.
///
/// Reuses `SecretStore`'s Keychain pattern (device-only, off-backups storage) rather than
/// inventing a new wrapper. `SecretStore.Kind` is a closed enum owned by `SecretStore.swift`
/// (read-only for this change), so isolation from provider/tool/forge secrets comes from a
/// dedicated Keychain `service` string below instead of a new `Kind` case — the account/kind
/// namespacing inside that service never collides with `SecretStore`'s default service.

/// A user-exportable backup of the account's recovery secret. `displayText` is what a
/// guided restore accepts back via manual entry or QR scan.
public struct RecoveryBackup: Equatable, Sendable {
    public let displayText: String
    public let createdAt: Date

    public init(displayText: String, createdAt: Date = Date()) {
        self.displayText = displayText
        self.createdAt = createdAt
    }
}

public enum RecoveryError: Error, LocalizedError, Equatable {
    case noSecretToBackUp
    case invalidRestoreInput

    public var errorDescription: String? {
        switch self {
        case .noSecretToBackUp: return "There is no account secret to back up yet."
        case .invalidRestoreInput: return "That doesn't look like a valid recovery key."
        }
    }
}

/// Coordinates backup/restore of the account recovery secret via `SecretStore`. Kept
/// separate from daemon pairing (`KeychainStore`) and provider secrets (`SecretStore`'s
/// other kinds) — same risk-separation rationale as `SecretStore`'s own doc comment.
public struct AccountRecoveryStore: Sendable {
    /// A `SecretStore.Kind` value is required by the shared API but is meaningless once the
    /// service string below already isolates this store; `.toolAPIKey` is reused purely as
    /// an inert tag, never mixed with actual tool keys because the service differs.
    private static let kind = SecretStore.Kind.toolAPIKey
    private let secretStore: SecretStore
    private let accountId: String

    public init(
        secretStore: SecretStore = SecretStore(service: "app.coderight.CodeRight.recovery"),
        accountId: String = "default"
    ) {
        self.secretStore = secretStore
        self.accountId = accountId
    }

    /// Produces a user-exportable backup of the current recovery secret. Generates and
    /// persists one on first use if none exists yet, so "back up my key" always has
    /// something to show.
    public func makeBackup() throws -> RecoveryBackup {
        if let existing = try secretStore.read(kind: Self.kind, id: accountId) {
            return RecoveryBackup(displayText: existing)
        }
        let generated = Self.generateRecoveryKey()
        try secretStore.store(generated, kind: Self.kind, id: accountId)
        return RecoveryBackup(displayText: generated)
    }

    /// Guided restore: accepts a manually-typed or QR-scanned recovery key and persists it,
    /// replacing whatever is currently stored. Both entry paths funnel through this one
    /// validating call.
    public func restore(from input: String) throws {
        let trimmed = input.trimmingCharacters(in: .whitespacesAndNewlines)
        guard Self.isValidRecoveryKey(trimmed) else { throw RecoveryError.invalidRestoreInput }
        try secretStore.store(trimmed, kind: Self.kind, id: accountId)
    }

    public func hasBackup() -> Bool {
        secretStore.isStored(kind: Self.kind, id: accountId)
    }

    /// Recovery keys are grouped 4-4-4-4-4 base32-alphabet blocks — easy to read aloud for
    /// the "lost access" support path, and easy to render as a QR payload.
    static func generateRecoveryKey() -> String {
        let alphabet = Array("ABCDEFGHJKLMNPQRSTUVWXYZ23456789") // no 0/O/1/I
        var chars: [Character] = []
        for _ in 0..<20 {
            chars.append(alphabet.randomElement()!)
        }
        return stride(from: 0, to: chars.count, by: 4)
            .map { String(chars[$0..<min($0 + 4, chars.count)]) }
            .joined(separator: "-")
    }

    static func isValidRecoveryKey(_ input: String) -> Bool {
        let stripped = input.replacingOccurrences(of: "-", with: "").uppercased()
        guard stripped.count >= 8 else { return false }
        let allowed = Set("ABCDEFGHJKLMNPQRSTUVWXYZ23456789")
        return stripped.allSatisfy { allowed.contains($0) }
    }
}

/// Optional phone-number linking as an additional recovery factor.
///
/// TODO(backend): there is no phone-linking/verification endpoint yet. This type models
/// the client-side state machine (unlinked → codeSent → verified) so the UI can be built
/// and exercised now, but `requestVerificationCode` / `confirmVerificationCode` below must
/// be wired to a real daemon/account-service endpoint before this ships as functional —
/// they deliberately throw `.notImplemented` rather than silently reporting success.
public struct PhoneRecoveryLink: Sendable {
    public enum State: Equatable, Sendable {
        case unlinked
        case codeSent(phoneNumber: String)
        case verified(phoneNumber: String)
    }

    public enum Failure: Error, LocalizedError, Equatable {
        case notImplemented

        public var errorDescription: String? {
            "Phone-number recovery isn't available yet — no verification backend is wired up."
        }
    }

    public init() {}

    public func requestVerificationCode(for phoneNumber: String) async throws -> State {
        // TODO(backend): POST phoneNumber to an account-recovery verification endpoint
        // once one exists, then return .codeSent(phoneNumber: phoneNumber).
        throw Failure.notImplemented
    }

    public func confirmVerificationCode(_ code: String, for phoneNumber: String) async throws -> State {
        // TODO(backend): verify `code` against the endpoint from requestVerificationCode,
        // then return .verified(phoneNumber: phoneNumber).
        throw Failure.notImplemented
    }
}

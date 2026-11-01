// RIGHTKIT-SERIOUS: I-3 synced-credential policy for the pro.bridge item (iCloud consent switch and allow-list). RightKitCredentials SecretsStore hard-codes kSecAttrSynchronizable false (RightKitCredentials.swift:64-70) and has no consent or allow-list; kept, joint ruling NO (docs/rightkit-exceptions/M11.md I-3)
import CryptoKit
import Foundation
import RightKitAppleCredentials

/// The only credentials the app may place in the *synchronizable* iCloud
/// Keychain.
///
/// **May sync**:
///   - `sync.consent`: historical account marker, contains no user data.
///   - `pro.bridge`: opaque backend-issued continuity token (`SyncedCredentialAccount`).
///
/// **Never synced, never touched here**: provider API keys, session secrets, MCP
/// secrets, daemon pairing tokens, licence tokens, device ids, tool and forge
/// tokens, token-plan refresh secrets. Those stay in `SecretStore`,
/// `MCPSecretStore`, `KeychainStore` and `LicenseStore`'s device-only Keychain items.
///
/// Turning sync OFF never deletes the iCloud copy.
enum SyncedCredentialAccount: Hashable, Sendable {
    case syncConsent
    case proBridge

    var rawValue: String {
        switch self {
        case .syncConsent: return "sync.consent"
        case .proBridge: return "pro.bridge"
        }
    }

    /// Parses a raw Keychain account name. Returns nil for anything outside the
    /// allow-list, which is how the policy refuses `license.key`, `device.id`,
    /// `session.*`, `mcp.*`, `api.*` and every other account.
    init?(rawValue: String) {
        switch rawValue {
        case "sync.consent": self = .syncConsent
        case "pro.bridge": self = .proBridge
        default: return nil
        }
    }
}

/// Result of reconciling one account while sync is being turned on. Nothing is
/// overwritten just because it lives in iCloud.
enum CredentialSyncOutcome: Equatable, Sendable {
    /// Only an iCloud copy existed; the caller should adopt it locally.
    case adoptedRemote
    /// Only a local copy existed; it was pushed up so other devices find it.
    case seededRemote
    /// Both copies exist with the same value.
    case unchanged
    /// Both copies exist and differ. Both are preserved untouched.
    case conflict
    /// Neither copy exists, or the Keychain could not be reached or written.
    case unavailable
}

enum AppleSyncedCredentialError: LocalizedError, Equatable {
    case keychain(String)

    var errorDescription: String? {
        switch self {
        case let .keychain(detail): return detail
        }
    }
}

/// Synchronizable Keychain policy layer for the consent flag and `pro.bridge`.
// UserDefaults is documented thread-safe; the struct holds no other mutable state.
struct AppleSyncedCredentialStore: @unchecked Sendable {
    /// The one user-facing switch. Defaults OFF.
    static let syncEnabledDefaultsKey = "coderight.sync.apple.enabled"
    static let syncConsentMarker = "coderight.apple-sync.v1"

    /// `$(AppIdentifierPrefix)` in the entitlement resolves to the team id at
    /// build time; the Security APIs need the literal prefix. Resolved exactly
    /// as the app does (same team, hard-coded prefix).
    static let accessGroup = "6KLGD3LLKF.app.coderight.shared"
    static let service = "app.coderight.shared.credentials"

    static let shared = AppleSyncedCredentialStore()

    private let defaults: UserDefaults
    private let rightKitStore: AppleCredentialStore

    init(defaults: UserDefaults = .standard, backend: (any AppleCredentialBackend)? = nil) {
        self.defaults = defaults
        self.rightKitStore = AppleCredentialStore(
            service: Self.service,
            accessGroup: Self.accessGroup,
            backend: backend
        )
    }

    /// No cloud write may happen before the user has explicitly opted in.
    var isSyncEnabled: Bool {
        defaults.object(forKey: Self.syncEnabledDefaultsKey) as? Bool ?? false
    }

    /// Ported from the app `adoptAccountConsentIfNeeded`, which is deliberately NOT an auto-enable:
    /// the synchronized `sync.consent` marker is historical account state that may be stale, so it
    /// never turns sync on for a fresh or reprovisioned device, and a locally stored `false` is an
    /// explicit per-device disable. Returns the current local choice.
    @discardableResult
    func adoptAccountConsentIfNeeded() -> Bool {
        isSyncEnabled
    }

    // MARK: - Toggle

    /// ON: records consent, then reconciles the given accounts (`pro.bridge`) against their local copy.
    @discardableResult
    func enableSync(
        accounts: [(account: SyncedCredentialAccount, local: String?)] = []
    ) -> [SyncedCredentialAccount: CredentialSyncOutcome] {
        defaults.set(true, forKey: Self.syncEnabledDefaultsKey)
        _ = try? writeSynced(Self.syncConsentMarker, account: .syncConsent)
        var outcomes: [SyncedCredentialAccount: CredentialSyncOutcome] = [:]
        for entry in accounts {
            outcomes[entry.account] = reconcile(entry.account, local: entry.local)
        }
        return outcomes
    }

    /// OFF: stops reading and writing iCloud. The iCloud copy is never deleted.
    func disableSync() {
        defaults.set(false, forKey: Self.syncEnabledDefaultsKey)
    }

    func reconcile(_ account: SyncedCredentialAccount, local: String?) -> CredentialSyncOutcome {
        let remote = readSynced(account)
        switch (remote, local) {
        case (nil, nil):
            return .unavailable
        case (.some, nil):
            return .adoptedRemote
        case let (nil, .some(localValue)):
            do {
                try writeSynced(localValue, account: account)
                return .seededRemote
            } catch {
                return .unavailable
            }
        case let (.some(remoteValue), .some(localValue)):
            return Self.version(of: remoteValue) == Self.version(of: localValue) ? .unchanged : .conflict
        }
    }

    /// One-way digest used only to compare two copies; never logged.
    private static func version(of value: String) -> String {
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        return SHA256.hash(data: Data(trimmed.utf8)).map { String(format: "%02x", $0) }.joined()
    }

    // MARK: - Reads and writes (pro.bridge, sync.consent)

    /// The iCloud copy when sync is on; nil when sync is off (no keychain call).
    func read(_ account: SyncedCredentialAccount) -> String? {
        guard isSyncEnabled else { return nil }
        return readSynced(account)
    }

    /// Passive presence check, no secret bytes read. False when sync is off.
    func exists(_ account: SyncedCredentialAccount) -> Bool {
        guard isSyncEnabled else { return false }
        if case let .success(found) = rightKitStore.exists(account.rawValue, scope: .synced) {
            return found
        }
        return false
    }

    /// Writes the iCloud copy. Returns false without touching the Keychain when
    /// sync is off. Throws when the write fails.
    @discardableResult
    func write(_ value: String, for account: SyncedCredentialAccount) throws -> Bool {
        guard isSyncEnabled else { return false }
        try writeSynced(value, account: account)
        return true
    }

    /// The user explicitly deleted this credential. Removes the iCloud copy.
    /// Skips the Keychain when nothing is stored.
    func removeEverywhere(_ account: SyncedCredentialAccount) {
        if case .success(false) = rightKitStore.exists(account.rawValue, scope: .synced),
           case .success(false) = rightKitStore.exists(account.rawValue, scope: .device) {
            return
        }
        _ = rightKitStore.deleteEverywhere(account.rawValue)
    }

    // MARK: - Internals

    private func readSynced(_ account: SyncedCredentialAccount) -> String? {
        guard case let .success(value) = rightKitStore.read(account.rawValue, scope: .synced)
        else { return nil }
        return value
    }

    private func writeSynced(_ value: String, account: SyncedCredentialAccount) throws {
        if case let .failure(error) = rightKitStore.write(value, account: account.rawValue, scope: .synced) {
            throw AppleSyncedCredentialError.keychain(error.localizedDescription)
        }
    }
}

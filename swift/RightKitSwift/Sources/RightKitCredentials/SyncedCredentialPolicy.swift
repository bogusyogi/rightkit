import Foundation

/// What a synced credential looks like to the policy. The policy compares fingerprints, never secret values.
public struct SyncedCredentialSnapshot: Equatable, Sendable {
    public let account: String
    /// Digest of the stored secret, computed by the caller. The policy never sees the secret.
    public let fingerprint: String
    public let updatedAt: Date

    public init(account: String, fingerprint: String, updatedAt: Date) {
        self.account = account
        self.fingerprint = fingerprint
        self.updatedAt = updatedAt
    }
}

/// Why a reconcile needs the user's consent before anything changes.
public enum SyncedCredentialConflict: Equatable, Sendable {
    /// The remote copy has no local counterpart yet.
    case localMissing
    /// Both copies exist with the same timestamp but different values.
    case valuesDiffer
    /// The remote copy is newer and differs from the local copy.
    case remoteNewer
}

public enum SyncedCredentialDecision: Equatable, Sendable {
    /// Nothing to do.
    case absent
    /// The local copy stands. No write.
    case keepLocal
    /// Replace the local copy with the remote copy. Only after consent.
    case adoptRemote
    /// Do not change anything until the user decides.
    case needsConsent(SyncedCredentialConflict)
}

/// Reconciles a credential that arrived through iCloud Keychain sync with the copy on this device.
///
/// The policy never overwrites a local secret silently. A newer or missing remote copy changes local state only
/// when the user has given consent for that account. A local copy that is newer than the remote stays. The policy
/// only decides; the caller performs the keychain write, and only the creating binary may write its own items.
public enum SyncedCredentialPolicy {
    public enum Consent: Equatable, Sendable {
        /// The user has not agreed to take the remote copy.
        case none
        /// The user agreed to take the remote copy for this account.
        case adoptRemote
    }

    public static func reconcile(
        local: SyncedCredentialSnapshot?,
        remote: SyncedCredentialSnapshot?,
        consent: Consent
    ) -> SyncedCredentialDecision {
        guard let remote else {
            return local == nil ? .absent : .keepLocal
        }
        guard let local else {
            return consent == .adoptRemote ? .adoptRemote : .needsConsent(.localMissing)
        }
        if local.fingerprint == remote.fingerprint {
            return .keepLocal
        }
        if local.updatedAt > remote.updatedAt {
            return .keepLocal
        }
        if local.updatedAt < remote.updatedAt {
            return consent == .adoptRemote ? .adoptRemote : .needsConsent(.remoteNewer)
        }
        return consent == .adoptRemote ? .adoptRemote : .needsConsent(.valuesDiffer)
    }
}

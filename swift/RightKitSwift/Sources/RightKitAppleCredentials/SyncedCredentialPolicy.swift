import CryptoKit
import Foundation

/// What happened when a credential account was reconciled while Apple sync was being turned on.
/// Nothing is overwritten just because it lives in iCloud.
public enum CredentialSyncOutcome: Equatable, Sendable {
    /// Only a remote copy existed; reads prefer the synchronized scope, so no write was needed.
    case adoptedRemote
    /// Only a local copy existed; it was pushed up so other devices find it.
    case seededRemote
    /// Both copies exist and carry the same value.
    case unchanged
    /// Both copies exist and differ. BOTH are preserved untouched; the UI must report an
    /// actionable conflict. The value itself is never logged.
    case conflict
    /// The synchronized copy held a deletion (a tombstone) and the local copy was erased. Produced by
    /// ``SyncedEnvelopeStore``; the plain-account reconciler never reports it.
    case erased
    /// An existing copy could not be read or understood; it is left untouched and nothing is written.
    case unreadable
    /// Neither copy exists, or the Keychain could not be written.
    case unavailable
}

/// The pure decision half of reconciliation, separated from storage so it is unit-testable and
/// shared by every suite app (ScrapeRight and HeardRight previously duplicated it).
public enum SyncedCredentialPolicy {
    public enum Decision: Equatable, Sendable {
        case nothing
        case adoptRemote
        /// Write this local value to the synchronized scope.
        case seedRemote(String)
        case unchanged
        case conflict
    }

    /// Never "synchronized always wins": both values are compared and a genuine difference is a
    /// conflict with neither side overwritten.
    public static func decide(remote: String?, local: String?) -> Decision {
        switch (remote, local) {
        case (nil, nil): return .nothing
        case (.some, nil): return .adoptRemote
        case let (nil, .some(value)): return .seedRemote(value)
        case let (.some(r), .some(l)): return digest(of: r) == digest(of: l) ? .unchanged : .conflict
        }
    }

    /// One-way content digest used only to compare two copies of the same credential.
    public static func digest(of value: String) -> String {
        SHA256.hash(data: Data(value.utf8)).map { String(format: "%02x", $0) }.joined()
    }

    /// A fresh install with no stored choice defaults OFF: no cloud write before explicit opt-in.
    public static func isSyncEnabled(stored: Bool?) -> Bool { stored ?? false }

    /// Consent adoption: a stored choice (including an explicit per-device `false`) is never
    /// overwritten, and a synchronized consent marker is historical state that never silently
    /// turns sync on after local state is cleared. So the only automatic result is the current state.
    public static func adoptsAccountConsent(stored: Bool?, remoteMarkerMatches _: Bool) -> Bool {
        isSyncEnabled(stored: stored)
    }
}

/// Applies `SyncedCredentialPolicy` to an `AppleCredentialStore`. Turning sync OFF never deletes
/// the synchronized copy (Apple propagates that deletion to every device): it copies the value
/// to the device-only scope and stops reading and writing the synchronized one.
public struct SyncedCredentialReconciler: Sendable {
    public let store: AppleCredentialStore
    // UserDefaults is documented thread-safe (Apple: "can be accessed from any thread") but is not
    // declared Sendable, which Swift 6.4 rejects in a Sendable struct. The property is immutable and
    // only used through those thread-safe accessors.
    nonisolated(unsafe) private let defaults: UserDefaults
    public let enabledDefaultsKey: String
    /// Versioned, content-free marker account/value published on opt-in; nil to skip it.
    public let consent: (account: String, marker: String)?

    public init(
        store: AppleCredentialStore,
        defaults: UserDefaults = .standard,
        enabledDefaultsKey: String,
        consent: (account: String, marker: String)? = nil
    ) {
        self.store = store
        self.defaults = defaults
        self.enabledDefaultsKey = enabledDefaultsKey
        self.consent = consent
    }

    public var isSyncEnabled: Bool {
        SyncedCredentialPolicy.isSyncEnabled(stored: defaults.object(forKey: enabledDefaultsKey) as? Bool)
    }

    /// `nil` only on authoritative absence; a Keychain failure is surfaced, never read as "absent",
    /// so a failed read can never cause the other copy to overwrite a value that was merely unreadable.
    private func value(_ account: String, _ scope: AppleCredentialScope) -> Result<String?, AppleCredentialError> {
        switch store.read(account, scope: scope) {
        case let .success(text?):
            return .success(text)
        case .success(nil):
            // A non-UTF-8 item also reads back as nil: absence is only authoritative when no item exists.
            switch store.exists(account, scope: scope) {
            case .success(false): return .success(nil)
            case .success(true): return .failure(.unavailable("Keychain item is present but not readable as text."))
            case let .failure(error): return .failure(error)
            }
        case let .failure(error):
            return .failure(error)
        }
    }

    private func lenientValue(_ account: String, _ scope: AppleCredentialScope) -> String? {
        (try? value(account, scope).get()).flatMap { $0 }
    }

    /// Reconciles one account for the ON transition. Any unreadable copy is `.unreadable` and nothing is written.
    public func reconcile(_ account: String) -> CredentialSyncOutcome {
        guard case let .success(remote) = value(account, .synced),
              case let .success(local) = value(account, .device)
        else { return .unreadable }
        switch SyncedCredentialPolicy.decide(remote: remote, local: local) {
        case .nothing: return .unavailable
        case .adoptRemote: return .adoptedRemote
        case .unchanged: return .unchanged
        case .conflict: return .conflict
        case let .seedRemote(local):
            return (try? store.write(local, account: account, scope: .synced).get()) != nil
                ? .seededRemote : .unavailable
        }
    }

    /// Flips the sync switch. ON reconciles every account; OFF copies synced values to the
    /// device-only scope and leaves iCloud untouched. If any synced value cannot be read or copied
    /// down, the switch is NOT turned off (so reads keep seeing the synced copy) and that account is
    /// reported `.unavailable`.
    @discardableResult
    public func setSyncEnabled(_ enabled: Bool, accounts: [String]) -> [String: CredentialSyncOutcome] {
        if enabled {
            defaults.set(true, forKey: enabledDefaultsKey)
            if let consent { _ = store.write(consent.marker, account: consent.account, scope: .synced) }
            var outcomes: [String: CredentialSyncOutcome] = [:]
            for account in accounts { outcomes[account] = reconcile(account) }
            return outcomes
        }
        var failed: [String: CredentialSyncOutcome] = [:]
        for account in accounts {
            switch value(account, .synced) {
            case .success(nil): continue
            case let .success(synced?):
                if case .failure = store.write(synced, account: account, scope: .device) { failed[account] = .unavailable }
            case .failure: failed[account] = .unavailable
            }
        }
        if failed.isEmpty { defaults.set(false, forKey: enabledDefaultsKey) }
        return failed
    }

    /// Reads through the policy: synchronized (falling back to a not-yet-seeded device value)
    /// when sync is on, device-only when off.
    public func read(_ account: String) -> String? {
        if isSyncEnabled { return lenientValue(account, .synced) ?? lenientValue(account, .device) }
        return lenientValue(account, .device)
    }

    /// Writes through the policy. Blank input removes the credential everywhere.
    public func write(_ raw: String, account: String) -> Result<Void, AppleCredentialError> {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return store.deleteEverywhere(account) }
        return store.write(trimmed, account: account, scope: isSyncEnabled ? .synced : .device)
    }
}

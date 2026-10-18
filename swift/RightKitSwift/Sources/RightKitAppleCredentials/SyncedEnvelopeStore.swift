import Foundation

/// Redacting wrapper so a secret carried in an enum payload never prints in a log, assertion
/// message or debugger description.
public struct SyncedSecret: Equatable, Sendable, CustomStringConvertible, CustomDebugStringConvertible {
    public let value: String
    public init(value: String) { self.value = value }
    public var description: String { "<redacted>" }
    public var debugDescription: String { "<redacted>" }
}

/// The JSON value a revisioned credential keeps in the synchronizable Keychain:
/// `{v, app, provider, credentialId, revision, updatedAt, state: active|deleted, secret}`.
/// A tombstone (`deleted`) carries an explicit JSON `null` secret and a higher revision, so other
/// devices learn of a deletion instead of resurrecting the key.
public struct SyncedCredentialEnvelope: Codable, Equatable, Sendable, CustomStringConvertible {
    public enum State: String, Codable, Sendable { case active, deleted }

    public static let currentVersion = 1

    public var v: Int
    public var app: String
    public var provider: String
    public var credentialId: String
    public var revision: Int
    public var updatedAt: String
    public var state: State
    public var secret: String?

    public init(
        v: Int = SyncedCredentialEnvelope.currentVersion, app: String, provider: String, credentialId: String,
        revision: Int, updatedAt: String, state: State, secret: String?
    ) {
        self.v = v; self.app = app; self.provider = provider; self.credentialId = credentialId
        self.revision = revision; self.updatedAt = updatedAt; self.state = state; self.secret = secret
    }

    enum CodingKeys: String, CodingKey { case v, app, provider, credentialId, revision, updatedAt, state, secret }

    /// Encodes `secret` as an explicit JSON null for a tombstone.
    public func encode(to encoder: Encoder) throws {
        var values = encoder.container(keyedBy: CodingKeys.self)
        try values.encode(v, forKey: .v)
        try values.encode(app, forKey: .app)
        try values.encode(provider, forKey: .provider)
        try values.encode(credentialId, forKey: .credentialId)
        try values.encode(revision, forKey: .revision)
        try values.encode(updatedAt, forKey: .updatedAt)
        try values.encode(state, forKey: .state)
        try values.encode(secret, forKey: .secret)
    }

    /// Never includes the secret.
    public var description: String { "envelope(provider: \(provider), revision: \(revision), state: \(state.rawValue))" }

    /// False for an unknown version, wrong app or provider, a malformed id or revision, an active
    /// record without a secret, or a tombstone with one.
    public func isValid(app expectedApp: String, provider expectedProvider: String) -> Bool {
        guard v == Self.currentVersion, app == expectedApp, provider == expectedProvider,
              UUID(uuidString: credentialId) != nil, revision >= 1
        else { return false }
        let trimmed = secret?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        switch state {
        case .active: return !trimmed.isEmpty
        case .deleted: return secret == nil
        }
    }
}

/// Failures of ``SyncedEnvelopeStore`` writes.
public enum SyncedEnvelopeError: Error, Equatable, Sendable, LocalizedError {
    /// The iCloud item could not be read or understood, so it is left untouched.
    case unreadableRemote
    case emptySecret
    case keychain(String)

    public var errorDescription: String? {
        switch self {
        case .unreadableRemote: return "The iCloud copy could not be read, so it was left unchanged."
        case .emptySecret: return "The key is empty."
        case let .keychain(detail): return detail
        }
    }
}

/// Revisioned provider-key sync through the synchronizable Keychain, with tombstones and a
/// per-subject applied revision so an older copy is never adopted or re-published (rightkit#553;
/// ported from CodeRight's `AppleSyncedCredentialStore`). It owns only the iCloud side and the
/// applied-revision bookkeeping: the caller applies a ``Pull`` to its device-only store and then calls
/// ``markApplied(_:revision:)``. Consistent with ``SyncedCredentialReconciler``: a store whose sync
/// switch is absent or `false` never touches the Keychain, turning sync off never deletes the iCloud
/// copy, and an unreadable item is never overwritten.
public struct SyncedEnvelopeStore: Sendable {
    /// One synced credential: its Keychain `account` and the `provider` id written into (and checked
    /// against) the envelope, e.g. account `api.anthropic.v1`, provider `anthropic`.
    public struct Subject: Hashable, Sendable {
        public let account: String
        public let provider: String
        public init(account: String, provider: String) { self.account = account; self.provider = provider }
    }

    /// What to do with the iCloud copy given the device's local key.
    public enum Pull: Equatable, Sendable {
        case nothing
        /// Install this secret locally, then `markApplied`.
        case adopt(SyncedSecret, revision: Int)
        /// Erase the local key (the user deleted it elsewhere), then `markApplied`.
        case erase(revision: Int)
        /// Both exist and differ; neither is changed. The UI reports it.
        case conflict
        /// Undecodable, unknown version, mismatched app/provider, or a Keychain failure. Never treated as "no key".
        case unreadable
    }

    public enum PublishResult: Equatable, Sendable {
        case syncOff
        case unchanged
        case written(revision: Int)
    }

    public let store: AppleCredentialStore
    public let appID: String
    public let enabledDefaultsKey: String
    public let appliedRevisionKeyPrefix: String
    // UserDefaults is documented thread-safe but not declared Sendable; immutable and used only through those accessors.
    nonisolated(unsafe) private let defaults: UserDefaults
    private let now: @Sendable () -> Date

    /// - Parameters:
    ///   - appID: written into every envelope and required on read (`"coderight"`).
    ///   - enabledDefaultsKey: the one user-facing switch; absent means OFF.
    ///   - appliedRevisionKeyPrefix: defaults to `"<appID>.sync.appliedRevision."`.
    public init(
        store: AppleCredentialStore, defaults: UserDefaults = .standard, appID: String,
        enabledDefaultsKey: String? = nil, appliedRevisionKeyPrefix: String? = nil,
        now: @escaping @Sendable () -> Date = { Date() }
    ) {
        self.store = store; self.defaults = defaults; self.appID = appID
        self.enabledDefaultsKey = enabledDefaultsKey ?? "\(appID).sync.apple.enabled"
        self.appliedRevisionKeyPrefix = appliedRevisionKeyPrefix ?? "\(appID).sync.appliedRevision."
        self.now = now
    }

    /// No cloud write may happen before the user has opted in.
    public var isSyncEnabled: Bool {
        SyncedCredentialPolicy.isSyncEnabled(stored: defaults.object(forKey: enabledDefaultsKey) as? Bool)
    }

    // MARK: Applied revision

    /// Highest revision this device has applied, published or seen. Device-local.
    public func appliedRevision(_ subject: Subject) -> Int {
        defaults.integer(forKey: appliedRevisionKeyPrefix + subject.provider)
    }

    /// Records that this device now holds `revision`. Only ever moves forward.
    public func markApplied(_ subject: Subject, revision: Int) {
        guard revision > appliedRevision(subject) else { return }
        defaults.set(revision, forKey: appliedRevisionKeyPrefix + subject.provider)
    }

    // MARK: Pull

    /// Passive presence check (active, tombstone or unreadable); reads no secret bytes. False when sync is off.
    public func hasRemoteItem(_ subject: Subject) -> Bool {
        guard isSyncEnabled else { return false }
        return remoteItemExists(subject)
    }

    /// `.nothing` when sync is off, nothing is stored, or the stored revision is not newer than one
    /// already applied (no resurrection).
    public func pull(_ subject: Subject, local: String?) -> Pull {
        guard isSyncEnabled else { return .nothing }
        let applied = appliedRevision(subject)
        switch readRemote(subject) {
        case .absent: return .nothing
        case .unreadable: return .unreadable
        case let .deleted(envelope):
            return envelope.revision > applied ? .erase(revision: envelope.revision) : .nothing
        case let .active(envelope):
            guard envelope.revision > applied else { return .nothing }
            let remoteSecret = (envelope.secret ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            guard let local else { return .adopt(SyncedSecret(value: remoteSecret), revision: envelope.revision) }
            if local.trimmingCharacters(in: .whitespacesAndNewlines) == remoteSecret {
                markApplied(subject, revision: envelope.revision)
                return .nothing
            }
            return .conflict
        }
    }

    /// The outcome vocabulary shared with ``SyncedCredentialReconciler`` for a ``Pull`` the caller applied.
    public static func outcome(for pull: Pull) -> CredentialSyncOutcome {
        switch pull {
        case .nothing: return .unchanged
        case .adopt: return .adoptedRemote
        case .erase: return .erased
        case .conflict: return .conflict
        case .unreadable: return .unreadable
        }
    }

    // MARK: Publish

    /// Publishes `secret` as a new active revision above anything seen locally or in iCloud. Never
    /// overwrites an unreadable item. With `onlyIfChanged`, an identical active copy is left alone.
    @discardableResult
    public func publish(_ secret: String, subject: Subject, onlyIfChanged: Bool = false) throws -> PublishResult {
        guard isSyncEnabled else { return .syncOff }
        let trimmed = secret.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { throw SyncedEnvelopeError.emptySecret }
        let applied = appliedRevision(subject)
        let credentialId: String
        let base: Int
        switch readRemote(subject) {
        case .unreadable:
            throw SyncedEnvelopeError.unreadableRemote
        case .absent:
            credentialId = UUID().uuidString
            base = applied
        case let .active(envelope):
            if onlyIfChanged, envelope.secret == trimmed {
                markApplied(subject, revision: envelope.revision)
                return .unchanged
            }
            credentialId = envelope.credentialId
            base = max(applied, envelope.revision)
        case let .deleted(envelope):
            credentialId = UUID().uuidString
            base = max(applied, envelope.revision)
        }
        let revision = base + 1
        try writeEnvelope(makeEnvelope(subject, credentialId: credentialId, revision: revision, state: .active, secret: trimmed), subject: subject)
        markApplied(subject, revision: revision)
        return .written(revision: revision)
    }

    /// The user explicitly deleted this key. Replaces an active iCloud item with a tombstone at a
    /// higher revision; never removes the item. Writes only if sync is on or an item already exists,
    /// so deleting never creates cloud data. Returns true when a tombstone was written.
    @discardableResult
    public func publishTombstone(_ subject: Subject) throws -> Bool {
        guard isSyncEnabled || remoteItemExists(subject) else { return false }
        let applied = appliedRevision(subject)
        switch readRemote(subject) {
        case .absent:
            return false
        case let .deleted(envelope):
            markApplied(subject, revision: envelope.revision)
            return false
        case .unreadable:
            throw SyncedEnvelopeError.unreadableRemote
        case let .active(envelope):
            let revision = max(applied, envelope.revision) + 1
            try writeEnvelope(makeEnvelope(subject, credentialId: envelope.credentialId, revision: revision, state: .deleted, secret: nil), subject: subject)
            markApplied(subject, revision: revision)
            return true
        }
    }

    // MARK: Internals

    private enum Remote {
        case absent
        case active(SyncedCredentialEnvelope)
        case deleted(SyncedCredentialEnvelope)
        case unreadable
    }

    private func makeEnvelope(_ subject: Subject, credentialId: String, revision: Int, state: SyncedCredentialEnvelope.State, secret: String?) -> SyncedCredentialEnvelope {
        SyncedCredentialEnvelope(
            app: appID, provider: subject.provider, credentialId: credentialId, revision: revision,
            updatedAt: ISO8601DateFormatter().string(from: now()), state: state, secret: secret)
    }

    private func remoteItemExists(_ subject: Subject) -> Bool {
        if case .success(true) = store.exists(subject.account, scope: .synced) { return true }
        return false
    }

    private func readRemote(_ subject: Subject) -> Remote {
        switch store.read(subject.account, scope: .synced) {
        case .failure:
            return .unreadable
        case let .success(text):
            guard let text else {
                // Bytes that are not UTF-8 read back as nil; presence still means unreadable.
                return remoteItemExists(subject) ? .unreadable : .absent
            }
            guard let envelope = try? JSONDecoder().decode(SyncedCredentialEnvelope.self, from: Data(text.utf8)),
                  envelope.isValid(app: appID, provider: subject.provider)
            else { return .unreadable }
            return envelope.state == .active ? .active(envelope) : .deleted(envelope)
        }
    }

    private func writeEnvelope(_ envelope: SyncedCredentialEnvelope, subject: Subject) throws {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        guard let data = try? encoder.encode(envelope), let text = String(data: data, encoding: .utf8) else {
            throw SyncedEnvelopeError.keychain("The key could not be encoded for sync.")
        }
        if case let .failure(error) = store.write(text, account: subject.account, scope: .synced) {
            throw SyncedEnvelopeError.keychain(error.localizedDescription)
        }
    }
}

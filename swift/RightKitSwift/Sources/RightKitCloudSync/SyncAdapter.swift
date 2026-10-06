import Foundation

/// What a decoded remote record means.
public enum SyncRemoteChange<Item: Sendable>: Sendable {
    case upsert(Item)
    /// `version` is the deletion time (or best known update time).
    case tombstone(itemID: String, version: Date)
}

public enum SyncConflictPolicy: Sendable {
    /// Newer `version(of:)` wins; equal versions change nothing.
    case lastWriterWins
    /// Any version difference takes the server copy.
    case preferRemote
    /// Any version difference overwrites the server copy.
    case preferLocal
}

public enum SyncResolution: Sendable {
    case uploadLocal
    case applyRemote
    case keepBoth
}

/// The app's whole contribution: record mapping plus local persistence. One adapter per synced
/// record type. Everything else (zone, subscription, tokens, tombstones, retries, triggers) is generic.
///
/// Tombstones always win over live copies and permanently block re-upload and re-download of that
/// item id (the rule shared by the iOS and desktop sync implementations).
public protocol CloudSyncAdapter: Sendable {
    associatedtype Item: Sendable

    // MARK: Mapping
    var recordType: String { get }
    var conflictPolicy: SyncConflictPolicy { get }

    func itemID(of item: Item) -> String
    /// Monotonic edit time used for conflict resolution.
    func version(of item: Item) -> Date
    /// Immutable origin time; used to publish oldest-first.
    func createdAt(of item: Item) -> Date
    /// Record name for a new item. Defaults to the item id.
    func recordName(forItemID id: String) -> String
    /// Resolve a hard-deleted record name that is not a known alias. `nil` quarantines it.
    func itemID(forDeletedRecordName name: String) -> String?

    func encode(_ item: Item) -> [String: SyncValue]
    /// Throw for malformed records: they are quarantined (counted, never fatal) and retried when
    /// the server modifies them again.
    func decode(_ record: SyncRecord) throws -> SyncRemoteChange<Item>
    /// Fields written over a record to delete it: set the deletion marker and clear content with `.null`.
    func tombstoneFields(itemID: String, deletedAt: Date) -> [String: SyncValue]

    // MARK: Conflicts
    func resolve(local: Item, remote: Item) -> SyncResolution

    // MARK: Local persistence
    func loadLocal() async throws -> [Item]
    /// Persist remote changes durably. Throw when the write did not stick: the engine then keeps the
    /// change token where it was so the page replays. Must not clobber a local item that became
    /// newer while the sync was suspended.
    func apply(upserts: [Item], deletedIDs: Set<String>) async throws

    // MARK: Account isolation
    /// The iCloud account changed (`previous` is nil for a ledger that predates account binding).
    /// The engine has NOT yet reset its ledger: throw to abort and retry. Decide what happens to
    /// local items that belong to the previous account (purge them, or keep them to be uploaded to
    /// the new account) before returning. After this returns the engine drops the old account's
    /// token, versions, aliases and tombstones and replays the new account's zone from the start.
    func accountDidSwitch(from previous: String, to current: String) async throws
}

public extension CloudSyncAdapter {
    var conflictPolicy: SyncConflictPolicy { .lastWriterWins }
    func recordName(forItemID id: String) -> String { id }
    func itemID(forDeletedRecordName name: String) -> String? { name }
    func accountDidSwitch(from previous: String, to current: String) async throws {}

    func resolve(local: Item, remote: Item) -> SyncResolution {
        let l = version(of: local), r = version(of: remote)
        switch conflictPolicy {
        case .lastWriterWins:
            if r < l { return .uploadLocal }
            if r > l { return .applyRemote }
            return .keepBoth
        case .preferRemote: return r == l ? .keepBoth : .applyRemote
        case .preferLocal: return r == l ? .keepBoth : .uploadLocal
        }
    }
}

/// Convenience for the common tombstone shape.
public enum SyncTombstone {
    public static func fields(
        idKey: String = "id",
        itemID: String,
        deletedAt: Date,
        deletedAtKey: String = "deletedAt",
        updatedAtKey: String = "updatedAt",
        clearing contentKeys: [String]
    ) -> [String: SyncValue] {
        var fields: [String: SyncValue] = [
            idKey: .string(itemID),
            updatedAtKey: .date(deletedAt),
            deletedAtKey: .date(deletedAt),
        ]
        for key in contentKeys { fields[key] = SyncValue.null }
        return fields
    }
}

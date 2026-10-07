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

/// The app's whole contribution: record mapping plus local persistence. Everything else (zone,
/// subscription, tokens, tombstones, retries, triggers) is generic.
///
/// Record types: a single-type adapter only declares `recordType`. An adapter that owns several
/// ledger types returns the type per item from `recordType(of:)` / `recordType(forItemID:)`.
/// Types the app writes directly (through `CloudSyncEngine.writeDirect` or its own transport
/// calls) and never uploads from `loadLocal` go in `directRecordTypes`: the engine decodes them
/// from the feed and hands every changed upsert to `apply`, but keeps them out of conflict
/// resolution, the version cache, aliases and tombstones.
///
/// Tombstones always win over live copies and permanently block re-upload and re-download of that
/// item id (the rule shared by the iOS and desktop sync implementations).
public protocol CloudSyncAdapter: Sendable {
    associatedtype Item: Sendable

    // MARK: Mapping
    /// The default record type: used for every upload and tombstone unless the per-item hooks
    /// below say otherwise.
    var recordType: String { get }
    /// Record type an item is uploaded as. Defaults to `recordType`.
    func recordType(of item: Item) -> String
    /// Record type a tombstone for `id` is written as. Defaults to `recordType`.
    func recordType(forItemID id: String) -> String
    /// Record types read from the zone but never uploaded from `loadLocal` and never tombstoned
    /// (e.g. request-queue or device records written directly). Defaults to empty.
    var directRecordTypes: Set<String> { get }
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
    /// Fields for an upload, produced at upload time. Defaults to `encode(_:)`. Implement it when
    /// `loadLocal` returns items whose payload is lazy (a file URL, a closure, `SyncLazyPayload`):
    /// it is called once per uploaded item, batch by batch, so a pass that uploads nothing never
    /// reads payloads. Throwing fails only that item (counted as a failed upload and retried);
    /// it is never uploaded with missing content.
    func encodeForUpload(_ item: Item) async throws -> [String: SyncValue]
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
    func recordType(of item: Item) -> String { recordType }
    func recordType(forItemID id: String) -> String { recordType }
    var directRecordTypes: Set<String> { [] }
    func encodeForUpload(_ item: Item) async throws -> [String: SyncValue] { encode(item) }
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

/// A payload produced on demand, for items whose content is too large to hold for every local
/// item on every pass. `loadLocal` returns items carrying one; `encodeForUpload` resolves it.
public struct SyncLazyPayload: Sendable {
    private let produce: @Sendable () async throws -> [String: SyncValue]

    public init(_ produce: @escaping @Sendable () async throws -> [String: SyncValue]) {
        self.produce = produce
    }

    /// Already-materialised fields (whole-item loading).
    public static func fields(_ fields: [String: SyncValue]) -> SyncLazyPayload {
        SyncLazyPayload { fields }
    }

    public enum FileEncoding: Sendable {
        /// UTF-8 text as `.string`.
        case utf8String
        /// Raw bytes as `.data`.
        case data
        /// Raw bytes as a CloudKit asset (`.asset`).
        case asset
    }

    /// One field read from `url` at encode time. An unreadable file throws, so the item is not
    /// uploaded with empty content.
    public static func file(_ url: URL, field: String, as encoding: FileEncoding) -> SyncLazyPayload {
        SyncLazyPayload {
            let bytes = try Data(contentsOf: url)
            switch encoding {
            case .utf8String:
                guard let text = String(data: bytes, encoding: .utf8) else {
                    throw CloudSyncError.malformedRecord("payload at \(url.lastPathComponent) is not UTF-8")
                }
                return [field: .string(text)]
            case .data: return [field: .data(bytes)]
            case .asset: return [field: .asset(bytes)]
            }
        }
    }

    public func resolve() async throws -> [String: SyncValue] { try await produce() }

    /// `base` fields overlaid with this payload's fields (payload wins on a key collision).
    public func merged(over base: [String: SyncValue]) async throws -> [String: SyncValue] {
        base.merging(try await resolve()) { _, payload in payload }
    }
}

import Foundation
@testable import RightKitCloudSync

struct Note: Equatable, Sendable {
    var id: String
    var text: String
    var updatedAt: Date
    var createdAt: Date
}

func t(_ seconds: Double) -> Date { Date(timeIntervalSince1970: 1_700_000_000 + seconds) }

/// An app adapter backed by an in-memory "database". `failApply` simulates a failed local write.
final class NoteAdapter: CloudSyncAdapter, @unchecked Sendable {
    typealias Item = Note
    private let lock = NSLock()
    private var rows: [String: Note] = [:]
    private var failApplyFlag = false
    private(set) var applyCalls = 0
    private(set) var switches: [String] = []
    /// What an app does with the previous account's local data on a switch.
    var purgeOnSwitch = false

    let recordType = "Note"
    var notes: [String: Note] { lock.withLock { rows } }
    func put(_ note: Note) { lock.withLock { rows[note.id] = note } }
    func remove(_ id: String) { lock.withLock { rows[id] = nil } }
    func failApply(_ fail: Bool) { lock.withLock { failApplyFlag = fail } }

    func itemID(of item: Note) -> String { item.id }
    func version(of item: Note) -> Date { item.updatedAt }
    func createdAt(of item: Note) -> Date { item.createdAt }

    func encode(_ item: Note) -> [String: SyncValue] {
        ["id": .string(item.id), "text": .string(item.text), "updatedAt": .date(item.updatedAt),
         "createdAt": .date(item.createdAt), "deletedAt": .null]
    }

    func decode(_ record: SyncRecord) throws -> SyncRemoteChange<Note> {
        guard let id = record.fields["id"]?.stringValue, let updated = record.fields["updatedAt"]?.dateValue else {
            throw CloudSyncError.malformedRecord(record.recordName)
        }
        if let deleted = record.fields["deletedAt"]?.dateValue { return .tombstone(itemID: id, version: deleted) }
        guard let text = record.fields["text"]?.stringValue, let created = record.fields["createdAt"]?.dateValue else {
            throw CloudSyncError.malformedRecord(record.recordName)
        }
        return .upsert(Note(id: id, text: text, updatedAt: updated, createdAt: created))
    }

    func tombstoneFields(itemID: String, deletedAt: Date) -> [String: SyncValue] {
        SyncTombstone.fields(itemID: itemID, deletedAt: deletedAt, clearing: ["text", "createdAt"])
    }

    func accountDidSwitch(from previous: String, to current: String) async throws {
        lock.withLock {
            switches.append("\(previous)->\(current)")
            if purgeOnSwitch { rows = [:] }
        }
    }

    func loadLocal() async throws -> [Note] { Array(notes.values) }

    func apply(upserts: [Note], deletedIDs: Set<String>) async throws {
        try lock.withLock {
            applyCalls += 1
            if failApplyFlag { throw CloudSyncError.persistence("disk full") }
            for note in upserts { rows[note.id] = note }
            for id in deletedIDs { rows[id] = nil }
        }
    }
}

struct Device {
    let adapter = NoteAdapter()
    let enabled = InMemorySyncEnabledStore(enabled: true)
    let store = InMemorySyncStateStore()
    let engine: CloudSyncEngine<NoteAdapter>

    init(transport: InMemorySyncTransport, configuration: CloudSyncConfiguration = CloudSyncConfiguration()) {
        var config = configuration
        config.clock = { t(0) }
        engine = CloudSyncEngine(
            adapter: adapter, transport: transport, stateStore: store, enabledStore: enabled, configuration: config
        )
    }
}

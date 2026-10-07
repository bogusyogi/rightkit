import Foundation
import Testing
@testable import RightKitCloudSync

// 0.3.3 additions: read-one-record, several record types per adapter (including direct types),
// a retry gate the app can raise, legacy-ledger adoption without an account call, lazy payloads,
// and `SyncTrigger.submit`.

// MARK: - A multi-type adapter shaped like ScrapeRight's

enum Entry: Equatable, Sendable {
    /// Ledger types: uploaded from `loadLocal`, versioned, tombstoned.
    case note(Note)
    case pin(id: String, label: String, updatedAt: Date)
    /// Direct types: written by the app (or another device), never uploaded from the ledger.
    case request(id: String, status: String)
    case device(id: String)
}

final class MultiAdapter: CloudSyncAdapter, @unchecked Sendable {
    typealias Item = Entry
    private let lock = NSLock()
    private var rows: [String: Entry] = [:]
    private(set) var applied: [Entry] = []
    private(set) var appliedDeletes: Set<String> = []

    let recordType = "Note"
    var directRecordTypes: Set<String> { ["QueueRequest", "Device"] }

    func put(_ entry: Entry) { lock.withLock { rows[itemID(of: entry)] = entry } }
    func remove(_ id: String) { lock.withLock { rows[id] = nil } }
    var appliedEntries: [Entry] { lock.withLock { applied } }

    func recordType(of item: Entry) -> String {
        switch item {
        case .note: "Note"
        case .pin: "Pin"
        case .request: "QueueRequest"
        case .device: "Device"
        }
    }

    func recordType(forItemID id: String) -> String { id.hasPrefix("pin-") ? "Pin" : "Note" }

    func itemID(of item: Entry) -> String {
        switch item {
        case let .note(n): n.id
        case let .pin(id, _, _): id
        case let .request(id, _): id
        case let .device(id): id
        }
    }

    func version(of item: Entry) -> Date {
        switch item {
        case let .note(n): n.updatedAt
        case let .pin(_, _, d): d
        default: t(0)
        }
    }

    func createdAt(of item: Entry) -> Date { version(of: item) }

    func encode(_ item: Entry) -> [String: SyncValue] {
        switch item {
        case let .note(n): ["id": .string(n.id), "text": .string(n.text), "updatedAt": .date(n.updatedAt), "deletedAt": .null]
        case let .pin(id, label, d): ["id": .string(id), "label": .string(label), "updatedAt": .date(d), "deletedAt": .null]
        case let .request(id, status): ["id": .string(id), "status": .string(status)]
        case let .device(id): ["id": .string(id)]
        }
    }

    func decode(_ record: SyncRecord) throws -> SyncRemoteChange<Entry> {
        guard let id = record.fields["id"]?.stringValue else { throw CloudSyncError.malformedRecord(record.recordName) }
        switch record.recordType {
        case "QueueRequest":
            // A cancelled request decodes as a tombstone: it must NOT bury the id forever.
            if record.fields["status"]?.stringValue == "cancelled" { return .tombstone(itemID: id, version: t(0)) }
            return .upsert(.request(id: id, status: record.fields["status"]?.stringValue ?? ""))
        case "Device": return .upsert(.device(id: id))
        default:
            guard let updated = record.fields["updatedAt"]?.dateValue else { throw CloudSyncError.malformedRecord(id) }
            if let deleted = record.fields["deletedAt"]?.dateValue { return .tombstone(itemID: id, version: deleted) }
            if record.recordType == "Pin" {
                return .upsert(.pin(id: id, label: record.fields["label"]?.stringValue ?? "", updatedAt: updated))
            }
            return .upsert(.note(Note(id: id, text: record.fields["text"]?.stringValue ?? "", updatedAt: updated, createdAt: updated)))
        }
    }

    func tombstoneFields(itemID: String, deletedAt: Date) -> [String: SyncValue] {
        SyncTombstone.fields(itemID: itemID, deletedAt: deletedAt, clearing: ["text", "label"])
    }

    func loadLocal() async throws -> [Entry] {
        lock.withLock { Array(rows.values.filter { if case .note = $0 { true } else if case .pin = $0 { true } else { false } }) }
    }

    func apply(upserts: [Entry], deletedIDs: Set<String>) async throws {
        lock.withLock {
            applied += upserts
            appliedDeletes.formUnion(deletedIDs)
            for e in upserts {
                switch e {
                case .note, .pin: rows[itemID(of: e)] = e
                default: break
                }
            }
            for id in deletedIDs { rows[id] = nil }
        }
    }
}

private func multiEngine(_ zone: InMemorySyncTransport, adapter: MultiAdapter, store: InMemorySyncStateStore = InMemorySyncStateStore(),
                         enabled: InMemorySyncEnabledStore = InMemorySyncEnabledStore(enabled: true)) -> CloudSyncEngine<MultiAdapter> {
    var config = CloudSyncConfiguration()
    config.clock = { t(0) }
    return CloudSyncEngine(adapter: adapter, transport: zone, stateStore: store, enabledStore: enabled, configuration: config)
}

/// A transport written against 0.3.2: no `fetchRecord`.
private struct PreFetchRecordTransport: SyncTransport {
    func accountState() async throws -> SyncAccountState { .available }
    func prepareZone() async throws {}
    func ensureSubscription() async throws {}
    func fetchChanges(since token: SyncChangeToken?) async throws -> SyncChangePage {
        SyncChangePage(records: [], deletedRecordNames: [], token: SyncChangeToken(data: Data()), moreComing: false)
    }
    func save(_ writes: [SyncRecordWrite]) async throws -> [String: SyncSaveOutcome] { [:] }
}

// MARK: - Lazy payload adapter

struct FileNote: Sendable {
    let id: String
    let updatedAt: Date
    let body: SyncLazyPayload
}

final class LazyAdapter: CloudSyncAdapter, @unchecked Sendable {
    typealias Item = FileNote
    private let lock = NSLock()
    private var rows: [String: FileNote] = [:]
    private var encodes = 0
    let recordType = "FileNote"
    var encodeCount: Int { lock.withLock { encodes } }
    func put(_ note: FileNote) { lock.withLock { rows[note.id] = note } }

    func itemID(of item: FileNote) -> String { item.id }
    func version(of item: FileNote) -> Date { item.updatedAt }
    func createdAt(of item: FileNote) -> Date { item.updatedAt }
    /// Header only; content comes from `encodeForUpload`.
    func encode(_ item: FileNote) -> [String: SyncValue] { ["id": .string(item.id), "updatedAt": .date(item.updatedAt)] }
    func encodeForUpload(_ item: FileNote) async throws -> [String: SyncValue] {
        lock.withLock { encodes += 1 }
        return try await item.body.merged(over: encode(item))
    }
    func decode(_ record: SyncRecord) throws -> SyncRemoteChange<FileNote> {
        guard let id = record.fields["id"]?.stringValue, let d = record.fields["updatedAt"]?.dateValue else {
            throw CloudSyncError.malformedRecord(record.recordName)
        }
        return .upsert(FileNote(id: id, updatedAt: d, body: .fields(record.fields)))
    }
    func tombstoneFields(itemID: String, deletedAt: Date) -> [String: SyncValue] {
        SyncTombstone.fields(itemID: itemID, deletedAt: deletedAt, clearing: ["text"])
    }
    func loadLocal() async throws -> [FileNote] { lock.withLock { Array(rows.values) } }
    func apply(upserts: [FileNote], deletedIDs: Set<String>) async throws {}
}

@Suite struct Engine033Tests {
    // MARK: 1. read one record

    @Test func inMemoryTransportReadsOneRecordOrNil() async throws {
        let zone = InMemorySyncTransport()
        await #expect(throws: SyncTransportError.zoneNotFound) { _ = try await zone.fetchRecord(named: "x") }
        await zone.seed(SyncRecord(recordName: "r1", recordType: "Note", fields: ["id": .string("r1")]))
        #expect(try await zone.fetchRecord(named: "r1")?.fields["id"] == .string("r1"))
        #expect(try await zone.fetchRecord(named: "missing") == nil)
        await zone.setFailNextRecordFetch(.busy(retryAfter: 40))
        await #expect(throws: SyncTransportError.busy(retryAfter: 40)) { _ = try await zone.fetchRecord(named: "r1") }
        #expect(try await zone.fetchRecord(named: "r1") != nil, "the failure is one-shot")
        #expect(await zone.recordFetchCount == 5)
    }

    @Test func transportsWrittenBefore033ThrowUnsupportedInsteadOfReportingAbsence() async {
        do {
            _ = try await PreFetchRecordTransport().fetchRecord(named: "x")
            Issue.record("expected unsupported")
        } catch let error as SyncTransportError {
            guard case let .other(code, _) = error else { Issue.record("wrong error \(error)"); return }
            #expect(code == SyncTransportError.unsupportedOperationCode)
        } catch { Issue.record("wrong error \(error)") }
    }

    @Test func engineReadFeedsTheRetryGateAndRespectsOff() async throws {
        let zone = InMemorySyncTransport()
        let a = Device(transport: zone)
        a.adapter.put(Note(id: "n1", text: "one", updatedAt: t(1), createdAt: t(1)))
        _ = await a.engine.sync()
        #expect(try await a.engine.fetchRecord(named: "n1")?.fields["text"] == .string("one"))
        #expect(try await a.engine.fetchRecord(named: "nope") == nil)
        await zone.setFailNextRecordFetch(.busy(retryAfter: 90))
        await #expect(throws: SyncTransportError.self) { _ = try await a.engine.fetchRecord(named: "n1") }
        #expect((await a.engine.retryDelayRemaining() ?? 0) >= 90)

        await a.engine.setEnabled(false)
        let before = await zone.recordFetchCount
        await #expect(throws: CancellationError.self) { _ = try await a.engine.fetchRecord(named: "n1") }
        #expect(await zone.recordFetchCount == before, "OFF: nothing leaves the device")
    }

    // MARK: 2. several record types, direct types

    @Test func ledgerItemsUploadAndTombstoneUnderTheirOwnRecordTypes() async throws {
        let zone = InMemorySyncTransport()
        let adapter = MultiAdapter()
        let engine = multiEngine(zone, adapter: adapter)
        adapter.put(.note(Note(id: "n1", text: "hi", updatedAt: t(1), createdAt: t(1))))
        adapter.put(.pin(id: "pin-1", label: "home", updatedAt: t(2)))
        let r = await engine.sync()
        #expect(r.status == .ready && r.uploaded == 2)
        #expect(await zone.record("n1")?.recordType == "Note")
        #expect(await zone.record("pin-1")?.recordType == "Pin")

        adapter.remove("pin-1")
        try await engine.markDeleted("pin-1")
        _ = await engine.sync()
        let buried = await zone.record("pin-1")
        #expect(buried?.recordType == "Pin" && buried?.fields["deletedAt"]?.dateValue != nil)
    }

    @Test func directRecordsAreAppliedAsObservedAndNeverEnterTheLedger() async throws {
        let zone = InMemorySyncTransport()
        let phone = MultiAdapter(), mac = MultiAdapter()
        let phoneEngine = multiEngine(zone, adapter: phone)
        let macEngine = multiEngine(zone, adapter: mac)
        _ = await phoneEngine.sync()

        // The phone submits a request directly; the Mac announces itself directly.
        let outcomes = try await phoneEngine.writeDirect([
            SyncRecordWrite(recordName: "qr-1", recordType: "QueueRequest", fields: ["id": .string("qr-1"), "status": .string("queued")]),
        ])
        #expect(outcomes["qr-1"] == .saved)
        try await macEngine.writeDirect([SyncRecordWrite(recordName: "device-mac", recordType: "Device", fields: ["id": .string("device-mac")])])

        let macPass = await macEngine.sync()
        #expect(macPass.status == .ready && macPass.uploaded == 0)
        #expect(mac.appliedEntries.contains(.request(id: "qr-1", status: "queued")))

        // The Mac progresses the request; the phone sees each change, with no local counterpart.
        try await macEngine.writeDirect([
            SyncRecordWrite(recordName: "qr-1", recordType: "QueueRequest", fields: ["id": .string("qr-1"), "status": .string("running")]),
        ])
        let phonePass = await phoneEngine.sync()
        #expect(phonePass.downloaded == 2, "request + device, counted as new data")
        #expect(phone.appliedEntries.contains(.request(id: "qr-1", status: "running")))
        #expect(phone.appliedEntries.contains(.device(id: "device-mac")))
        let state = await phoneEngine.snapshot
        #expect(state.remoteVersions["qr-1"] == nil && state.aliases["qr-1"] == nil)
        #expect(state.remoteVersions["device-mac"] == nil && state.tombstones.isEmpty)

        // A direct record that decodes as a tombstone (cancelled) never buries its id, and a
        // hard delete of a direct record is neither a tombstone nor an unresolved deletion.
        try await phoneEngine.writeDirect([
            SyncRecordWrite(recordName: "qr-1", recordType: "QueueRequest", fields: ["status": .string("cancelled")]),
        ])
        await zone.hardDelete("device-mac")
        let after = await macEngine.sync()
        #expect(after.status == .ready)
        let macState = await macEngine.snapshot
        #expect(macState.tombstones.isEmpty && macState.quarantined.isEmpty)
        #expect(mac.appliedDeletes.isEmpty)

        // The same id can be re-announced later and is applied again.
        try await macEngine.writeDirect([SyncRecordWrite(recordName: "device-mac", recordType: "Device", fields: ["id": .string("device-mac")])])
        let before = phone.appliedEntries.filter { $0 == .device(id: "device-mac") }.count
        _ = await phoneEngine.sync()
        #expect(phone.appliedEntries.filter { $0 == .device(id: "device-mac") }.count == before + 1)
    }

    @Test func singleTypeAdaptersKeepTheirDefaults() {
        let adapter = NoteAdapter()
        #expect(adapter.directRecordTypes.isEmpty)
        #expect(adapter.recordType(of: Note(id: "x", text: "", updatedAt: t(0), createdAt: t(0))) == "Note")
        #expect(adapter.recordType(forItemID: "x") == "Note")
    }

    // MARK: 3. retry gate raised from outside

    @Test func appSideFailuresBackOffWithTheEngineAndTheCoordinator() async throws {
        let zone = InMemorySyncTransport()
        let a = Device(transport: zone)
        #expect(await a.engine.retryDelayRemaining() == nil)
        await a.engine.noteFailure(SyncTransportError.busy(retryAfter: 120))
        #expect((await a.engine.retryDelayRemaining() ?? 0) >= 120)
        // Non-retryable and non-transport errors never move the gate; it never lowers.
        await a.engine.noteFailure(SyncTransportError.quotaExceeded)
        await a.engine.noteFailure(CocoaError(.fileNoSuchFile))
        await a.engine.raiseRetryGate(toAtLeast: 10)
        #expect((await a.engine.retryDelayRemaining() ?? 0) >= 120)
        #expect(await a.engine.retryGate.notBefore == t(120))

        let coordinator = CloudSyncCoordinator(runner: a.engine, enabledStore: a.enabled, sleep: { _ in throw CancellationError() })
        let fetchesBefore = await zone.fetchCount
        let gated = await coordinator.request(.submit)
        #expect(gated.status == .unavailable(SyncMessages().retryWhenConnected))
        #expect(await zone.fetchCount == fetchesBefore, "the gated pass never reached the network")
    }

    @Test func directWriteFailuresRaiseTheGate() async throws {
        let zone = InMemorySyncTransport()
        let adapter = MultiAdapter()
        let engine = multiEngine(zone, adapter: adapter)
        _ = await engine.sync()
        await zone.setFailingSaves(["qr-1": .busy(retryAfter: 45)])
        let outcomes = try await engine.writeDirect([SyncRecordWrite(recordName: "qr-1", recordType: "QueueRequest", fields: ["id": .string("qr-1")])])
        #expect(outcomes["qr-1"] == .failed(.busy(retryAfter: 45)))
        #expect((await engine.retryDelayRemaining() ?? 0) >= 45)

        await zone.setFailNextSave(.networkUnavailable(retryAfter: 200))
        await #expect(throws: SyncTransportError.networkUnavailable(retryAfter: 200)) {
            try await engine.writeDirect([SyncRecordWrite(recordName: "qr-2", recordType: "QueueRequest", fields: ["id": .string("qr-2")])])
        }
        #expect((await engine.retryDelayRemaining() ?? 0) >= 200)

        await engine.setEnabled(false)
        let saves = await zone.saveCallCount
        await #expect(throws: CancellationError.self) {
            try await engine.writeDirect([SyncRecordWrite(recordName: "qr-3", recordType: "QueueRequest", fields: [:])])
        }
        #expect(await zone.saveCallCount == saves)
    }

    // MARK: 4. legacy ledger adoption without an account call

    @Test func adoptedLegacyLedgerKeepsItsCursorAndVersionsAndBindsOnFirstPass() async throws {
        let zone = InMemorySyncTransport()
        let old = Device(transport: zone)
        old.adapter.put(Note(id: "n1", text: "one", updatedAt: t(1), createdAt: t(1)))
        _ = await old.engine.sync()
        let legacyCursor = await old.engine.snapshot.changeToken
        #expect(legacyCursor != nil)

        func legacyLedger() -> CloudSyncState {
            var s = CloudSyncState()
            s.changeToken = legacyCursor
            s.remoteVersions["n1"] = .init(recordName: "n1", version: t(1))
            s.aliases["n1"] = ["n1"]
            s.tombstones["gone"] = t(0)
            s.pendingDeletions = ["gone"]
            return s
        }

        // Adopted: the imported cursor is a delta, so nothing is re-downloaded; versions keep n1
        // from being re-uploaded; the queued deletion is flushed; the account is bound.
        let adopted = Device(transport: zone)
        adopted.adapter.put(Note(id: "n1", text: "one", updatedAt: t(1), createdAt: t(1)))
        try await adopted.engine.adoptLegacyLedger(legacyLedger())
        #expect(await adopted.engine.snapshot.pendingLegacyAdoption)
        #expect(try adopted.store.load().pendingLegacyAdoption, "persisted before any pass")
        let r = await adopted.engine.sync()
        #expect(r.status == .ready && r.downloaded == 0 && r.uploaded == 0)
        let s = await adopted.engine.snapshot
        #expect(s.accountID == "in-memory-account" && !s.pendingLegacyAdoption)
        #expect(s.remoteVersions["n1"] != nil && s.pendingDeletions.isEmpty)
        #expect(await zone.record("gone")?.fields["deletedAt"]?.dateValue != nil)

        // The same unbound ledger WITHOUT adoption keeps today's rule: cursor and versions are
        // dropped at binding, so the zone replays from the start.
        let unflagged = Device(transport: zone)
        try unflagged.store.save(legacyLedger())
        let relaunched = CloudSyncEngine(adapter: unflagged.adapter, transport: zone, stateStore: unflagged.store, enabledStore: unflagged.enabled)
        let replay = await relaunched.sync()
        #expect(replay.downloaded >= 1)
        #expect(await relaunched.snapshot.pendingDeletions.isEmpty)
    }

    @Test func adoptingAnAlreadyBoundLedgerIsNotMarkedPending() async throws {
        let a = Device(transport: InMemorySyncTransport())
        var bound = CloudSyncState()
        bound.accountID = "in-memory-account"
        try await a.engine.adoptLegacyLedger(bound)
        #expect(await a.engine.snapshot.pendingLegacyAdoption == false)
    }

    @Test func stateWithoutTheAdoptionFieldStillDecodesAndTheFlagRoundTrips() throws {
        let old = #"{"tombstones":{},"markers":[],"pendingDeletions":[],"remoteVersions":{},"aliases":{}}"#
        let decoded = try JSONDecoder().decode(CloudSyncState.self, from: Data(old.utf8))
        #expect(!decoded.pendingLegacyAdoption)
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("cloudsync-\(UUID().uuidString)/state.json")
        var state = CloudSyncState()
        state.pendingLegacyAdoption = true
        try FileSyncStateStore(fileURL: url).save(state)
        #expect(try FileSyncStateStore(fileURL: url).load().pendingLegacyAdoption)
    }

    // MARK: 5. lazy payloads

    @Test func lazyPayloadsAreReadOnlyWhenUploadedAndAnUnreadableOneFailsOnlyThatItem() async throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("lazy-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let present = dir.appendingPathComponent("a.txt"), missing = dir.appendingPathComponent("b.txt")
        try Data("alpha".utf8).write(to: present)

        let zone = InMemorySyncTransport()
        let adapter = LazyAdapter()
        var config = CloudSyncConfiguration()
        config.clock = { t(0) }
        let store = InMemorySyncStateStore(), enabled = InMemorySyncEnabledStore(enabled: true)
        let engine = CloudSyncEngine(adapter: adapter, transport: zone, stateStore: store, enabledStore: enabled, configuration: config)
        adapter.put(FileNote(id: "a", updatedAt: t(1), body: .file(present, field: "text", as: .utf8String)))
        adapter.put(FileNote(id: "b", updatedAt: t(2), body: .file(missing, field: "text", as: .utf8String)))

        let first = await engine.sync()
        #expect(first.uploaded == 1 && first.status == .unavailable(SyncMessages().pendingChanges))
        #expect(await zone.record("a")?.fields["text"] == .string("alpha"))
        #expect(await zone.record("b") == nil, "never uploaded with missing content")

        try Data("beta".utf8).write(to: missing)
        let relaunched = CloudSyncEngine(adapter: adapter, transport: zone, stateStore: store, enabledStore: enabled, configuration: config)
        let second = await relaunched.sync()
        #expect(second.uploaded == 1 && second.status == .ready)
        #expect(await zone.record("b")?.fields["text"] == .string("beta"))

        // A pass with nothing to upload never produces a payload.
        let count = adapter.encodeCount
        _ = await relaunched.sync()
        #expect(adapter.encodeCount == count)
    }

    @Test func lazyPayloadHelpers() async throws {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("lazy-\(UUID().uuidString).bin")
        try Data([1, 2, 3]).write(to: url)
        #expect(try await SyncLazyPayload.file(url, field: "blob", as: .data).resolve() == ["blob": .data(Data([1, 2, 3]))])
        #expect(try await SyncLazyPayload.file(url, field: "blob", as: .asset).resolve() == ["blob": .asset(Data([1, 2, 3]))])
        let merged = try await SyncLazyPayload.fields(["k": .string("payload")]).merged(over: ["k": .string("base"), "h": .int(1)])
        #expect(merged == ["k": .string("payload"), "h": .int(1)])
        try Data([0xFF, 0xFE]).write(to: url)
        await #expect(throws: CloudSyncError.self) { _ = try await SyncLazyPayload.file(url, field: "t", as: .utf8String).resolve() }
    }

    // MARK: 6. submit trigger

    @Test func submitTriggerRunsAPass() async throws {
        #expect(SyncTrigger(rawValue: "submit") == .submit)
        let zone = InMemorySyncTransport()
        let a = Device(transport: zone)
        a.adapter.put(Note(id: "n1", text: "one", updatedAt: t(1), createdAt: t(1)))
        let coordinator = CloudSyncCoordinator(runner: a.engine, enabledStore: a.enabled)
        let r = await coordinator.request(.submit)
        #expect(r.status == .ready && r.uploaded == 1)
    }
}

import Foundation
import Testing

@testable import RightKitWatchBridge

private struct Take: Codable, Sendable { let seconds: Int }

private func entry(_ id: String) -> TransferLedgerEntry<Take> {
    TransferLedgerEntry(id: id, payload: Take(seconds: 3), fileName: "\(id).m4a")
}

@Test func deliveryIsClosedOnlyByAcknowledgementAndStrandedItemsAreResent() throws {
    let t0 = Date(timeIntervalSince1970: 100)
    var ledger = TransferLedger<Take>()
    let added = ledger.enqueue(entry("a"))
    let duplicate = ledger.enqueue(entry("a"))
    #expect(added && !duplicate)

    let first = ledger.claimDispatches(now: t0, activeIDs: [], fileExists: { _ in true })
    #expect(first.dispatches == [.init(id: "a", isRetry: false)])
    // In flight at the OS: never re-sent, whatever its age.
    let inFlight = ledger.claimDispatches(now: t0.addingTimeInterval(60), activeIDs: ["a"], fileExists: { _ in true })
    #expect(inFlight.dispatches.isEmpty)
    // Inside the registration debounce nothing is re-sent; past it the unlisted item is stranded.
    let debounced = ledger.claimDispatches(now: t0.addingTimeInterval(1), activeIDs: [], fileExists: { _ in true })
    let stranded = ledger.claimDispatches(now: t0.addingTimeInterval(4), activeIDs: [], fileExists: { _ in true })
    #expect(debounced.dispatches.isEmpty)
    #expect(stranded.dispatches == [.init(id: "a", isRetry: true)])

    ledger.transportFinished(id: "a", failed: true)
    #expect(ledger.entries[0].state == .queued)
    let rolled = ledger.rollback(id: "a")
    let unknown = ledger.rollback(id: "zzz")
    #expect(rolled && !unknown)

    // Persisted across a relaunch, then reconciled against what the OS still lists.
    var restored = TransferLedger<Take>(decoding: ledger.encoded())
    restored.restoreOutstanding(["a"], now: t0)
    #expect(restored.entries[0].state == .awaitingAcknowledgement)
    let missing = restored.claimDispatches(now: t0, activeIDs: [], fileExists: { _ in false })
    #expect(missing.missingFiles == ["a"])

    let acked = restored.acknowledge(id: "a")
    #expect(acked?.fileName == "a.m4a" && restored.pendingCount == 0)
}

@Test func discardCanBeUndoneButNotOverAReplacement() {
    var ledger = TransferLedger<Take>()
    ledger.enqueue(entry("a"))
    let removed = ledger.remove(id: "a")
    ledger.restore(removed)
    #expect(ledger.entries.map(\.id) == ["a"])
    let again = ledger.remove(id: "a")
    ledger.enqueue(entry("a"))
    ledger.restore(again)
    #expect(ledger.pendingCount == 1)
}

private final class FakeTransport: WatchMessageTransport {
    var isReachable: Bool
    var messages: [[String: Any]] = []
    var userInfos: [[String: Any]] = []
    init(reachable: Bool) { isReachable = reachable }
    func sendMessage(_ message: [String: Any], onError: @escaping (Error) -> Void) { messages.append(message) }
    func transferUserInfo(_ userInfo: [String: Any]) { userInfos.append(userInfo) }
}

@Test func senderAlwaysQueuesDurablyAndAddsImmediatePathOnlyWhenReachable() {
    for reachable in [true, false] {
        let transport = FakeTransport(reachable: reachable)
        let attempted = DualPathSender(transport: transport).send(["ack": "a"])
        #expect(attempted == reachable)
        #expect(transport.userInfos.count == 1 && transport.messages.count == (reachable ? 1 : 0))
    }
    #expect(WatchReceipts.confirmed(requested: ["a", "b", "c"], received: ["c", "a"]) == ["a", "c"])
    #expect(WatchReceipts.queryChunks((0..<250).map(String.init)).map(\.count) == [100, 100, 50])
    let defaults = UserDefaults(suiteName: "rk-bridge-\(UUID().uuidString)")!
    let log = BoundedStringLog(defaults: defaults, key: "k", limit: 2)
    #expect(log.insertIfNew("x") && !log.insertIfNew("x"))
    log.remember("y"); log.remember("z")
    #expect(log.all == ["z", "y"])
}

@Test func concurrentInsertIfNewAdmitsEachValueExactlyOnceAndStillPrunes() {
    let suite = "rk-bridge-\(UUID().uuidString)"
    let defaults = UserDefaults(suiteName: suite)!
    let accepted = NSLock()
    nonisolated(unsafe) var wins: [String] = []
    let group = DispatchGroup()
    for _ in 0..<16 {
        group.enter()
        Thread {
            // Separate log values and defaults handles for one key: the supported multi-instance model.
            let log = BoundedStringLog(defaults: UserDefaults(suiteName: suite)!, key: "dedupe", limit: 100)
            for receipt in 0..<50 where log.insertIfNew("receipt-\(receipt)") {
                accepted.withLock { wins.append("receipt-\(receipt)") }
            }
            group.leave()
        }.start()
    }
    group.wait()
    #expect(wins.count == 50 && Set(wins).count == 50, "each receipt is accepted exactly once")

    let bounded = BoundedStringLog(defaults: defaults, key: "bounded", limit: 3)
    for index in 0..<10 { bounded.remember("v\(index)") }
    #expect(bounded.all == ["v9", "v8", "v7"], "history still prunes to the limit")
}

private func recordingKeys() -> WatchRecordingStorageKeys {
    .init(manifest: "manifest", processed: "processed", terminal: "terminal",
          terminalReasons: "reasons", retryCounts: "retries", lastError: "error")
}

@Test func recordingAssignmentIsOptInAndAcknowledgesOnlyDurableReturnedIdentity() throws {
    let suite = "rk-receive-\(UUID().uuidString)"
    let defaults = UserDefaults(suiteName: suite)!
    let root = FileManager.default.temporaryDirectory.appendingPathComponent(suite)
    defer {
        defaults.removePersistentDomain(forName: suite)
        try? FileManager.default.removeItem(at: root)
    }
    try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    let incoming = root.appendingPathComponent("source")
    try Data([1, 2, 3]).write(to: incoming)
    let strict = WatchRecordingStorage(root: root, defaults: defaults, keys: recordingKeys())
    let strictTransport = FakeTransport(reachable: true)
    let strictInbox = WatchRecordingInbox(storage: strict)
    let invalidMetadata: [[String: Any]] = [[:], ["id": "bad-id"]]
    for metadata in invalidMetadata {
        #expect(throws: WatchRecordingStorageError.self) {
            try strictInbox.receive(fileURL: incoming, metadata: metadata, transport: strictTransport)
        }
    }
    #expect(strictTransport.userInfos.allSatisfy { $0["watchTransferAck"] == nil })
    #expect(try strict.records().isEmpty)
    let lowerID = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
    #expect(try strict.receive(fileURL: incoming, metadata: ["id": lowerID]) == lowerID)

    let storage = WatchRecordingStorage(root: root, defaults: defaults, keys: recordingKeys(),
        receivePolicy: .assignCanonicalIdentifier)
    var delivery = WatchRecordingDelivery()
    delivery.keys.acknowledgement = "hostAck"
    delivery.keys.rollback = "hostRollback"
    let inbox = WatchRecordingInbox(storage: storage, delivery: delivery)
    let compatibleMetadata: [[String: Any]] = [[:], ["id": "malformed"], ["id": lowerID]]
    for metadata in compatibleMetadata {
        let transport = FakeTransport(reachable: true)
        let id = try inbox.receive(fileURL: incoming, metadata: metadata, transport: transport)
        #expect(UUID(uuidString: id)?.uuidString == id)
        let records = try storage.records()
        let record = try #require(records.first { $0.id == id })
        #expect(FileManager.default.fileExists(atPath: storage.audioURL(for: record).path))
        let saved = try storage.metadata(for: record)
        #expect(saved["id"] as? String == metadata["id"] as? String)
        #expect(transport.messages.first?["hostAck"] as? String == id)
        #expect(transport.userInfos.first?["hostAck"] as? String == id)
        let replay = FakeTransport(reachable: false)
        try inbox.resendDurableAcknowledgements(transport: replay)
        #expect(replay.userInfos.contains { $0["hostAck"] as? String == id })
        let receipts = try inbox.receiptReply(to: [delivery.keys.receiptQuery: [id]])
        #expect(receipts[delivery.keys.receipts] as? [String] == [id])
    }
    let canonicalID = UUID(uuidString: lowerID)!.uuidString
    let before = try storage.records().count
    #expect(try storage.receive(fileURL: incoming, metadata: ["id": lowerID]) == canonicalID)
    #expect(try storage.records().count == before)

    let failed = FakeTransport(reachable: true)
    #expect(throws: (any Error).self) {
        try inbox.receive(fileURL: root.appendingPathComponent("missing"),
            metadata: ["id": UUID().uuidString], transport: failed)
    }
    #expect(try storage.records().count == before)
    #expect(failed.userInfos.count == 1)
    #expect(failed.userInfos[0]["hostRollback"] != nil)
    #expect(failed.userInfos[0]["hostAck"] == nil)
    #expect(throws: WatchRecordingStorageError.self) {
        try storage.receive(fileURL: incoming, metadata: ["duration": 601])
    }
    #expect(throws: WatchRecordingStorageError.self) {
        try storage.receive(fileURL: incoming, metadata: ["unsupported": Date()])
    }
}

@Test func recordingManifestSoftCapPreservesPendingAndEvictsOnlyOldFinishedRows() throws {
    let suite = "rk-manifest-\(UUID().uuidString)"
    let defaults = UserDefaults(suiteName: suite)!
    let root = FileManager.default.temporaryDirectory.appendingPathComponent(suite)
    defer {
        defaults.removePersistentDomain(forName: suite)
        try? FileManager.default.removeItem(at: root)
    }
    try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    let source = root.appendingPathComponent("source")
    try Data([1]).write(to: source)
    let records = (0..<252).map { index in
        let id = UUID().uuidString
        return WatchReceivedRecording(id: id, fileName: "\(id).m4a",
            receivedAt: Date(timeIntervalSince1970: Double(index)),
            metadataFileName: "\(id).json", startedAt: nil)
    }
    defaults.set(try JSONEncoder().encode(records), forKey: "manifest")
    let storage = WatchRecordingStorage(root: root, defaults: defaults, keys: recordingKeys(),
        maximumManifestRecords: 250)
    // More pending rows than cap remain pending even after another durable receipt.
    let added = try storage.receive(fileURL: source, metadata: ["id": UUID().uuidString])
    #expect(try storage.records().count == 253)
    #expect(try storage.status().pendingCount == 253)
    try storage.markProcessed(records[0].id)
    try storage.markTerminal(records[1].id, reason: "no_speech")
    try storage.markProcessed(records[2].id)
    try storage.markProcessed(records[3].id)
    // Existing finished audio & sidecar survive removal of their manifest row.
    let oldAudio = root.appendingPathComponent(records[0].fileName)
    let oldMetadata = root.appendingPathComponent(records[0].metadataFileName)
    try Data([2]).write(to: oldAudio)
    try Data("{}".utf8).write(to: oldMetadata)
    _ = try storage.receive(fileURL: source, metadata: ["id": added])
    let retained = try storage.records()
    #expect(retained.count == 250)
    #expect(retained.contains { $0.id == records[3].id })
    #expect(!retained.contains { $0.id == records[0].id || $0.id == records[1].id || $0.id == records[2].id })
    #expect(try storage.status().pendingCount == 249)
    #expect(FileManager.default.fileExists(atPath: oldAudio.path))
    #expect(FileManager.default.fileExists(atPath: oldMetadata.path))
    #expect(storage.isFinished(records[0].id) && storage.isFinished(records[1].id))
    try storage.recoverOrphans()
    #expect(try storage.records().count == 250)
    #expect(try storage.status().pendingCount == 249)
    // Default policy keeps original unbounded manifest behavior.
    defaults.set(try JSONEncoder().encode(records), forKey: "manifest")
    let unbounded = WatchRecordingStorage(root: root, defaults: defaults, keys: recordingKeys())
    _ = try unbounded.receive(fileURL: source, metadata: ["id": records[0].id])
    #expect(try unbounded.records().count == 252)
}


private final class ReceiptObservations: @unchecked Sendable {
    private let lock = NSLock()
    private var values: [WatchRecordingReceiptEvent] = []
    func append(_ event: WatchRecordingReceiptEvent) { lock.withLock { values.append(event) } }
    var events: [WatchRecordingReceiptEvent] { lock.withLock { values } }
}

private struct ImmediateReceiptError: LocalizedError {
    var errorDescription: String? { "immediate unavailable" }
}

private final class ReceiptTransport: WatchMessageTransport {
    let isReachable: Bool
    var userInfos: [[String: Any]] = []
    var beforeSend: () -> Void = {}
    init(reachable: Bool) { isReachable = reachable }
    func sendMessage(_ message: [String: Any], onError: @escaping (Error) -> Void) {
        beforeSend()
        onError(ImmediateReceiptError())
    }
    func transferUserInfo(_ userInfo: [String: Any]) { beforeSend(); userInfos.append(userInfo) }
}

@Test func receiptObservationsFollowDurableIngestAndPreserveRollbackPolicy() throws {
    let suite = "rk-receipt-events-\(UUID().uuidString)"
    let defaults = UserDefaults(suiteName: suite)!
    let root = FileManager.default.temporaryDirectory.appendingPathComponent(suite)
    defer {
        defaults.removePersistentDomain(forName: suite)
        try? FileManager.default.removeItem(at: root)
    }
    try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    let source = root.appendingPathComponent("source")
    try Data([1]).write(to: source)
    let storage = WatchRecordingStorage(root: root, defaults: defaults, keys: recordingKeys())
    let observations = ReceiptObservations()
    let inbox = WatchRecordingInbox(storage: storage, rollbackReason: { _ in "host wording" },
        missingIdentifierRollbackID: "unknown", observe: { observations.append($0) })
    let id = UUID().uuidString
    let transport = ReceiptTransport(reachable: true)
    transport.beforeSend = {
        #expect((try? storage.receivedRecordingIDs()) == [id])
        #expect(FileManager.default.fileExists(atPath: root.appendingPathComponent("\(id).json").path))
    }
    #expect(try inbox.receive(fileURL: source, metadata: ["id": id], transport: transport) == id)
    #expect(observations.events == [
        .immediateFailed(kind: .acknowledgement, recordingID: id, error: "immediate unavailable"),
        .queued(kind: .acknowledgement, recordingID: id, immediateAttempted: true),
    ])
    let rollback = ReceiptTransport(reachable: true)
    #expect(throws: WatchRecordingStorageError.self) {
        try inbox.receive(fileURL: source, metadata: [:], transport: rollback)
    }
    #expect(rollback.userInfos.first?["watchTransferRollback"] as? String == "unknown")
    #expect(rollback.userInfos.first?["watchTransferRollbackReason"] as? String == "host wording")
    #expect(Array(observations.events.suffix(2)) == [
        .immediateFailed(kind: .rollback, recordingID: "unknown", error: "immediate unavailable"),
        .queued(kind: .rollback, recordingID: "unknown", immediateAttempted: true),
    ])
    let strict = ReceiptTransport(reachable: false)
    #expect(throws: WatchRecordingStorageError.self) {
        try WatchRecordingInbox(storage: storage).receive(fileURL: source, metadata: [:], transport: strict)
    }
    #expect(strict.userInfos.isEmpty)
}

@Test func activationReplayUsesNewestDurableRowsWithoutLimitingReceiptQueries() throws {
    let suite = "rk-receipt-replay-\(UUID().uuidString)"
    let defaults = UserDefaults(suiteName: suite)!
    let root = FileManager.default.temporaryDirectory.appendingPathComponent(suite)
    defer {
        defaults.removePersistentDomain(forName: suite)
        try? FileManager.default.removeItem(at: root)
    }
    try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    // Intentionally not manifest order. Newest row lacks audio and must never be acknowledged.
    let records = [1, 3, 0, 2].map { index in
        let id = UUID().uuidString
        return WatchReceivedRecording(id: id, fileName: "\(id).m4a",
            receivedAt: Date(timeIntervalSince1970: Double(index)), metadataFileName: "\(id).json", startedAt: nil)
    }
    defaults.set(try JSONEncoder().encode(records), forKey: "manifest")
    for record in records where record != records[1] {
        try Data([1]).write(to: root.appendingPathComponent(record.fileName))
    }
    let storage = WatchRecordingStorage(root: root, defaults: defaults, keys: recordingKeys())
    let observations = ReceiptObservations()
    let inbox = WatchRecordingInbox(storage: storage, activationReplayLimit: 1,
                                     observe: { observations.append($0) })
    let transport = ReceiptTransport(reachable: false)
    try inbox.resendDurableAcknowledgements(transport: transport)
    #expect(transport.userInfos.compactMap { $0["watchTransferAck"] as? String } == [records[3].id])
    #expect(observations.events.isEmpty)
    let requested = [records[2].id, records[1].id, records[0].id, records[3].id]
    let reply = try inbox.receiptReply(to: ["watchTransferReceiptQuery": requested])
    #expect(reply["watchTransferReceipts"] as? [String] == [records[2].id, records[0].id, records[3].id])
    let unlimited = ReceiptTransport(reachable: false)
    try WatchRecordingInbox(storage: storage).resendDurableAcknowledgements(transport: unlimited)
    #expect(unlimited.userInfos.compactMap { $0["watchTransferAck"] as? String }
        == [records[0].id, records[2].id, records[3].id])
}

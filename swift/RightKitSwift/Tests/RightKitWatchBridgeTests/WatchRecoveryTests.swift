import Foundation
import Testing

@testable import RightKitWatchBridge

private struct RecoveryFixture {
    let suite = "rk-watch-recovery-\(UUID().uuidString)"
    let defaults: UserDefaults
    let root: URL
    let source: URL
    let storage: WatchRecordingStorage

    init() throws {
        defaults = UserDefaults(suiteName: suite)!
        root = FileManager.default.temporaryDirectory.appendingPathComponent(suite)
        source = root.appendingPathComponent("source")
        storage = WatchRecordingStorage(root: root, defaults: defaults, keys: Self.keys)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        try Data([1, 2, 3]).write(to: source)
    }
    static var keys: WatchRecordingStorageKeys {
        .init(manifest: "manifest", processed: "processed", terminal: "terminal",
              terminalReasons: "reasons", retryCounts: "retries", lastError: "error")
    }
    func receive(_ id: String) throws {
        _ = try storage.receive(fileURL: source, metadata: ["id": id])
    }
    func clean() {
        defaults.removePersistentDomain(forName: suite)
        try? FileManager.default.removeItem(at: root)
    }
}

@MainActor
private final class RecoveryPause {
    private(set) var entered = false
    private var continuation: CheckedContinuation<Void, Never>?
    func suspend() async {
        await withCheckedContinuation { continuation in
            self.continuation = continuation
            entered = true
        }
    }
    func resume() {
        continuation?.resume()
        continuation = nil
    }
}

private enum RecoveryFailure: Error { case model, save }

@Test @MainActor func busyWatchDrainProcessesNewArrivalsWithoutRepeatingDeferredIDs() async throws {
    let fixture = try RecoveryFixture()
    defer { fixture.clean() }
    let blockedID = UUID().uuidString
    let incomingID = UUID().uuidString
    try fixture.receive(blockedID)
    let pause = RecoveryPause()
    var attempts: [String] = []
    var committed: [String] = []
    let processor = WatchRecordingProcessor<String>(storage: fixture.storage,
        classify: { _ in .modelRequired },
        prepare: { record, _, _ in
            attempts.append(record.id)
            if record.id == blockedID {
                if !pause.entered { await pause.suspend() }
                throw RecoveryFailure.model
            }
            return record.id
        }, commit: { record, _ in committed.append(record.id) })
    let active = Task { try await processor.processPending() }
    while !pause.entered { await Task.yield() }
    try fixture.receive(incomingID)
    // Many callbacks coalesce to one follow-on snapshot, never simultaneous preparation.
    for _ in 0..<8 {
        let busy = try await processor.processPending()
        #expect(busy.alreadyRunning && !busy.succeeded)
    }
    pause.resume()
    let result = try await active.value
    #expect(result.pendingCount == 1 && !result.alreadyRunning)
    #expect(attempts == [blockedID, incomingID])
    #expect(committed == [incomingID])
    #expect(fixture.storage.retryCount(blockedID) == 0)
    // Deferred work becomes eligible on a later external invocation.
    _ = try await processor.processPending()
    #expect(attempts == [blockedID, incomingID, blockedID])
}

@Test @MainActor func watchCommitFailureDoesNotRetryDuringFollowOnOrBlockAnotherRecording() async throws {
    let fixture = try RecoveryFixture()
    defer { fixture.clean() }
    let failedID = UUID().uuidString
    let incomingID = UUID().uuidString
    try fixture.receive(failedID)
    let pause = RecoveryPause()
    var prepared: [String] = []
    var committed: [String] = []
    let processor = WatchRecordingProcessor<String>(storage: fixture.storage,
        classify: { _ in .transient }, prepare: { record, _, _ in
            prepared.append(record.id)
            if record.id == failedID { await pause.suspend() }
            return record.id
        }, commit: { record, _ in
            if record.id == failedID { throw RecoveryFailure.save }
            committed.append(record.id)
        })
    let active = Task { try await processor.processPending() }
    while !pause.entered { await Task.yield() }
    try fixture.receive(incomingID)
    _ = try await processor.processPending()
    pause.resume()
    let result = try await active.value
    #expect(result.pendingCount == 1)
    #expect(prepared == [failedID, incomingID] && committed == [incomingID])
    #expect(fixture.storage.retryCount(failedID) == 0)
    #expect(!fixture.storage.isFinished(failedID))
}

@Test @MainActor func cancellationLeavesCoalescedWatchArrivalsForNextDrain() async throws {
    let fixture = try RecoveryFixture()
    defer { fixture.clean() }
    let firstID = UUID().uuidString
    let incomingID = UUID().uuidString
    try fixture.receive(firstID)
    let pause = RecoveryPause()
    var committed: [String] = []
    let processor = WatchRecordingProcessor<String>(storage: fixture.storage,
        classify: { _ in .transient }, prepare: { record, _, _ in
            if !pause.entered { await pause.suspend() }
            return record.id
        }, commit: { record, _ in committed.append(record.id) })
    let active = Task { try await processor.processPending() }
    while !pause.entered { await Task.yield() }
    try fixture.receive(incomingID)
    _ = try await processor.processPending()
    active.cancel()
    pause.resume()
    let cancelled = try await active.value
    #expect(cancelled.wasCancelled && cancelled.pendingCount == 2 && committed.isEmpty)
    let resumed = try await processor.processPending()
    #expect(resumed.succeeded && Set(committed) == [firstID, incomingID])
    #expect(committed.count == 2)
}

@Test func corruptWatchManifestRecoversOnlyCompletePairsWithoutResettingLedgers() throws {
    let fixture = try RecoveryFixture()
    defer { fixture.clean() }
    let pendingID = UUID().uuidString
    let processedID = UUID().uuidString
    let terminalID = UUID().uuidString
    for id in [pendingID, processedID, terminalID] { try fixture.receive(id) }
    try fixture.storage.markProcessed(processedID)
    try fixture.storage.markTerminal(terminalID, reason: "decode_exhausted")
    fixture.storage.setRetryCount(1, for: pendingID)
    fixture.storage.lastError = "retained error"
    let missingSidecarID = UUID().uuidString
    let badSidecarID = UUID().uuidString
    let mismatchedID = UUID().uuidString
    let emptyAudioID = UUID().uuidString
    for id in [missingSidecarID, badSidecarID, mismatchedID, emptyAudioID] {
        try Data([4]).write(to: fixture.root.appendingPathComponent("\(id).m4a"))
    }
    try Data("[]".utf8).write(to: fixture.root.appendingPathComponent("\(badSidecarID).json"))
    try JSONSerialization.data(withJSONObject: ["id": UUID().uuidString])
        .write(to: fixture.root.appendingPathComponent("\(mismatchedID).json"))
    try Data().write(to: fixture.root.appendingPathComponent("\(emptyAudioID).m4a"))
    try Data("{}".utf8).write(to: fixture.root.appendingPathComponent("\(emptyAudioID).json"))
    let corrupt = Data("{truncated".utf8)
    fixture.defaults.set(corrupt, forKey: "manifest")
    try fixture.storage.recoverOrphans()
    #expect(Set(try fixture.storage.records().map(\.id)) == [pendingID, processedID, terminalID])
    #expect(fixture.storage.isFinished(processedID) && fixture.storage.isFinished(terminalID))
    #expect(fixture.storage.terminalReasons[terminalID] == "decode_exhausted")
    #expect(fixture.storage.retryCount(pendingID) == 1 && fixture.storage.lastError == "retained error")
    let backups = try FileManager.default.contentsOfDirectory(at: fixture.root, includingPropertiesForKeys: nil)
        .filter { $0.lastPathComponent.hasPrefix(".manifest-corrupt-") }
    #expect(backups.count == 1)
    #expect(try Data(contentsOf: #require(backups.first)) == corrupt)
    // A fresh receipt also repairs corruption without requiring a startup recovery callback.
    fixture.defaults.set(corrupt, forKey: "manifest")
    let newID = UUID().uuidString
    try fixture.receive(newID)
    #expect(Set(try fixture.storage.receivedRecordingIDs()) == [pendingID, processedID, terminalID, newID])
    try fixture.storage.recoverOrphans()
    #expect(try fixture.storage.records().count == 4)
}

@Test func validLegacyWatchManifestKeepsReceiptWithoutMetadataSidecar() throws {
    let fixture = try RecoveryFixture()
    defer { fixture.clean() }
    let id = UUID().uuidString
    try fixture.receive(id)
    try FileManager.default.removeItem(at: fixture.root.appendingPathComponent("\(id).json"))
    try fixture.storage.recoverOrphans()
    #expect(try fixture.storage.receivedRecordingIDs() == [id])
    let record = try #require(fixture.storage.records().first)
    #expect(try fixture.storage.metadataData(for: record) == Data("{}".utf8))
}

private final class RecoveryDefaults: UserDefaults, @unchecked Sendable {
    var permitsPersistence = true
    override func synchronize() -> Bool { permitsPersistence && super.synchronize() }
}

private final class RecoveryTransport: WatchMessageTransport {
    var isReachable: Bool { true }
    var messages: [[String: Any]] = []
    func sendMessage(_ message: [String: Any], onError: @escaping (Error) -> Void) { messages.append(message) }
    func transferUserInfo(_ userInfo: [String: Any]) { messages.append(userInfo) }
}

@Test func failedWatchManifestPersistenceNeverExposesNewReceiptOrDiscardsCorruptBytes() throws {
    let fixture = try RecoveryFixture()
    defer { fixture.clean() }
    let defaults = RecoveryDefaults(suiteName: fixture.suite)!
    let storage = WatchRecordingStorage(root: fixture.root, defaults: defaults, keys: RecoveryFixture.keys)
    let oldID = UUID().uuidString
    _ = try storage.receive(fileURL: fixture.source, metadata: ["id": oldID])
    defaults.permitsPersistence = false
    let transport = RecoveryTransport()
    #expect(throws: (any Error).self) {
        try WatchRecordingInbox(storage: storage).receive(fileURL: fixture.source,
            metadata: ["id": UUID().uuidString], transport: transport)
    }
    #expect(transport.messages.allSatisfy { $0["watchTransferAck"] == nil })
    #expect(try storage.receivedRecordingIDs() == [oldID])
    let corrupt = Data("not-json".utf8)
    defaults.set(corrupt, forKey: "manifest")
    #expect(throws: WatchRecordingStorageError.self) { try storage.recoverOrphans() }
    #expect(defaults.data(forKey: "manifest") == corrupt)
    let backups = try FileManager.default.contentsOfDirectory(at: fixture.root, includingPropertiesForKeys: nil)
        .filter { $0.lastPathComponent.hasPrefix(".manifest-corrupt-") }
    #expect(backups.count == 1)
    #expect(try Data(contentsOf: #require(backups.first)) == corrupt)
}

@Test func corruptWatchManifestPreservesAssignedIdentityWithLegacySenderMetadata() throws {
    let fixture = try RecoveryFixture()
    defer { fixture.clean() }
    let compatible = WatchRecordingStorage(root: fixture.root, defaults: fixture.defaults,
        keys: RecoveryFixture.keys, receivePolicy: .assignCanonicalIdentifier)
    var ids: [String] = []
    let metadataValues: [[String: Any]] = [[:], ["id": "legacy-invalid"]]
    for metadata in metadataValues {
        ids.append(try compatible.receive(fileURL: fixture.source, metadata: metadata))
    }
    fixture.defaults.set(Data("broken".utf8), forKey: "manifest")
    #expect(Set(try compatible.receivedRecordingIDs()) == Set(ids))
    let recovered = try compatible.records()
    let assigned = try #require(recovered.first { $0.id == ids[1] })
    #expect(try compatible.metadata(for: assigned)["id"] as? String == "legacy-invalid")
}

@Test func failedWatchLedgerPersistenceDoesNotMarkWorkFinished() throws {
    let fixture = try RecoveryFixture()
    defer { fixture.clean() }
    let defaults = RecoveryDefaults(suiteName: fixture.suite)!
    let storage = WatchRecordingStorage(root: fixture.root, defaults: defaults, keys: RecoveryFixture.keys)
    let processedID = UUID().uuidString
    let terminalID = UUID().uuidString
    _ = try storage.receive(fileURL: fixture.source, metadata: ["id": processedID])
    _ = try storage.receive(fileURL: fixture.source, metadata: ["id": terminalID])

    defaults.permitsPersistence = false
    #expect(throws: WatchRecordingStorageError.self) { try storage.markProcessed(processedID) }
    #expect(throws: WatchRecordingStorageError.self) { try storage.markTerminal(terminalID, reason: "missing_audio") }
    #expect(!storage.isFinished(processedID) && !storage.isFinished(terminalID))
    #expect(storage.terminalReasons[terminalID] == nil)
    #expect(try storage.status().pendingCount == 2)

    defaults.permitsPersistence = true
    try storage.markProcessed(processedID)
    try storage.markTerminal(terminalID, reason: "missing_audio")
    #expect(storage.isFinished(processedID) && storage.isFinished(terminalID))
    #expect(storage.terminalReasons[terminalID] == "missing_audio")
    #expect(try storage.status().pendingCount == 0)
}

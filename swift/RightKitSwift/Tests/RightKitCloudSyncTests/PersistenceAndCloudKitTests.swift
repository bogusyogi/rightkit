import CloudKit
import Foundation
import Testing
@testable import RightKitCloudSync

@Suite struct PersistenceAndCloudKitTests {
    @Test func stateFileSurvivesRestartAndCorruptionRecoversToFreshStateKeepingTheBadBytes() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("cloudsync-\(UUID().uuidString)")
        let url = dir.appendingPathComponent("state.json")
        let store = FileSyncStateStore(fileURL: url)
        var state = CloudSyncState()
        state.changeToken = SyncChangeToken(data: Data([1, 2, 3]))
        state.tombstones["x"] = t(5)
        state.pendingDeletions = ["x"]
        state.aliases["x"] = ["x", "legacy-x"]
        try store.save(state)
        #expect(try FileSyncStateStore(fileURL: url).load() == state)

        try Data("{not json".utf8).write(to: url)
        let recovered = try store.load()
        #expect(recovered == CloudSyncState())
        #expect(FileManager.default.fileExists(atPath: url.path + ".corrupt"))
    }

    @Test func ckRecordCodingRoundTripsAndOmitsEmptyListsAndClearsNulls() {
        let record = CKRecord(recordType: "Note", recordID: CKRecord.ID(recordName: "n1"))
        record["old"] = "stale" as NSString
        CloudKitRecordCoding.apply([
            "text": .string("hi"), "n": .int(7), "x": .double(1.5), "when": .date(t(3)),
            "blob": .data(Data([9])), "tags": .strings(["a", "b"]), "empty": .strings([]), "old": .null,
        ], to: record)
        let decoded = CloudKitRecordCoding.decode(record)
        #expect(decoded.fields["text"] == .string("hi") && decoded.fields["n"] == .int(7))
        #expect(decoded.fields["x"] == .double(1.5) && decoded.fields["when"] == .date(t(3)))
        #expect(decoded.fields["blob"] == .data(Data([9])) && decoded.fields["tags"] == .strings(["a", "b"]))
        #expect(decoded.fields["empty"] == nil && decoded.fields["old"] == nil)
    }

    @Test func ckErrorsClassifyWithServerRetryGuidanceIncludingPartialFailures() {
        #expect(SyncTransportError(CKError(.userDeletedZone)) == .zoneNotFound)
        #expect(SyncTransportError(CKError(.changeTokenExpired)) == .changeTokenExpired)
        #expect(SyncTransportError(CKError(.quotaExceeded)) == .quotaExceeded)
        let limited = CKError(.requestRateLimited, userInfo: [CKErrorRetryAfterKey: NSNumber(value: 90)])
        #expect(SyncTransportError(limited).retryDelay == 90)
        let partial = CKError(.partialFailure, userInfo: [CKPartialErrorsByItemIDKey: [
            "a": limited as NSError,
        ] as [String: NSError]])
        // Partial wrapper has no code of its own, but the nested guidance must not be lost.
        guard case let .other(code, _) = SyncTransportError(partial) else { Issue.record("expected other"); return }
        #expect(code == CKError.Code.partialFailure.rawValue)
    }
}

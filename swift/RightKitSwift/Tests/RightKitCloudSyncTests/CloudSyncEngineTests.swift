import Foundation
import Testing
@testable import RightKitCloudSync

@Suite struct CloudSyncEngineTests {
    @Test func twoDevicesConvergeOnEditsAndDeletesAndTombstonesWin() async throws {
        let zone = InMemorySyncTransport()
        let a = Device(transport: zone), b = Device(transport: zone)
        a.adapter.put(Note(id: "n1", text: "one", updatedAt: t(1), createdAt: t(1)))
        a.adapter.put(Note(id: "n2", text: "two", updatedAt: t(2), createdAt: t(2)))

        let first = await a.engine.sync()
        #expect(first.status == .ready && first.uploaded == 2)
        let down = await b.engine.sync()
        #expect(down.downloaded == 2 && b.adapter.notes == a.adapter.notes)

        // B edits newer: A converges, nothing is re-uploaded by A.
        b.adapter.put(Note(id: "n1", text: "one-b", updatedAt: t(10), createdAt: t(1)))
        _ = await b.engine.sync()
        let aPull = await a.engine.sync()
        #expect(aPull.downloaded == 1 && aPull.uploaded == 0)
        #expect(a.adapter.notes["n1"]?.text == "one-b")

        // A deletes n2; B must lose it. B holds a NEWER stale edit, but the tombstone still wins.
        b.adapter.put(Note(id: "n2", text: "two-edited-late", updatedAt: t(99), createdAt: t(2)))
        a.adapter.remove("n2")
        try await a.engine.markDeleted("n2")
        _ = await a.engine.sync()
        let bAfter = await b.engine.sync()
        #expect(bAfter.deleted == 1 && b.adapter.notes["n2"] == nil)
        let stored = await zone.record("n2")
        #expect(stored?.fields["deletedAt"]?.dateValue != nil && stored?.fields["text"] == nil)
        // A brand-new local copy of a buried id is never published back.
        b.adapter.put(Note(id: "n2", text: "zombie", updatedAt: t(200), createdAt: t(2)))
        _ = await b.engine.sync()
        #expect(b.adapter.notes["n2"] == nil)
        let still = await zone.record("n2")
        #expect(still?.fields["text"] == nil)
    }

    @Test func failedLocalWriteKeepsTokenSoThePageReplays() async throws {
        let zone = InMemorySyncTransport()
        let a = Device(transport: zone), b = Device(transport: zone)
        a.adapter.put(Note(id: "n1", text: "one", updatedAt: t(1), createdAt: t(1)))
        _ = await a.engine.sync()

        b.adapter.failApply(true)
        let failed = await b.engine.sync()
        guard case .unavailable = failed.status else { Issue.record("expected unavailable"); return }
        let tokenAfterFail = await b.engine.snapshot.changeToken
        let gate = await b.engine.retryDelayRemaining()
        #expect(tokenAfterFail == nil && gate != nil)

        b.adapter.failApply(false)
        // Retry gate is a wall-clock hint; engine clock is frozen at t(0) so it is still closed.
        // A fresh engine on the same state (relaunch) proves replay with the saved state.
        let relaunched = CloudSyncEngine(
            adapter: b.adapter, transport: zone, stateStore: b.store, enabledStore: b.enabled
        )
        let ok = await relaunched.sync()
        #expect(ok.status == .ready && ok.downloaded == 1 && b.adapter.notes["n1"]?.text == "one")
        let tokenAfterOk = await relaunched.snapshot.changeToken
        #expect(tokenAfterOk != nil)
    }

    @Test func partialUploadFailureStillSavesTheRestAndReportsPending() async throws {
        let zone = InMemorySyncTransport()
        let a = Device(transport: zone)
        for i in 1...3 { a.adapter.put(Note(id: "n\(i)", text: "t\(i)", updatedAt: t(Double(i)), createdAt: t(Double(i)))) }
        await zone.setFailingSaves(["n2": .busy(retryAfter: 60)])
        let r = await a.engine.sync()
        #expect(r.uploaded == 2 && r.status == .unavailable(SyncMessages().pendingChanges))
        let n2 = await zone.record("n2"), n1 = await zone.record("n1")
        #expect(n2 == nil && n1 != nil)
        let wait = await a.engine.retryDelayRemaining() ?? 0
        #expect(wait >= 60)
        // The failed item is retried (not forgotten) on a later pass.
        let relaunched = CloudSyncEngine(adapter: a.adapter, transport: zone, stateStore: a.store, enabledStore: a.enabled)
        let again = await relaunched.sync()
        #expect(again.uploaded == 1 && again.status == .ready)
        let n2Again = await zone.record("n2")
        #expect(n2Again != nil)
    }

    @Test func expiredTokenAndDeletedZoneReplayWithoutLosingData() async throws {
        let zone = InMemorySyncTransport()
        await zone.setPageSize(2)
        let a = Device(transport: zone)
        for i in 1...5 { a.adapter.put(Note(id: "n\(i)", text: "t\(i)", updatedAt: t(Double(i)), createdAt: t(Double(i)))) }
        _ = await a.engine.sync()
        let b = Device(transport: zone)
        let pulled = await b.engine.sync()
        #expect(pulled.downloaded == 5) // paginated across 3 pages

        await zone.expireTokens()
        let afterExpiry = await b.engine.sync()
        #expect(afterExpiry.status == .ready && b.adapter.notes.count == 5)

        // User wiped iCloud data for the app: zone is gone, local data must survive and re-publish.
        await zone.deleteZone()
        let recovered = await b.engine.sync()
        #expect(recovered.status == .ready && recovered.uploaded == 5)
        let count = await zone.allRecords().count
        #expect(count == 5)
    }

    @Test func accountAndEnabledStatesGateAllNetworkWork() async throws {
        let zone = InMemorySyncTransport()
        let a = Device(transport: zone)
        a.adapter.put(Note(id: "n1", text: "one", updatedAt: t(1), createdAt: t(1)))

        await zone.setAccount(.noAccount)
        let none = await a.engine.sync()
        #expect(none.status == .unavailable(SyncMessages().noAccount))
        let f0 = await zone.fetchCount, s0 = await zone.saveCallCount
        #expect(f0 == 0 && s0 == 0)

        await zone.setAccount(.available)
        await a.engine.setEnabled(false)
        let off = await a.engine.sync()
        let f1 = await zone.fetchCount
        #expect(off.status == .disabled && f1 == 0)

        // Turning sync off while a fetch is in flight fences the pass: nothing is written either way.
        await a.engine.setEnabled(true)
        let engine = a.engine
        await zone.setOnFetch { await engine.setEnabled(false) }
        let fenced = await a.engine.sync()
        #expect(fenced.status == .disabled)
        let s1 = await zone.saveCallCount
        #expect(s1 == 0 && a.adapter.applyCalls == 0)
    }

    @Test func malformedRemoteRecordIsQuarantinedNotFatal() async throws {
        let zone = InMemorySyncTransport()
        await zone.seed(SyncRecord(recordName: "bad", recordType: "Note", fields: ["text": .string("no id")]))
        await zone.seed(SyncRecord(recordName: "ok", recordType: "Note", fields: [
            "id": .string("ok"), "text": .string("fine"), "updatedAt": .date(t(1)), "createdAt": .date(t(1)),
        ]))
        let b = Device(transport: zone)
        let r = await b.engine.sync()
        #expect(r.status == .ready && b.adapter.notes.keys.sorted() == ["ok"])
    }

    @Test func hardDeletedRecordResolvesThroughAliasesAndTombstonesLocalCopy() async throws {
        let zone = InMemorySyncTransport()
        let a = Device(transport: zone), b = Device(transport: zone)
        a.adapter.put(Note(id: "n1", text: "one", updatedAt: t(1), createdAt: t(1)))
        _ = await a.engine.sync()
        _ = await b.engine.sync()
        await zone.hardDelete("n1") // dashboard / other client removed the record outright
        let r = await b.engine.sync()
        #expect(r.deleted == 1 && b.adapter.notes.isEmpty)
        _ = await a.engine.sync()
        #expect(a.adapter.notes.isEmpty)
    }

    @Test func subscriptionFailureNeverBlocksDataAndRetriesBounded() async throws {
        let zone = InMemorySyncTransport()
        await zone.setFailNextSubscription(.networkUnavailable(retryAfter: nil))
        var config = CloudSyncConfiguration()
        config.sleep = { _ in }
        let a = Device(transport: zone, configuration: config)
        a.adapter.put(Note(id: "n1", text: "one", updatedAt: t(1), createdAt: t(1)))
        let ready = AsyncStream<Void>.makeStream()
        await a.engine.onSubscriptionReady { ready.continuation.yield() }
        let first = await a.engine.sync()
        #expect(first.uploaded == 1)
        var iterator = ready.stream.makeAsyncIterator()
        _ = await iterator.next()
        let subs = await zone.subscriptionCount
        #expect(subs == 1)
    }
}

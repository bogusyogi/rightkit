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

    @Test func transientPerRecordFetchErrorAbortsThePullKeepsTheTokenAndNeverQuarantines() async throws {
        let zone = InMemorySyncTransport()
        let a = Device(transport: zone), b = Device(transport: zone)
        a.adapter.put(Note(id: "n1", text: "one", updatedAt: t(1), createdAt: t(1)))
        a.adapter.put(Note(id: "n2", text: "two", updatedAt: t(2), createdAt: t(2)))
        _ = await a.engine.sync()
        // b has its own unsynced note: a partial pull must not push it.
        b.adapter.put(Note(id: "local", text: "l", updatedAt: t(3), createdAt: t(3)))
        let saves = await zone.saveCallCount

        // The page succeeds and its token advances, but n2 fails to fetch (rate limited) many times.
        await zone.setFailingFetches(["n2": 100])
        await zone.setFailingFetchRetryAfter(120)
        for _ in 0..<8 {
            let r = await b.engine.sync()
            guard case .unavailable = r.status else { Issue.record("expected unavailable"); return }
        }
        let held = await b.engine.snapshot
        #expect(held.changeToken == nil, "token must not advance past an unfetched record")
        #expect(held.quarantined.isEmpty, "a transient error is never quarantined, however often it repeats")
        #expect(b.adapter.notes.keys.sorted() == ["local"], "nothing is applied from a partial snapshot")
        #expect(await zone.saveCallCount == saves, "no push before a successful pull")
        let wait = await b.engine.retryDelayRemaining()
        #expect((wait ?? 0) >= 100, "server retryAfter is honoured")

        await zone.setFailingFetches([:])
        let ok = await CloudSyncEngine(adapter: b.adapter, transport: zone, stateStore: b.store, enabledStore: b.enabled).sync()
        #expect(ok.status == .ready)
        #expect(b.adapter.notes["n1"]?.text == "one" && b.adapter.notes["n2"]?.text == "two")
        #expect(await zone.record("local") != nil)
    }

    @Test func permanentlyUndecodableRecordIsQuarantinedAtOnceAndRetriedByScheduledReplay() async throws {
        let zone = InMemorySyncTransport()
        let a = Device(transport: zone)
        a.adapter.put(Note(id: "bad", text: "x", updatedAt: t(1), createdAt: t(1)))
        a.adapter.put(Note(id: "good", text: "y", updatedAt: t(2), createdAt: t(2)))
        _ = await a.engine.sync()

        var config = CloudSyncConfiguration()
        let now = ManualClock()
        config.clock = { now.value }
        let adapter = NoteAdapter(), store = InMemorySyncStateStore(), enabled = InMemorySyncEnabledStore(enabled: true)
        let engine = CloudSyncEngine(adapter: adapter, transport: zone, stateStore: store, enabledStore: enabled, configuration: config)
        await zone.setUndecodableRecords(["bad"])
        let first = await engine.sync()
        #expect(first.status == .ready, "a permanent decode error never blocks the pass")
        let quarantined = await engine.snapshot
        #expect(quarantined.changeToken != nil && quarantined.quarantined["bad"] != nil)
        #expect(adapter.notes.keys.sorted() == ["good"])
        #expect(await zone.record("bad")?.fields["text"] == .string("x"), "unreadable server record is never overwritten")

        await zone.setUndecodableRecords([])
        now.value = now.value.addingTimeInterval(config.quarantineReplayInterval + 1)
        let replay = await engine.sync()
        #expect(replay.status == .ready && adapter.notes["bad"]?.text == "x")
        #expect(await engine.snapshot.quarantined.isEmpty)
    }

    @Test func legacyUnscopedLedgerKeepsTombstonesButNeverImportsItsCursorOrDeletionQueue() async throws {
        let zone = InMemorySyncTransport()
        let d = Device(transport: zone)
        var legacy = CloudSyncState()
        legacy.tombstones["old"] = t(1)
        legacy.pendingDeletions = ["old"]
        legacy.changeToken = SyncChangeToken(data: Data([1]))
        legacy.remoteVersions["x"] = .init(recordName: "x", version: t(1))
        try d.store.save(legacy)
        let engine = CloudSyncEngine(adapter: d.adapter, transport: zone, stateStore: d.store, enabledStore: d.enabled)
        d.adapter.put(Note(id: "old", text: "o", updatedAt: t(1), createdAt: t(1)))
        let r = await engine.sync()
        #expect(r.status == .ready)
        let state = await engine.snapshot
        #expect(state.accountID == "in-memory-account")
        #expect(state.tombstones["old"] != nil && d.adapter.notes["old"] == nil, "legacy deletion still suppresses local history")
        #expect(state.remoteVersions["x"] == nil && state.pendingDeletions.isEmpty)
        #expect(await zone.record("old") == nil, "legacy deletion never enters this account's outbound queue")
    }

    @Test func accountChangeDuringAPassFencesTheCommitAndNextPassRebinds() async throws {
        let zone = InMemorySyncTransport()
        let a = Device(transport: zone)
        a.adapter.put(Note(id: "n1", text: "one", updatedAt: t(1), createdAt: t(1)))
        _ = await a.engine.sync()
        let b = Device(transport: zone)
        _ = await b.engine.sync()
        let boundToken = await b.engine.snapshot.changeToken
        a.adapter.put(Note(id: "n2", text: "two", updatedAt: t(2), createdAt: t(2)))
        _ = await a.engine.sync()
        b.adapter.put(Note(id: "mine", text: "m", updatedAt: t(5), createdAt: t(5)))

        // The user switches iCloud accounts while b's fetch is in flight.
        await zone.setOnFetch { await zone.setAccountID("account-b") }
        let during = await b.engine.sync()
        #expect(during.status == .disabled, "in-flight pass for the old account is cancelled")
        let after = await b.engine.snapshot
        #expect(after.changeToken == boundToken && after.accountID == "in-memory-account", "old ledger untouched by the fenced pass")
        #expect(await zone.record("mine") == nil, "nothing uploads under the new account from the old pass")

        await zone.setOnFetch(nil)
        _ = await b.engine.sync()
        #expect(await b.engine.snapshot.accountID == "account-b")
        #expect(b.adapter.switches == ["in-memory-account->account-b"])
    }

    @Test func accountSwitchDropsTheOldLedgerAndReplaysTheNewAccountZone() async throws {
        let zone = InMemorySyncTransport()
        let d = Device(transport: zone)
        d.adapter.put(Note(id: "mine", text: "m", updatedAt: t(1), createdAt: t(1)))
        _ = await d.engine.sync()
        let bound = await d.engine.snapshot
        #expect(bound.accountID == "in-memory-account" && bound.changeToken != nil && !bound.remoteVersions.isEmpty)
        try await d.engine.markDeleted("gone")

        // Another iCloud account signs in on the same device: a different (empty) zone.
        await zone.deleteZone()
        await zone.setAccountID("account-b")
        d.adapter.purgeOnSwitch = true
        await zone.seed(SyncRecord(recordName: "theirs", recordType: "Note", fields: [
            "id": .string("theirs"), "text": .string("t"), "updatedAt": .date(t(5)), "createdAt": .date(t(5)),
        ]))
        let result = await d.engine.sync()
        #expect(result.status == .ready)
        #expect(d.adapter.switches == ["in-memory-account->account-b"])
        let state = await d.engine.snapshot
        #expect(state.accountID == "account-b")
        #expect(state.tombstones["gone"] == nil && state.remoteVersions["mine"] == nil, "old account ledger is gone")
        #expect(d.adapter.notes.keys.sorted() == ["theirs"])
        #expect(await zone.record("mine") == nil, "previous account's item never uploads into the new account")
    }
}

final class ManualClock: @unchecked Sendable {
    private let lock = NSLock()
    private var current = t(0)
    var value: Date {
        get { lock.withLock { current } }
        set { lock.withLock { current = newValue } }
    }
}

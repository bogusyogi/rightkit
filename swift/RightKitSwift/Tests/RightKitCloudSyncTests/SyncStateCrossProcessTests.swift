import Darwin
import Foundation
import Testing
@testable import RightKitCloudSync

/// S03: the app-group ledger must survive interleaved read-modify-write from several processes.
/// Two independent `FileSyncStateStore` instances over one directory stand in for the app and an
/// extension (each owns its own file descriptor, so `flock` excludes them exactly as it excludes
/// processes); a real separate process (`perl`, as `RightKitStoreTests` does) proves the lock is
/// honoured across a process boundary and released when its holder is killed.
@Suite struct SyncStateCrossProcessTests {
    private func scratch() -> URL {
        FileManager.default.temporaryDirectory.appendingPathComponent("cloudsync-xp-\(UUID().uuidString)")
    }

    @Test func interleavedLoadModifySaveKeepsBothWritersChanges() throws {
        let url = scratch().appendingPathComponent("state.json")
        let app = FileSyncStateStore(fileURL: url), ext = FileSyncStateStore(fileURL: url)
        var seed = CloudSyncState()
        seed.accountID = "acct"
        seed.tombstones["old"] = t(1)
        try app.save(seed)

        var a = try app.load()
        var b = try ext.load()  // both read the same document...
        b.tombstones["from-ext"] = t(2)
        b.remoteVersions["r1"] = .init(recordName: "r1", version: t(2))
        b.changeToken = SyncChangeToken(data: Data([2]))
        try ext.save(b)  // ...the extension writes first...
        a.tombstones["from-app"] = t(3)
        a.pendingDeletions = ["p"]
        try app.save(a)  // ...and the app's whole-document write must not erase it

        let final = try FileSyncStateStore(fileURL: url).load()
        #expect(final.tombstones.keys.sorted() == ["from-app", "from-ext", "old"])
        #expect(final.remoteVersions["r1"] != nil)
        #expect(final.changeToken == SyncChangeToken(data: Data([2])), "app did not change the token, so the extension's stands")
        #expect(final.pendingDeletions == ["p"])
    }

    @Test func commitReturnsTheMergedStateAndLaterSavesDoNotDeleteWhatWasMergedIn() throws {
        let url = scratch().appendingPathComponent("state.json")
        let app = FileSyncStateStore(fileURL: url), ext = FileSyncStateStore(fileURL: url)
        var mine = try app.load()
        var theirs = try ext.load()
        theirs.tombstones["theirs"] = t(1)
        try ext.save(theirs)
        mine.tombstones["mine"] = t(2)
        let merged = try app.commit(mine)
        #expect(Set(merged.tombstones.keys) == ["mine", "theirs"])

        var next = merged
        next.markers.insert("m")
        try app.save(next)
        #expect(Set(try ext.load().tombstones.keys) == ["mine", "theirs"])
    }

    @Test func aCallerThatIgnoresTheMergeUsingSaveStillNeverDeletesTheOtherWritersEntries() throws {
        let url = scratch().appendingPathComponent("state.json")
        let app = FileSyncStateStore(fileURL: url), ext = FileSyncStateStore(fileURL: url)
        var mine = try app.load()
        var theirs = try ext.load()
        theirs.tombstones["theirs"] = t(1)
        try ext.save(theirs)
        mine.tombstones["mine"] = t(2)
        try app.save(mine)  // merged on disk, but `mine` in memory still lacks "theirs"
        mine.markers.insert("later")
        try app.save(mine)
        #expect(Set(try ext.load().tombstones.keys) == ["mine", "theirs"])
        #expect(try ext.load().markers == ["later"])
    }

    @Test func aDeletionByOneWriterPropagatesWhileTheOthersAdditionSurvives() throws {
        let url = scratch().appendingPathComponent("state.json")
        let app = FileSyncStateStore(fileURL: url), ext = FileSyncStateStore(fileURL: url)
        var seed = CloudSyncState()
        seed.pendingDeletions = ["done", "keep"]
        try app.save(seed)
        var a = try app.load(), b = try ext.load()
        a.pendingDeletions.remove("done")  // written to the server by the app
        b.pendingDeletions.insert("new")  // queued by the extension
        try app.save(a)
        try ext.save(b)
        #expect(try app.load().pendingDeletions == ["keep", "new"])
    }

    @Test func ledgersForDifferentAccountsNeverMerge() throws {
        let url = scratch().appendingPathComponent("state.json")
        let app = FileSyncStateStore(fileURL: url), ext = FileSyncStateStore(fileURL: url)
        var old = CloudSyncState()
        old.accountID = "old"
        old.tombstones["x"] = t(1)
        try app.save(old)
        var a = try app.load(), b = try ext.load()
        b.tombstones["y"] = t(2)
        try ext.save(b)
        a = CloudSyncState()  // app switched accounts: fresh ledger
        a.accountID = "new"
        try app.save(a)
        let final = try app.load()
        #expect(final.accountID == "new" && final.tombstones.isEmpty)
    }

    @Test func manyIndependentWritersLoseNothing() async throws {
        let url = scratch().appendingPathComponent("state.json")
        let writers = 6, rounds = 25
        await withTaskGroup(of: Void.self) { group in
            for writer in 0..<writers {
                group.addTask {
                    let store = FileSyncStateStore(fileURL: url)  // one instance per "process"
                    for round in 0..<rounds {
                        do {
                            var state = try store.load()
                            state.tombstones["w\(writer)-\(round)"] = t(Double(round))
                            try store.save(state)
                        } catch { Issue.record("writer \(writer) round \(round): \(error)") }
                    }
                }
            }
        }
        let final = try FileSyncStateStore(fileURL: url).load()
        #expect(final.tombstones.count == writers * rounds)
    }

    @Test func aSeparateProcessHoldingTheLockBlocksWritesAndKillingItReleasesThem() throws {
        let url = scratch().appendingPathComponent("state.json")
        let store = FileSyncStateStore(fileURL: url, lockTimeout: 0.3)
        var seed = CloudSyncState()
        seed.tombstones["a"] = t(1)
        try store.save(seed)

        let holder = Process()
        holder.executableURL = URL(fileURLWithPath: "/usr/bin/perl")
        holder.arguments = ["-e", "use Fcntl qw(:flock); open(F, '>>', $ARGV[0]) or die; flock(F, LOCK_EX) or die; $|=1; print qq(held\\n); sleep 60;", url.path + ".lock"]
        let out = Pipe()
        holder.standardOutput = out
        try holder.run()
        defer { if holder.isRunning { kill(holder.processIdentifier, SIGKILL); holder.waitUntilExit() } }
        #expect(String(decoding: out.fileHandleForReading.availableData, as: UTF8.self).contains("held"))

        var changed = seed
        changed.tombstones["b"] = t(2)
        #expect(throws: CloudSyncError.self) { try store.save(changed) }
        #expect(throws: CloudSyncError.self) { _ = try store.load() }
        #expect(try Data(contentsOf: url) == Data(contentsOf: url), "file readable")
        let raw = try String(contentsOf: url, encoding: .utf8)
        #expect(!raw.contains("\"b\""), "nothing is written without the lock")

        // Crashed holder: SIGKILL, no unlock. The kernel drops the flock, so the ledger is not stuck.
        kill(holder.processIdentifier, SIGKILL)
        holder.waitUntilExit()
        try store.save(changed)
        #expect(Set(try store.load().tombstones.keys) == ["a", "b"])
    }

    // MARK: Merge rules

    @Test func mergeRulesForVersionsTokensAndModifiedVersusDeleted() {
        var base = CloudSyncState()
        base.remoteVersions = ["both": .init(recordName: "both", version: t(1)), "gone": .init(recordName: "gone", version: t(1))]
        base.changeToken = SyncChangeToken(data: Data([0]))
        var mine = base, theirs = base
        mine.remoteVersions["both"] = .init(recordName: "both", version: t(5))
        theirs.remoteVersions["both"] = .init(recordName: "both", version: t(9))
        mine.remoteVersions["gone"] = nil  // mine forgot it...
        theirs.remoteVersions["gone"] = .init(recordName: "gone", version: t(7))  // ...theirs updated it
        mine.changeToken = SyncChangeToken(data: Data([1]))
        let merged = CloudSyncState.merged(base: base, mine: mine, theirs: theirs)
        #expect(merged.remoteVersions["both"]?.version == t(9), "later version wins")
        #expect(merged.remoteVersions["gone"]?.version == t(7), "a modification beats a concurrent deletion")
        #expect(merged.changeToken == SyncChangeToken(data: Data([1])), "the changing side's token")
    }

    @Test func singleWriterStoresKeepSaveSemanticsThroughTheDefaultCommit() throws {
        let memory = InMemorySyncStateStore()
        var state = CloudSyncState()
        state.markers = ["x"]
        #expect(try memory.commit(state) == state)
        #expect(try memory.load() == state)
    }
}

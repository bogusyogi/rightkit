import Foundation
import Testing

@testable import RightKitStore

private struct Command: AppGroupCommand {
    let requestID: UUID
    let issuedAt: Date
    let name: String
    init(_ name: String, at date: Date) { requestID = UUID(); issuedAt = date; self.name = name }
}

private struct Snapshot: RevisionedState {
    var revision: UInt64 = 0
    var updatedAt = Date(timeIntervalSince1970: 0)
    var phase = "idle"
}

private func config(timeout: UInt32 = 250_000) -> (AppGroupConfiguration, URL) {
    let dir = FileManager.default.temporaryDirectory.appendingPathComponent("rk-store-\(UUID().uuidString)")
    try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    let suite = UserDefaults(suiteName: "rk-store-\(UUID().uuidString)")!
    return (AppGroupConfiguration(groupID: "group.test", lockDirectory: dir, defaultsOverride: suite, lockTimeoutMicroseconds: timeout), dir)
}

@Test func commandIsAtLeastOnceUntilAcknowledgedAndExpiresWhenUnclaimed() {
    let (cfg, _) = config()
    let queue = AppGroupCommandQueue<Command>(configuration: cfg)
    let t0 = Date(timeIntervalSince1970: 1_000)
    let start = Command("start", at: t0)
    #expect(queue.submit(start, now: t0) != nil)

    #expect(queue.claimAll(now: t0).map(\.name) == ["start"])
    #expect(queue.claimAll(now: t0.addingTimeInterval(1)).isEmpty, "a claimed command is not handed out twice")
    // Consumer crashed before acknowledging: the claim lease expires and the command is retried.
    #expect(queue.claimAll(now: t0.addingTimeInterval(301)).map(\.name) == ["start"])

    queue.acknowledge([start.requestID], now: t0.addingTimeInterval(302))
    #expect(queue.claimAll(now: t0.addingTimeInterval(400)).isEmpty)
    #expect(queue.lastAcknowledgedRequestID() == start.requestID)

    let stale = Command("stale", at: t0)
    queue.submit(stale, now: t0)
    #expect(queue.pending(now: t0.addingTimeInterval(31)).isEmpty, "an unclaimed command past its TTL is dropped")
}

@Test func lockFailsClosedInsteadOfRunningUnsynchronised() throws {
    let (cfg, dir) = config(timeout: 20_000)
    let queue = AppGroupCommandQueue<Command>(configuration: cfg)
    // Another process holds the lock.
    let holder = open(dir.appendingPathComponent(cfg.lockName).path, O_CREAT | O_RDWR, 0o600)
    #expect(holder >= 0 && flock(holder, LOCK_EX) == 0)
    defer { flock(holder, LOCK_UN); close(holder) }

    var ran = false
    #expect(AppGroupFileLock.withExclusiveLock(cfg) { ran = true } == nil)
    #expect(!ran)
    #expect(queue.submit(Command("x", at: Date())) == nil, "a refused submit must not look queued")
    #expect(queue.claimAll().isEmpty)
}

@Test func ownerLeaseRejectsDeadProcessAndStateRevisionIsMonotonic() {
    let (cfg, _) = config()
    let now = Date()
    // A pid that cannot exist: a fresh timestamp must not keep a force-quit owner alive.
    let dead = AppGroupOwnerLease(owner: AppGroupOwnerLease.ownerToken(name: "app", pid: 2_000_000_000), sessionID: "s", savedAt: now)
    #expect(!dead.isLive(at: now))
    let live = AppGroupOwnerLease(owner: AppGroupOwnerLease.ownerToken(name: "app"), sessionID: "s", savedAt: now)
    #expect(live.isLive(at: now))
    #expect(!live.isLive(at: now.addingTimeInterval(31)))
    live.save(cfg)
    #expect(AppGroupOwnerLease.load(cfg)?.sessionID == "s")
    AppGroupOwnerLease.clear(cfg)
    #expect(AppGroupOwnerLease.load(cfg) == nil)

    let store = RevisionedStateStore<Snapshot>(configuration: cfg, key: "snap")
    store.write { $0.phase = "recording" }
    let second = store.write { $0.phase = "ready" }
    #expect(second.revision == 2 && store.snapshot?.phase == "ready")
}

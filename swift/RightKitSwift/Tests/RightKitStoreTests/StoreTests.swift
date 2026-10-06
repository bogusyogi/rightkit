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

@Test func ownerLeaseLivenessAndRevisionedStateIsMonotonic() throws {
    let (cfg, _) = config()
    let now = Date()
    // A pid that cannot exist: a fresh timestamp must not keep a force-quit owner alive.
    let dead = AppGroupOwnerLease(owner: AppGroupOwnerLease.ownerToken(name: "app", pid: 2_000_000_000), sessionID: "s", savedAt: now)
    #expect(!dead.isLive(at: now))
    let live = AppGroupOwnerLease(owner: AppGroupOwnerLease.ownerToken(name: "app"), sessionID: "s", savedAt: now)
    #expect(live.isLive(at: now))
    #expect(!live.isLive(at: now.addingTimeInterval(31)))

    let store = RevisionedStateStore<Snapshot>(configuration: cfg, key: "snap")
    try store.write { $0.phase = "recording" }
    let second = try store.write { $0.phase = "ready" }
    #expect(second.revision == 2 && store.snapshot?.phase == "ready")
}

@Test func leaseClaimRenewReleaseAreOwnerAndEpochChecked() throws {
    let (cfg, _) = config()
    let t0 = Date()
    let app = AppGroupOwnerLease.ownerToken(name: "app")
    let ext = AppGroupOwnerLease.ownerToken(name: "ext")
    let mine = try AppGroupOwnerLease.claim(cfg, owner: app, sessionID: "s1", now: t0)
    #expect(AppGroupOwnerLease.load(cfg) == mine)
    #expect(throws: AppGroupStoreError.leaseHeld(owner: app)) {
        try AppGroupOwnerLease.claim(cfg, owner: ext, sessionID: "s2", now: t0.addingTimeInterval(1))
    }
    // Renew keeps the epoch and moves savedAt.
    let renewed = try mine.renew(cfg, now: t0.addingTimeInterval(10))
    #expect(renewed.epoch == mine.epoch && renewed.savedAt == t0.addingTimeInterval(10))

    // The holder lapses; another owner takes over. The stale holder can neither renew nor clear it.
    let taker = try AppGroupOwnerLease.claim(cfg, owner: ext, sessionID: "s2", now: t0.addingTimeInterval(100))
    #expect(taker.epoch == mine.epoch + 1)
    #expect(throws: AppGroupStoreError.leaseLost) { try mine.renew(cfg, now: t0.addingTimeInterval(101)) }
    #expect(try mine.release(cfg) == false)
    #expect(AppGroupOwnerLease.load(cfg) == taker, "a replacement lease survives the stale owner's release")
    #expect(try taker.release(cfg) == true)
    #expect(AppGroupOwnerLease.load(cfg) == nil)
}

@Test func noSharedContainerFailsClosedUnlessTestFallbackIsExplicit() throws {
    let suite = UserDefaults(suiteName: "rk-store-\(UUID().uuidString)")!
    // No lockDirectory and a group the process is not entitled to: production must not lock a private temp file.
    let closed = AppGroupConfiguration(
        groupID: "group.rightkit.not-entitled-\(UUID().uuidString)", defaultsOverride: suite)
    var ran = false
    #expect(AppGroupFileLock.withExclusiveLock(closed) { ran = true } == nil && !ran)
    let queue = AppGroupCommandQueue<Command>(configuration: closed)
    #expect(queue.submit(Command("x", at: Date())) == nil)

    var open = closed
    open.allowTemporaryLockFallback = true
    #expect(AppGroupFileLock.withExclusiveLock(open) { 1 } == 1)
}

@Test func lockIsHeldByARealSeparateProcess() throws {
    let (cfg, dir) = config(timeout: 30_000)
    let lockPath = dir.appendingPathComponent(cfg.lockName).path
    let holder = Process()
    holder.executableURL = URL(fileURLWithPath: "/usr/bin/perl")
    holder.arguments = ["-e", "use Fcntl qw(:flock); open(F, '>>', $ARGV[0]) or die; flock(F, LOCK_EX) or die; $|=1; print qq(held\\n); sleep 30;", lockPath]
    let out = Pipe()
    holder.standardOutput = out
    try holder.run()
    defer { holder.terminate(); holder.waitUntilExit() }
    #expect(String(decoding: out.fileHandleForReading.availableData, as: UTF8.self).contains("held"))

    let queue = AppGroupCommandQueue<Command>(configuration: cfg)
    #expect(queue.submit(Command("blocked", at: Date())) == nil, "another process holds the lock: no fabricated success")
    #expect(throws: AppGroupStoreError.lockUnavailable) { try RevisionedStateStore<Snapshot>(configuration: cfg, key: "k").write { $0.phase = "x" } }

    holder.terminate(); holder.waitUntilExit()
    #expect(queue.submit(Command("ok", at: Date())) != nil)
}

@Test func unavailableDefaultsFailEveryDurableWriteInsteadOfReportingSuccess() throws {
    let dir = FileManager.default.temporaryDirectory.appendingPathComponent("rk-store-\(UUID().uuidString)")
    try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    // NSGlobalDomain is not a valid suite: UserDefaults(suiteName:) returns nil, like a missing entitlement.
    let cfg = AppGroupConfiguration(groupID: "NSGlobalDomain", lockDirectory: dir)
    #expect(cfg.defaults == nil)
    #expect(AppGroupCommandQueue<Command>(configuration: cfg).submit(Command("x", at: Date())) == nil)
    #expect(throws: AppGroupStoreError.defaultsUnavailable) { try RevisionedStateStore<Snapshot>(configuration: cfg, key: "k").write { $0.phase = "x" } }
    #expect(throws: AppGroupStoreError.defaultsUnavailable) { try AppGroupOwnerLease.claim(cfg, owner: "a#1", sessionID: "s") }
}

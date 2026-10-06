import Foundation
import Testing
@testable import RightKitCloudSync

private actor ScriptedRunner: CloudSyncRunning {
    var results: [SyncResult]
    var hint: TimeInterval?
    var calls = 0
    var gate: CheckedContinuation<Void, Never>?
    var blockFirst = false
    var enabledChanges: [Bool] = []

    init(results: [SyncResult], blockFirst: Bool = false) {
        self.results = results
        self.blockFirst = blockFirst
    }

    func sync() async -> SyncResult {
        calls += 1
        if blockFirst {
            blockFirst = false
            await withCheckedContinuation { gate = $0 }
        }
        let next = results.isEmpty ? SyncResult(status: .ready) : results.removeFirst()
        if case .unavailable = next.status { hint = 5 } else { hint = nil }
        return next
    }
    func retryDelayRemaining() async -> TimeInterval? { hint }
    func setHint(_ v: TimeInterval?) { hint = v }
    func accountDidChange() async {}
    func setEnabled(_ enabled: Bool) async { enabledChanges.append(enabled) }
    func release() { gate?.resume(); gate = nil }
}

@Suite struct CoordinatorTests {
    @Test func concurrentTriggersAreSingleFlightCoalescedAndAllCallersGetAResult() async throws {
        let runner = ScriptedRunner(results: [SyncResult(uploaded: 1, status: .ready)], blockFirst: true)
        let c = CloudSyncCoordinator(runner: runner, enabledStore: InMemorySyncEnabledStore(enabled: true))
        let first = Task { await c.request(.launch) }
        while await runner.calls == 0 { await Task.yield() }
        // Five more arrive while the first pass is suspended.
        let others = (0..<5).map { _ in Task { await c.request(.localMutation) } }
        try await Task.sleep(nanoseconds: 50_000_000)
        await runner.release()
        _ = await first.value
        for o in others { let r = await o.value; #expect(r.status == .ready) }
        // One in-flight pass + exactly one coalesced follow-up, never six.
        let total = await runner.calls
        #expect(total == 2)
    }

    @Test func failedPassRetriesWithBackoffAndSuccessStopsIt() async throws {
        let runner = ScriptedRunner(results: [
            SyncResult(status: .unavailable("down")), SyncResult(status: .unavailable("down")), SyncResult(status: .ready),
        ])
        let delays = DelayLog()
        let c = CloudSyncCoordinator(
            runner: runner, enabledStore: InMemorySyncEnabledStore(enabled: true),
            sleep: { await delays.add($0); await runner.setHint(nil) }
        )
        let first = await c.request(.foreground)
        guard case .unavailable = first.status else { Issue.record("expected unavailable"); return }
        // Retry timer fires through the injected sleeper until the pass succeeds.
        for _ in 0..<200 where await runner.calls < 3 { try await Task.sleep(nanoseconds: 10_000_000) }
        let total = await runner.calls
        #expect(total == 3)
        // Backoff doubled from the 15s base; server hint (5s) may only lengthen, never shorten.
        let seen = await delays.values
        #expect(Array(seen.prefix(2)) == [15, 30])
    }

    @Test func disabledCoordinatorTouchesNothingAndOffIsAHardFence() async throws {
        let runner = ScriptedRunner(results: [])
        let enabled = InMemorySyncEnabledStore(enabled: false)
        let c = CloudSyncCoordinator(runner: runner, enabledStore: enabled)
        let off = await c.request(.launch)
        let offCalls = await runner.calls
        #expect(off.status == .disabled && offCalls == 0)
        enabled.setEnabled(true)
        await c.setEnabled(false) // runner is told, state flips
        let changes = await runner.enabledChanges
        #expect(changes == [false])
    }

    @Test func backoffAndRetryGateRules() {
        let p = SyncBackoffPolicy()
        #expect(p.delay(attempt: 1, serverHint: 0) == 15 && p.delay(attempt: 3, serverHint: 0) == 60)
        #expect(p.delay(attempt: 20, serverHint: 0) == 300 && p.delay(attempt: 1, serverHint: 90) == 90)
        #expect(p.delay(attempt: 1, serverHint: .nan) == 15)
        var gate = SyncRetryGate()
        gate.note(delay: 60, now: t(0))
        gate.note(delay: 10, now: t(0)) // never shortened
        #expect(gate.remaining(now: t(30)) == 30 && gate.remaining(now: t(61)) == nil)
        #expect(SyncTransportError.busy(retryAfter: 120).retryDelay == 120)
        #expect(SyncTransportError.networkUnavailable(retryAfter: nil).retryDelay == 15)
        #expect(SyncTransportError.quotaExceeded.retryDelay == nil)
    }
}

private actor DelayLog {
    var values: [TimeInterval] = []
    func add(_ v: TimeInterval) { values.append(v) }
}

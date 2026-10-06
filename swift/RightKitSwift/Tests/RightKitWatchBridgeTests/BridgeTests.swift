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

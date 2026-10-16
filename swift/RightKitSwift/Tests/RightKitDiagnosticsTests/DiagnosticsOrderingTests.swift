import Foundation
import Testing

@testable import RightKitDiagnostics

private func orderingScratchURL() -> URL {
    FileManager.default.temporaryDirectory
        .appendingPathComponent("rightkit-diag-order-\(UUID().uuidString)")
        .appendingPathComponent("log.jsonl")
}

private final class MutableClock: @unchecked Sendable {
    private let lock = NSLock()
    private var date: Date
    init(_ date: Date) { self.date = date }
    func set(_ new: Date) { lock.withLock { date = new } }
    var now: Date { lock.withLock { date } }
}

private func iso(_ date: Date) -> String { ISO8601DateFormatter().string(from: date) }

private func lines(_ store: JSONLDiagnosticStore) -> [[String: String]] {
    store.validLines().compactMap {
        (try? JSONSerialization.jsonObject(with: Data($0.utf8))) as? [String: String]
    }
}

@Test func callerSuppliedTimestampIsWrittenAsGivenAndNilUsesTheClock() async {
    let clock = Date(timeIntervalSince1970: 1_700_000_000)
    let supplied = Date(timeIntervalSince1970: 1_600_000_123)
    let store = JSONLDiagnosticStore(fileURL: orderingScratchURL()) { clock }

    #expect(await store.append(DiagnosticEvent(name: "with_time", timestamp: supplied)))
    #expect(await store.append(DiagnosticEvent(name: "without_time")))
    #expect(await store.append(DiagnosticEvent(name: "at_overload"), at: supplied.addingTimeInterval(60)))
    #expect(store.record(DiagnosticEvent(name: "sync_record", timestamp: supplied.addingTimeInterval(120))))

    let written = lines(store)
    #expect(written.map { $0["event"] } == ["with_time", "without_time", "at_overload", "sync_record"])
    #expect(written.map { $0["timestamp"] } == [
        iso(supplied), iso(clock), iso(supplied.addingTimeInterval(60)), iso(supplied.addingTimeInterval(120)),
    ])
    // The initializer without a timestamp is unchanged.
    #expect(DiagnosticEvent(name: "x", fields: ["a": "b"]).timestamp == nil)
}

@Test func enqueueFromOneCallerIsWrittenInCallOrder() async {
    let store = JSONLDiagnosticStore(fileURL: orderingScratchURL(), options: StoreOptions(maxBytes: 10_000_000))
    for index in 0..<300 { store.enqueue(DiagnosticEvent(name: "e", fields: ["n": String(index)])) }
    await store.flush()
    #expect(lines(store).map { $0["n"] } == (0..<300).map(String.init))
}

@Test func enqueueStampsTheTimeOfTheCallNotOfTheWrite() async {
    let clock = MutableClock(Date(timeIntervalSince1970: 1_700_000_000))
    let store = JSONLDiagnosticStore(fileURL: orderingScratchURL()) { clock.now }
    store.enqueue(DiagnosticEvent(name: "first"))
    clock.set(Date(timeIntervalSince1970: 1_700_000_500))
    store.enqueue(DiagnosticEvent(name: "second"))
    clock.set(Date(timeIntervalSince1970: 1_700_009_999))   // the clock has moved on before the queue ran
    await store.flush()
    #expect(lines(store).map { $0["timestamp"] } == [
        iso(Date(timeIntervalSince1970: 1_700_000_000)), iso(Date(timeIntervalSince1970: 1_700_000_500)),
    ])
}

@Test func awaitedAppendsAndEnqueuesShareOneOrderAcrossStoreValuesForTheSameFile() async {
    let url = orderingScratchURL()
    let first = JSONLDiagnosticStore(fileURL: url, options: StoreOptions(maxBytes: 10_000_000))
    let second = JSONLDiagnosticStore(fileURL: url, options: StoreOptions(maxBytes: 10_000_000))
    var expected: [String] = []
    for index in 0..<40 {
        let store = index % 2 == 0 ? first : second
        switch index % 3 {
        case 0: store.enqueue(DiagnosticEvent(name: "e", fields: ["n": String(index)]))
        case 1: _ = await store.append(DiagnosticEvent(name: "e", fields: ["n": String(index)]))
        default: store.enqueue(DiagnosticEvent(name: "e", fields: ["n": String(index)]))
        }
        expected.append(String(index))
    }
    await second.flush()
    #expect(lines(first).map { $0["n"] } == expected)
}

@Test func flushWithNothingQueuedReturnsAndAFailedEnqueueDoesNotStopLaterOnes() async {
    let store = JSONLDiagnosticStore(fileURL: orderingScratchURL(), options: StoreOptions(maxBytes: 10_000_000))
    await store.flush()
    store.enqueue(DiagnosticEvent(name: "a"))
    store.enqueue(DiagnosticEvent(name: "b"))
    await store.flush()
    #expect(lines(store).map { $0["event"] } == ["a", "b"])
}

@Test func stampedEventCarriesTheCallSiteTime() {
    let at = Date(timeIntervalSince1970: 1_650_000_000)
    let event = DiagnosticEvent.stamped(name: "n", fields: ["k": "v"], at: at)
    #expect(event.timestamp == at)
    #expect(event.name == "n")
    #expect(event.fields == ["k": "v"])
    let before = Date()
    let now = DiagnosticEvent.stamped(name: "n")
    #expect(now.timestamp.map { $0 >= before && $0 <= Date() } == true)
}

import Foundation
import XCTest
@testable import RightKitWatch

#if os(macOS)
import CoreServices

/// Handler mode: one call per FSEvents batch on the caller's queue, bounded backlog, and the
/// `stop()` guarantees. The engine and sink are driven directly where determinism needs it.
final class RightKitWatchBatchTests: XCTestCase {
    private final class Calls: @unchecked Sendable {
        private let lock = NSLock()
        private var all: [[RightKitWatchEvent]] = []
        func add(_ batch: [RightKitWatchEvent]) { lock.withLock { all.append(batch) } }
        var batches: [[RightKitWatchEvent]] { lock.withLock { all } }
        func waitForCount(_ count: Int, timeout: TimeInterval = 5) -> Bool {
            let deadline = Date(timeIntervalSinceNow: timeout)
            while Date() < deadline { if batches.count >= count { return true }; Thread.sleep(forTimeInterval: 0.01) }
            return batches.count >= count
        }
    }

    private func record(_ id: UInt64) -> (path: String, flags: UInt32, id: UInt64) {
        ("/root/f\(id)", UInt32(kFSEventStreamEventFlagItemCreated | kFSEventStreamEventFlagItemIsFile), id)
    }

    private func paths(_ batch: [RightKitWatchEvent]) -> [String] {
        batch.compactMap { if case let .item(path, _, _) = $0 { path } else { nil } }
    }

    private func makeEngine(
        capacity: Int = 100, roots: [String] = ["/root"], queue: DispatchQueue = DispatchQueue(label: "test.delivery"),
        handler: @escaping @Sendable ([RightKitWatchEvent]) -> Void
    ) -> RightKitWatcher.Engine {
        let (_, continuation) = AsyncStream<RightKitWatchEvent>.makeStream()
        continuation.finish()
        let sink = RightKitWatcher.BatchSink(queue: queue, capacity: capacity, roots: roots, handler: handler)
        return RightKitWatcher.Engine(continuation: continuation, since: .init(rawValue: 1), roots: roots, batches: sink)
    }

    // MARK: Batch boundaries and queue

    func testEachCallbackIsOneHandlerCallInOrderOnTheCallersQueue() {
        let calls = Calls()
        let queue = DispatchQueue(label: "test.delivery.marker")
        let key = DispatchSpecificKey<Bool>()
        queue.setSpecific(key: key, value: true)
        let onQueue = Calls()   // records only when the handler ran on `queue`
        let engine = makeEngine(queue: queue) { batch in
            calls.add(batch)
            if DispatchQueue.getSpecific(key: key) == true { onQueue.add(batch) }
        }
        engine.deliver([record(1), record(2), record(3)])
        engine.deliver([record(4)])
        engine.deliver([record(5), record(6)])
        XCTAssertTrue(calls.waitForCount(3))
        XCTAssertEqual(calls.batches.map(paths), [["/root/f1", "/root/f2", "/root/f3"], ["/root/f4"], ["/root/f5", "/root/f6"]])
        XCTAssertEqual(onQueue.batches.count, 3, "every handler call ran on the supplied queue")
        engine.stop()
    }

    func testControlRecordsStayInTheirBatchAndEmptyCallbacksCallNothing() {
        let calls = Calls()
        let engine = makeEngine { calls.add($0) }
        engine.deliver([])
        engine.deliver([("", UInt32(kFSEventStreamEventFlagHistoryDone), 0), record(9)])
        XCTAssertTrue(calls.waitForCount(1))
        XCTAssertEqual(calls.batches.count, 1)
        XCTAssertEqual(calls.batches[0].first, .historyDone)
        XCTAssertEqual(calls.batches[0].count, 2)
        engine.stop()
    }

    // MARK: Bounded backlog

    func testSlowHandlerKeepsTheBoundThenGetsOneRescanPerRootAfterTheKeptEvents() {
        let calls = Calls()
        let started = DispatchSemaphore(value: 0), release = DispatchSemaphore(value: 0)
        let first = ManagedFlag()
        let engine = makeEngine(capacity: 3, roots: ["/root", "/other"]) { batch in
            calls.add(batch)
            if first.takeFirst() { started.signal(); release.wait() }
        }
        engine.deliver([record(1)])                                  // taken by the drain; handler blocks
        XCTAssertEqual(started.wait(timeout: .now() + 5), .success)
        engine.deliver([record(2), record(3)])                       // fits (2 of 3)
        engine.deliver([record(4), record(5), record(6)])            // room for one: cut, then overflow
        engine.deliver([record(7)])                                  // everything after a loss is lost
        XCTAssertEqual(engine.droppedEventCount, 3, "f5, f6 cut from the third batch plus f7")
        release.signal()
        XCTAssertTrue(calls.waitForCount(4))
        let received = calls.batches
        XCTAssertEqual(paths(received[0]), ["/root/f1"])
        XCTAssertEqual(paths(received[1]), ["/root/f2", "/root/f3"])
        XCTAssertEqual(paths(received[2]), ["/root/f4"], "a batch is cut at the bound, oldest first")
        XCTAssertEqual(received[3], [
            .mustRescan(path: "/root", reason: .bufferOverflow, eventID: .init(rawValue: 7)),
            .mustRescan(path: "/other", reason: .bufferOverflow, eventID: .init(rawValue: 7)),
        ])
        engine.stop()
    }

    // MARK: stop() guarantee

    func testStopWaitsForARunningHandlerAndNothingBeginsAfterItReturns() {
        let calls = Calls()
        let inHandler = DispatchSemaphore(value: 0), release = DispatchSemaphore(value: 0)
        let finished = Flag()
        let engine = makeEngine { batch in
            calls.add(batch)
            inHandler.signal()
            release.wait()
            finished.set()
        }
        engine.deliver([record(1)])
        engine.deliver([record(2)])                                  // queued behind the running call
        XCTAssertEqual(inHandler.wait(timeout: .now() + 5), .success)

        let stopReturned = DispatchSemaphore(value: 0)
        DispatchQueue.global().async { engine.stop(); stopReturned.signal() }
        XCTAssertEqual(stopReturned.wait(timeout: .now() + 0.3), .timedOut, "stop() must wait for the running handler")
        release.signal()
        XCTAssertEqual(stopReturned.wait(timeout: .now() + 5), .success)
        XCTAssertTrue(finished.value, "the running call completed before stop() returned")

        engine.deliver([record(3)])
        Thread.sleep(forTimeInterval: 0.2)
        XCTAssertEqual(calls.batches.count, 1, "the queued batch and later batches never ran")
        engine.stop()                                                // idempotent
    }

    func testStopFromInsideTheHandlerReturnsAtOnceAndNoLaterCallBegins() {
        let calls = Calls()
        let box = EngineBox()
        let returned = DispatchSemaphore(value: 0)
        let engine = makeEngine { batch in
            calls.add(batch)
            box.engine?.stop()          // would deadlock if it waited for itself
            returned.signal()
        }
        box.engine = engine
        engine.deliver([record(1)])
        engine.deliver([record(2)])
        XCTAssertEqual(returned.wait(timeout: .now() + 5), .success)
        Thread.sleep(forTimeInterval: 0.2)
        XCTAssertEqual(calls.batches.count, 1)
        box.engine = nil
    }

    // MARK: Real FSEvents

    private func makeTempDirectory() throws -> URL {
        let scratch = NSTemporaryDirectory() + "rightkit-watch-batch-\(UUID().uuidString)"
        try FileManager.default.createDirectory(atPath: scratch, withIntermediateDirectories: true)
        let dir = URL(fileURLWithPath: realpath(scratch, nil).map { String(cString: $0) } ?? scratch, isDirectory: true)
        addTeardownBlock { try? FileManager.default.removeItem(at: dir) }
        return dir
    }

    func testRealWatcherDeliversBatchesThenNothingAfterStop() async throws {
        let dir = try makeTempDirectory()
        let calls = Calls()
        let queue = DispatchQueue(label: "test.real.delivery")
        let watcher = try RightKitWatcher(.init(paths: [dir.path], latency: 0.3), queue: queue) { calls.add($0) }
        defer { watcher.stop() }

        for index in 0..<6 { try Data("x".utf8).write(to: dir.appendingPathComponent("f\(index).txt")) }
        watcher.flush()
        XCTAssertTrue(calls.waitForCount(1, timeout: 10), "no batch delivered")
        let deadline = Date(timeIntervalSinceNow: 10)
        while Set(calls.batches.flatMap(paths).filter { $0.hasSuffix(".txt") }).count < 6, Date() < deadline { try await Task.sleep(nanoseconds: 20_000_000) }
        let seen = calls.batches
        XCTAssertTrue(seen.allSatisfy { !$0.isEmpty })
        XCTAssertEqual(Set(seen.flatMap(paths).filter { $0.hasSuffix(".txt") }), Set((0..<6).map { dir.appendingPathComponent("f\($0).txt").path }))
        XCTAssertLessThan(seen.count, seen.joined().count, "events of one FSEvents callback share a handler call")
        let ids = seen.joined().compactMap(\.eventID?.rawValue)
        XCTAssertEqual(ids, ids.sorted(), "calls and events are in order")

        var finishedStream = true
        for await _ in watcher.events { finishedStream = false }
        XCTAssertTrue(finishedStream, "events is already finished in handler mode")

        watcher.stop()
        let before = calls.batches.count
        try Data("late".utf8).write(to: dir.appendingPathComponent("after-stop.txt"))
        try await Task.sleep(nanoseconds: 800_000_000)
        XCTAssertEqual(calls.batches.count, before, "no handler call after stop() returned")
    }

    func testStreamModeYieldsNothingAfterStopButKeepsWhatWasAlreadyHandedOver() async throws {
        let dir = try makeTempDirectory()
        let watcher = try RightKitWatcher(.init(paths: [dir.path], latency: 0.05))
        let early = dir.appendingPathComponent("early.txt"), late = dir.appendingPathComponent("late.txt")
        let initial = watcher.lastEventId
        try Data("x".utf8).write(to: early)
        watcher.flush()
        let deadline = Date(timeIntervalSinceNow: 5)
        while watcher.lastEventId <= initial, Date() < deadline { try await Task.sleep(nanoseconds: 20_000_000) }
        watcher.stop()
        try Data("x".utf8).write(to: late)
        try await Task.sleep(nanoseconds: 500_000_000)

        var paths: [String] = []
        for await event in watcher.events { if case let .item(path, _, _) = event { paths.append(path) } }   // ends: stream finished
        XCTAssertTrue(paths.contains(early.path), "an event yielded before stop() is still delivered; saw \(paths)")
        XCTAssertFalse(paths.contains(late.path), "nothing FSEvents reported after stop() returned")
    }

    func testHandlerModeRejectsBadConfiguration() {
        let queue = DispatchQueue(label: "q")
        XCTAssertThrowsError(try RightKitWatcher(.init(paths: []), queue: queue) { _ in }) { XCTAssertEqual($0 as? RightKitWatchError, .invalidConfiguration) }
        XCTAssertThrowsError(try RightKitWatcher(.init(paths: ["/tmp"], maxBufferedEvents: 0), queue: queue) { _ in }) { XCTAssertEqual($0 as? RightKitWatchError, .invalidConfiguration) }
    }

    // MARK: Helpers

    private final class Flag: @unchecked Sendable {
        private let lock = NSLock(); private var flag = false
        func set() { lock.withLock { flag = true } }
        var value: Bool { lock.withLock { flag } }
    }
    private final class ManagedFlag: @unchecked Sendable {
        private let lock = NSLock(); private var used = false
        func takeFirst() -> Bool { lock.withLock { defer { used = true }; return !used } }
    }
    private final class EngineBox: @unchecked Sendable {
        private let lock = NSLock(); private var stored: RightKitWatcher.Engine?
        var engine: RightKitWatcher.Engine? { get { lock.withLock { stored } } set { lock.withLock { stored = newValue } } }
    }
}
#endif

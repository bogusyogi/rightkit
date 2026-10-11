import Foundation
import XCTest
@testable import RightKitWatch

#if os(macOS)
import CoreServices

/// Drains a watcher's stream into a lock-protected list so tests can poll for what they expect.
private final class Recorder: @unchecked Sendable {
    private let lock = NSLock()
    private var recorded: [RightKitWatchEvent] = []
    private var task: Task<Void, Never>?

    init(_ watcher: RightKitWatcher) {
        let events = watcher.events
        task = Task { [self] in
            for await event in events {
                append(event)
            }
        }
    }

    private func append(_ event: RightKitWatchEvent) { lock.withLock { recorded.append(event) } }

    var events: [RightKitWatchEvent] { lock.withLock { recorded } }

    /// Polls until an event satisfies `predicate` or `timeout` elapses.
    func wait(timeout: TimeInterval = 10, where predicate: @Sendable (RightKitWatchEvent) -> Bool) async -> RightKitWatchEvent? {
        let deadline = Date(timeIntervalSinceNow: timeout)
        while Date() < deadline {
            if let hit = events.first(where: predicate) { return hit }
            try? await Task.sleep(nanoseconds: 20_000_000)
        }
        return events.first(where: predicate)
    }

    func cancel() { task?.cancel() }
}

final class RightKitWatchTests: XCTestCase {
    private func makeTempDirectory() throws -> URL {
        // FSEvents reports real paths (/private/var/...), so resolve the symlinked temp root first.
        // (Foundation's resolvingSymlinksInPath deliberately strips /private, so use realpath.)
        let scratch = NSTemporaryDirectory() + "rightkit-watch-\(UUID().uuidString)"
        try FileManager.default.createDirectory(atPath: scratch, withIntermediateDirectories: true)
        let dir = URL(fileURLWithPath: realpath(scratch, nil).map { String(cString: $0) } ?? scratch, isDirectory: true)
        addTeardownBlock { try? FileManager.default.removeItem(at: dir) }
        return dir
    }

    private func isItem(_ path: String, with kind: RightKitWatchItemKind) -> @Sendable (RightKitWatchEvent) -> Bool {
        { event in
            if case let .item(p, k, _) = event { return p == path && k.contains(kind) }
            return false
        }
    }

    // MARK: Decoding (no filesystem)

    func testDecodeItemFlagsKeepEveryKindOnOneEvent() {
        let flags = UInt32(kFSEventStreamEventFlagItemCreated | kFSEventStreamEventFlagItemModified | kFSEventStreamEventFlagItemIsFile)
        XCTAssertEqual(
            RightKitWatchEvent.decode(path: "/a/b", flags: flags, id: 42),
            [.item(path: "/a/b", kind: [.created, .modified, .isFile], eventID: .init(rawValue: 42))]
        )
    }

    func testDecodeControlRecords() {
        XCTAssertEqual(RightKitWatchEvent.decode(path: "", flags: UInt32(kFSEventStreamEventFlagHistoryDone), id: 0), [.historyDone])
        XCTAssertEqual(
            RightKitWatchEvent.decode(path: "/root", flags: UInt32(kFSEventStreamEventFlagRootChanged), id: 7),
            [.rootChanged(path: "/root")]
        )
        XCTAssertEqual(
            RightKitWatchEvent.decode(path: "/d", flags: UInt32(kFSEventStreamEventFlagMustScanSubDirs | kFSEventStreamEventFlagUserDropped), id: 9),
            [.mustRescan(path: "/d", reason: [.subdirectories, .userDropped], eventID: .init(rawValue: 9))]
        )
    }

    func testDecodeReportsUnrecognisedRecordInsteadOfSwallowingIt() {
        XCTAssertEqual(
            RightKitWatchEvent.decode(path: "/x", flags: 0, id: 3),
            [.item(path: "/x", kind: [], eventID: .init(rawValue: 3))]
        )
    }

    func testEventIDRoundTripsThroughCodableAndOrders() throws {
        let id = RightKitWatchEventID(rawValue: 123_456_789_012)
        let decoded = try JSONDecoder().decode(RightKitWatchEventID.self, from: JSONEncoder().encode(id))
        XCTAssertEqual(decoded, id)
        XCTAssertLessThan(RightKitWatchEventID(rawValue: 1), id)
    }

    func testRejectsEmptyConfiguration() {
        XCTAssertThrowsError(try RightKitWatcher(.init(paths: []))) { XCTAssertEqual($0 as? RightKitWatchError, .invalidConfiguration) }
        XCTAssertThrowsError(try RightKitWatcher(.init(paths: [""]))) { XCTAssertEqual($0 as? RightKitWatchError, .invalidConfiguration) }
    }

    // MARK: Live filesystem

    func testReportsCreateThenModifyInTempDirectory() async throws {
        let dir = try makeTempDirectory()
        let watcher = try RightKitWatcher(.init(paths: [dir.path], latency: 0.05))
        let recorder = Recorder(watcher)
        defer { watcher.stop(); recorder.cancel() }

        let file = dir.appendingPathComponent("note.txt")
        try Data("one".utf8).write(to: file)
        let created = await recorder.wait(where: isItem(file.path, with: .created))
        XCTAssertNotNil(created, "no create event; saw \(recorder.events)")
        XCTAssertTrue(created.map { isItem(file.path, with: .isFile)($0) } ?? false, "file events carry the file type")

        // A distinct later modification, after the create has been seen and flushed.
        watcher.flush()
        let handle = try FileHandle(forWritingTo: file)
        try handle.seekToEnd(); try handle.write(contentsOf: Data("two".utf8)); try handle.close()
        let before = recorder.events.count
        let modified = await recorder.wait { event in
            guard case let .item(p, k, _) = event else { return false }
            return p == file.path && k.contains(.modified) && recorder.events.firstIndex(of: event).map { $0 >= before } ?? false
        }
        XCTAssertNotNil(modified, "no modify event after append; saw \(recorder.events)")
        XCTAssertGreaterThan(watcher.lastEventId.rawValue, 0)
        XCTAssertEqual(watcher.lastEventId, recorder.events.compactMap(\.eventID).max())
    }

    func testReplaysHistoryFromSavedEventIDThenReportsHistoryDone() async throws {
        let dir = try makeTempDirectory()
        // Bookmark first, then change the tree while nothing is watching.
        let bookmark = RightKitWatchEventID.current()
        let file = dir.appendingPathComponent("while-away.txt")
        try Data("x".utf8).write(to: file)
        try await Task.sleep(nanoseconds: 300_000_000)

        let watcher = try RightKitWatcher(.init(paths: [dir.path], sinceWhen: bookmark, latency: 0.05))
        let recorder = Recorder(watcher)
        defer { watcher.stop(); recorder.cancel() }
        let replayed = await recorder.wait(where: isItem(file.path, with: .created))
        let done = await recorder.wait { $0 == .historyDone }
        XCTAssertNotNil(replayed, "history replay missed the create; saw \(recorder.events)")
        XCTAssertNotNil(done, "no historyDone; saw \(recorder.events)")
    }

    func testReportsRootChangedWhenWatchedDirectoryIsRenamed() async throws {
        let dir = try makeTempDirectory()
        let root = dir.appendingPathComponent("watched", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        let watcher = try RightKitWatcher(.init(paths: [root.path], latency: 0.05))
        let recorder = Recorder(watcher)
        defer { watcher.stop(); recorder.cancel() }
        watcher.flush()

        try FileManager.default.moveItem(at: root, to: dir.appendingPathComponent("moved", isDirectory: true))
        let changed = await recorder.wait { event in
            if case let .rootChanged(path) = event { return path == root.path }
            return false
        }
        XCTAssertNotNil(changed, "no rootChanged; saw \(recorder.events)")
    }

    func testStopFinishesTheStream() async throws {
        let dir = try makeTempDirectory()
        let watcher = try RightKitWatcher(.init(paths: [dir.path]))
        let finished = Task { for await _ in watcher.events {}; return true }
        watcher.stop()
        let result = await finished.value
        XCTAssertTrue(result)
        watcher.stop() // idempotent
    }
}
#endif

#if os(macOS)
/// S04: a stalled consumer gets bounded memory and an explicit rescan signal, never silent loss.
/// The engine is driven directly (no FSEvents) so overflow is deterministic.
final class RightKitWatchBackpressureTests: XCTestCase {
    private func makeEngine(capacity: Int, roots: [String]) -> (RightKitWatcher.Engine, AsyncStream<RightKitWatchEvent>) {
        let (stream, continuation) = AsyncStream<RightKitWatchEvent>.makeStream(bufferingPolicy: .bufferingOldest(capacity))
        return (RightKitWatcher.Engine(continuation: continuation, since: .init(rawValue: 1), roots: roots), stream)
    }

    private func burst(_ count: Int, from first: UInt64 = 100) -> [(path: String, flags: UInt32, id: UInt64)] {
        (0..<count).map { ("/root/f\($0)", UInt32(kFSEventStreamEventFlagItemCreated | kFSEventStreamEventFlagItemIsFile), first + UInt64($0)) }
    }

    private func drain(_ stream: AsyncStream<RightKitWatchEvent>, upTo count: Int) async -> [RightKitWatchEvent] {
        var out: [RightKitWatchEvent] = []
        for await event in stream {
            out.append(event)
            if out.count == count { break }
        }
        return out
    }

    func testStalledConsumerKeepsOnlyTheBoundAndThenGetsOneRescanPerRoot() async {
        let (engine, stream) = makeEngine(capacity: 3, roots: ["/root", "/other"])
        engine.deliver(burst(50))  // nobody is reading
        XCTAssertEqual(engine.droppedEventCount, 47)

        // Room appears as the consumer resumes: 3 kept events, then the loss announcement.
        let received = await drain(stream, upTo: 3 + 2)
        XCTAssertEqual(received.count, 5)
        XCTAssertEqual(received.prefix(3).compactMap { event -> String? in
            if case let .item(path, _, _) = event { return path } else { return nil }
        }, ["/root/f0", "/root/f1", "/root/f2"], "the buffered events are the oldest, intact and ordered")
        XCTAssertEqual(received[3], .mustRescan(path: "/root", reason: .bufferOverflow, eventID: .init(rawValue: 149)))
        XCTAssertEqual(received[4], .mustRescan(path: "/other", reason: .bufferOverflow, eventID: .init(rawValue: 149)))
        engine.stop()
    }

    func testLiveEventsResumeAfterTheRescanMarkerInOrder() async {
        let (engine, stream) = makeEngine(capacity: 2, roots: ["/root"])
        engine.deliver(burst(10))
        var iterator = stream.makeAsyncIterator()
        _ = await iterator.next(); _ = await iterator.next()  // drain the two kept events
        // Consumer is current again; a new event must queue behind the marker, not ahead of it.
        engine.deliver(burst(1, from: 500))
        let marker = await iterator.next()
        let live = await iterator.next()
        XCTAssertEqual(marker, .mustRescan(path: "/root", reason: .bufferOverflow, eventID: .init(rawValue: 109)))
        XCTAssertEqual(live, .item(path: "/root/f0", kind: [.created, .isFile], eventID: .init(rawValue: 500)))
        engine.stop()
    }

    func testMarkerStillArrivesWhenTheFilesystemGoesQuietBeforeTheConsumerResumes() async {
        let (engine, stream) = makeEngine(capacity: 1, roots: ["/root"])
        engine.deliver(burst(5))  // overflow, then no further FSEvents at all
        var iterator = stream.makeAsyncIterator()
        _ = await iterator.next()  // free the only slot; the retry timer must deliver the marker
        let marker = await iterator.next()
        XCTAssertEqual(marker, .mustRescan(path: "/root", reason: .bufferOverflow, eventID: .init(rawValue: 104)))
        engine.stop()
    }

    func testNoOverflowSignalWhenTheConsumerKeepsUp() async {
        let (engine, stream) = makeEngine(capacity: 4, roots: ["/root"])
        var iterator = stream.makeAsyncIterator()
        for round in 0..<20 {
            engine.deliver(burst(3, from: UInt64(100 + round * 10)))
            for _ in 0..<3 { let event = await iterator.next(); XCTAssertNotNil(event) }
        }
        XCTAssertEqual(engine.droppedEventCount, 0)
        engine.stop()
    }

    func testRejectsAZeroBuffer() {
        XCTAssertThrowsError(try RightKitWatcher(.init(paths: ["/tmp"], maxBufferedEvents: 0))) {
            XCTAssertEqual($0 as? RightKitWatchError, .invalidConfiguration)
        }
    }

    func testRealWatcherWithStalledReaderIsBoundedAndSignalsRescan() async throws {
        let scratch = NSTemporaryDirectory() + "rightkit-watch-bp-\(UUID().uuidString)"
        try FileManager.default.createDirectory(atPath: scratch, withIntermediateDirectories: true)
        let root = realpath(scratch, nil).map { String(cString: $0) } ?? scratch
        defer { try? FileManager.default.removeItem(atPath: scratch) }
        let watcher = try RightKitWatcher(.init(paths: [root], latency: 0.05, maxBufferedEvents: 4))
        defer { watcher.stop() }
        for index in 0..<200 { try Data("x".utf8).write(to: URL(fileURLWithPath: root + "/f\(index)")) }
        watcher.flush()
        let deadline = Date(timeIntervalSinceNow: 10)
        while watcher.droppedEventCount == 0, Date() < deadline { try await Task.sleep(nanoseconds: 20_000_000) }
        XCTAssertGreaterThan(watcher.droppedEventCount, 0, "200 files through a 4-event buffer must overflow")

        // Now read: at most 4 kept events, then the explicit rescan for the root.
        var sawRescan = false
        var kept = 0
        for await event in watcher.events {
            if case let .mustRescan(path, reason, _) = event, reason.contains(.bufferOverflow) {
                XCTAssertEqual(path, root)
                sawRescan = true
                break
            }
            kept += 1
            if kept > 4 { break }
        }
        XCTAssertTrue(sawRescan)
        XCTAssertLessThanOrEqual(kept, 4)
    }
}
#endif

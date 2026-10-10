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

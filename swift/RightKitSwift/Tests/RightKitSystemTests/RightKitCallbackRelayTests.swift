import Foundation
import XCTest
@testable import RightKitSystem

/// PK16-2 (HeardRight pill callbacks): the ready gate, the bounded backlog and the off-thread delivery
/// the shipped `PillCallbacks` has, with delivery order made explicit.
final class RightKitCallbackRelayTests: XCTestCase {
    private final class Lines: @unchecked Sendable {
        private let lock = NSLock()
        private var _all: [String] = []
        func add(_ surface: String, _ message: String) { lock.withLock { _all.append("\(surface):\(message)") } }
        var all: [String] { lock.withLock { _all } }
    }

    private func relay(limit: Int = 256, _ lines: Lines) -> RightKitCallbackRelay {
        RightKitCallbackRelay(pendingLimit: limit, sink: { lines.add($0, $1) })
    }

    func testLinesWaitForTheOwnerThenArriveOldestFirst() {
        let lines = Lines()
        let relay = relay(lines)
        relay.log("pill", "one")
        relay.log("pill", "two")
        relay.waitUntilDelivered()
        XCTAssertEqual(lines.all, [], "nothing is delivered before the owner is ready")
        XCTAssertEqual(relay.pendingCount, 2)
        XCTAssertFalse(relay.isReady)

        relay.markReady()
        relay.log("pill", "three")
        relay.waitUntilDelivered()
        XCTAssertEqual(lines.all, ["pill:one", "pill:two", "pill:three"])
        XCTAssertTrue(relay.isReady)
        XCTAssertEqual(relay.pendingCount, 0)
    }

    func testTheBacklogIsBoundedAndDropsTheOldestLine() {
        let lines = Lines()
        let relay = relay(limit: 3, lines)
        for index in 1...5 { relay.log("s", "m\(index)") }
        XCTAssertEqual(relay.pendingCount, 3)
        relay.markReady()
        relay.waitUntilDelivered()
        XCTAssertEqual(lines.all, ["s:m3", "s:m4", "s:m5"])
    }

    func testMarkingReadyTwiceDeliversNothingTwice() {
        let lines = Lines()
        let relay = relay(lines)
        relay.log("s", "once")
        relay.markReady()
        relay.markReady()
        relay.waitUntilDelivered()
        XCTAssertEqual(lines.all, ["s:once"])
    }

    func testReadyLinesKeepCallOrder() {
        let lines = Lines()
        let relay = relay(lines)
        relay.markReady()
        for index in 0..<200 { relay.log("s", "\(index)") }
        relay.waitUntilDelivered()
        XCTAssertEqual(lines.all, (0..<200).map { "s:\($0)" })
    }

    private final class Flags: @unchecked Sendable {
        private let lock = NSLock()
        private var _values: [Int] = []
        private var _sameThread = true
        func add(_ value: Int) { lock.withLock { _values.append(value) } }
        var values: [Int] { lock.withLock { _values } }
        var sameThread: Bool {
            get { lock.withLock { _sameThread } }
            set { lock.withLock { _sameThread = newValue } }
        }
    }

    func testPerformRunsAwayFromTheCallingThread() {
        let relay = relay(Lines())
        let done = expectation(description: "action ran")
        let flags = Flags()
        let caller = Thread.current
        relay.perform {
            flags.sameThread = Thread.current == caller
            done.fulfill()
        }
        wait(for: [done], timeout: 5)
        XCTAssertFalse(flags.sameThread)
    }

    func testMainThreadHelpersRunInlineOnMainAndHopOtherwise() async {
        // From the main thread: inline, so ordering is preserved and no deadlock is possible.
        await MainActor.run {
            let order = Flags()
            RightKitMainThread.run { order.add(1) }
            order.add(2)
            XCTAssertEqual(order.values, [1, 2], "an action requested on the main thread ran before the next statement")
            XCTAssertEqual(RightKitMainThread.sync { 42 }, 42)
        }
        // From a background thread: the answer comes from the main thread.
        let onMain = await Task.detached { RightKitMainThread.sync { Thread.isMainThread } }.value
        XCTAssertTrue(onMain)
    }
}

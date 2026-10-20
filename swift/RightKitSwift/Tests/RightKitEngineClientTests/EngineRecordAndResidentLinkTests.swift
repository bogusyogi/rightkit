import Foundation
import XCTest

@testable import RightKitEngineClient

// PK16-2 (Pulse hub / helper transport): the `resident.json` record shape the daemon heartbeat really
// writes, and the wanted-and-healthy attach lifecycle moved out of Pulse's `ResidentEngineClient`.

final class EngineRecordTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 1_000_000)

    private func endpoint(_ json: String) -> String? {
        AttachedCore.endpoint(fromRecord: Data(json.utf8), now: now)
    }

    /// The record `rightkit_daemon::ticker::spawn_heartbeat` writes (see pulse `engine/src/resident.rs`):
    /// the endpoint sits under `state`, beside `updated_ms`, and carries unrelated fields.
    func testHeartbeatRecordNamesTheEndpointWhileFresh() {
        let record = #"{"pid":42,"updated_ms":999990000,"state":{"service":"pulse","endpoint":"/tmp/engine.sock","version":"1.2.3","startedMs":1,"membrane":"running","phase":"ready"}}"#
        XCTAssertEqual(endpoint(record), "/tmp/engine.sock")
    }

    func testStaleHeartbeatIsNotARunningEngine() {
        // 100000 s old: left behind by a crash or a kill.
        let stale = #"{"pid":42,"updated_ms":900000000,"state":{"endpoint":"/tmp/engine.sock"}}"#
        XCTAssertNil(endpoint(stale))
        // Exactly at the limit is still fresh; one second past it is not.
        XCTAssertEqual(endpoint(#"{"pid":1,"updated_ms":999970000,"state":{"endpoint":"/e"}}"#), "/e")
        XCTAssertNil(endpoint(#"{"pid":1,"updated_ms":999969000,"state":{"endpoint":"/e"}}"#))
    }

    func testHeartbeatWithoutATimestampOrEndpointIsRejected() {
        XCTAssertNil(endpoint(#"{"pid":1,"state":{"endpoint":"/e"}}"#), "no updated_ms, so freshness cannot be shown")
        XCTAssertNil(endpoint(#"{"pid":1,"updated_ms":999990000,"state":{"endpoint":""}}"#))
        XCTAssertNil(endpoint(#"{"pid":1,"updated_ms":999990000,"state":{"phase":"ready"}}"#))
    }

    func testAFlatRecordIsStillRead() {
        XCTAssertEqual(endpoint(#"{"endpoint":"/tmp/flat.sock"}"#), "/tmp/flat.sock")
        XCTAssertNil(endpoint(#"{"endpoint":""}"#))
    }

    func testUnreadableRecordsNameNothing() {
        XCTAssertNil(endpoint("not json"))
        XCTAssertNil(endpoint("[]"))
        XCTAssertNil(endpoint("{}"))
    }
}

#if os(macOS)
final class ResidentEngineLinkTests: XCTestCase {
    private final class Switches: @unchecked Sendable {
        private let lock = NSLock()
        private var _wanted = true
        private var _healthy = true
        private var _made = 0
        var wanted: Bool {
            get { lock.withLock { _wanted } }
            set { lock.withLock { _wanted = newValue } }
        }
        var healthy: Bool {
            get { lock.withLock { _healthy } }
            set { lock.withLock { _healthy = newValue } }
        }
        var made: Int { lock.withLock { _made } }
        func noteMade() { lock.withLock { _made += 1 } }
    }

    private func link(_ switches: Switches) -> ResidentEngineLink {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("rk-link-\(UUID().uuidString)", isDirectory: true)
        return ResidentEngineLink(
            wanted: { switches.wanted },
            healthy: { switches.healthy },
            makeEngine: {
                switches.noteMade()
                return AttachedEngine(
                    stateDirectory: directory,
                    expectedExecutables: [URL(fileURLWithPath: "/nonexistent/engine")],
                    app: "example-notch",
                    engineName: "Example",
                    eventsPollMethod: "example.events.poll"
                )
            }
        )
    }

    func testNothingIsCreatedWhileTheEngineIsNotWantedAndCallsFailRetryable() async {
        let switches = Switches()
        switches.wanted = false
        let link = link(switches)
        defer { link.detach() }
        XCTAssertFalse(link.isAvailable)
        XCTAssertNil(link.current())
        XCTAssertEqual(switches.made, 0)
        do {
            _ = try await link.call("ping", params: Data("{}".utf8))
            XCTFail("expected engine_off")
        } catch let error as EngineError {
            XCTAssertEqual(error, EngineError.engineOff)
            XCTAssertEqual(error.code, "engine_off")
            XCTAssertTrue(error.retryable)
        } catch {
            XCTFail("unexpected \(error)")
        }
    }

    func testOneEngineIsAttachedAndReusedThenDroppedWhenUnhealthy() {
        let switches = Switches()
        let link = link(switches)
        defer { link.detach() }
        let first = link.current()
        XCTAssertNotNil(first)
        XCTAssertIdentical(link.current(), first, "the engine is reused, not rebuilt per call")
        XCTAssertEqual(switches.made, 1)
        XCTAssertTrue(link.isAttached)

        switches.healthy = false
        XCTAssertNil(link.current())
        XCTAssertFalse(link.isAttached, "an unhealthy service releases the connection")

        switches.healthy = true
        let second = link.current()
        XCTAssertNotNil(second)
        XCTAssertNotIdentical(second, first, "a later attach builds a fresh engine")
        XCTAssertEqual(switches.made, 2)
    }

    func testDetachIsIdempotentAndLeavesTheLinkReusable() {
        let switches = Switches()
        let link = link(switches)
        defer { link.detach() }
        _ = link.current()
        link.detach()
        link.detach()
        XCTAssertFalse(link.isAttached)
        XCTAssertNotNil(link.current())
        XCTAssertEqual(switches.made, 2)
    }
}
#endif

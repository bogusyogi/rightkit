import Foundation
import XCTest

@testable import RightKitEngineClient

final class EngineClientTests: XCTestCase {
    func testEngineErrorDescriptionAndCodableRoundTrip() throws {
        let error = EngineError(code: "busy", message: "try later", retryable: true)
        XCTAssertEqual(error.description, "busy: try later")
        let data = try JSONEncoder().encode(error)
        XCTAssertEqual(try JSONDecoder().decode(EngineError.self, from: data), error)
    }

    func testEngineEmptyEncodesAsEmptyObject() throws {
        XCTAssertEqual(String(decoding: try JSONEncoder().encode(EngineEmpty()), as: UTF8.self), "{}")
    }

    func testApplicationSupportDirectoryTakesTheProductFolderName() {
        let url = AttachedEngine.applicationSupportDirectory(named: "Example")
        XCTAssertTrue(url.path.hasSuffix("Library/Application Support/Example"))
    }

    func testConfigurationIsExactlyWhatTheAppPassed() {
        let engine = AttachedEngine(
            stateDirectory: URL(fileURLWithPath: "/tmp/rk-state"),
            expectedExecutables: [URL(fileURLWithPath: "/tmp/rk-engine")],
            app: "example-hub",
            engineName: "Example",
            eventsPollMethod: "example.events.poll"
        )
        XCTAssertEqual(engine.app, "example-hub")
        XCTAssertEqual(engine.engineName, "Example")
        XCTAssertEqual(engine.eventsPollMethod, "example.events.poll")
        XCTAssertNil(engine.endpoint)
        XCTAssertEqual(engine.stateDirectory.path, "/tmp/rk-state")
    }

    /// With no `resident.json` the call fails with the retryable `engine_starting`, and the message
    /// names the product the app configured (never a RightKit-chosen name).
    #if os(macOS)
    func testCallWithoutRecordIsRetryableStartingNamingTheConfiguredEngine() async throws {
        let empty = FileManager.default.temporaryDirectory
            .appendingPathComponent("rk-engine-client-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: empty, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: empty) }
        let engine = AttachedEngine(
            stateDirectory: empty,
            expectedExecutables: [URL(fileURLWithPath: "/nonexistent/engine")],
            app: "example-hub",
            engineName: "Example",
            eventsPollMethod: "example.events.poll"
        )
        defer { engine.detach() }
        do {
            _ = try await engine.call("ping", params: Data("{}".utf8), timeout: .seconds(2))
            XCTFail("expected engine_starting")
        } catch let error as EngineError {
            XCTAssertEqual(error.code, "engine_starting")
            XCTAssertTrue(error.retryable)
            XCTAssertTrue(error.message.hasPrefix("Example is starting"), error.message)
        }
    }
    #endif
}

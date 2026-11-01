import XCTest
@testable import RightKitAgent
@MainActor
final class SpineTests: XCTestCase {
    func testPairingURLParses() {
        let url = URL(string: "coderight://pair?base=http://192.168.1.9:5099&token=abc123&label=Studio")!
        let cfg = DaemonConfig.fromPairingURL(url)
        XCTAssertEqual(cfg?.base, "http://192.168.1.9:5099")
        XCTAssertEqual(cfg?.token, "abc123")
        XCTAssertEqual(cfg?.label, "Studio")
    }

    func testBaseNormalizationStripsTrailingSlash() {
        XCTAssertEqual(DaemonConfig.normalizeBase("http://x:5099/"), "http://x:5099")
    }

    func testAssistantCompletedMapsToMessage() throws {
        let json = #"{"$kind":"assistantMessageCompleted","seq":3,"message":{"content":"hello"}}"#
        let ev = try JSONDecoder().decode(ProtocolEvent.self, from: Data(json.utf8))
        XCTAssertEqual(ev.displayMessage()?.role, .assistant)
        XCTAssertEqual(ev.displayMessage()?.text, "hello")
    }

    func testUnknownKindHasNoDisplay() throws {
        let json = #"{"$kind":"somethingNew","seq":9}"#
        let ev = try JSONDecoder().decode(ProtocolEvent.self, from: Data(json.utf8))
        XCTAssertNil(ev.displayMessage())
    }
}

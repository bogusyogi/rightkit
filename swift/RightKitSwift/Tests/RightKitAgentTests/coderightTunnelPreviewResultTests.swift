import XCTest
@testable import RightKitAgent
@MainActor
final class TunnelPreviewResultTests: XCTestCase {
    func testStartedTunnelResultBecomesPublicHTTPSPreview() {
        let result = #"{"event":"dev_tunnel_started","public_hostname":"preview.example.com"}"#

        XCTAssertEqual(
            TunnelPreviewResult.url(from: result)?.absoluteString,
            "https://preview.example.com"
        )
    }

    func testOnlyStartedTunnelResultsCanOpenPreview() {
        let stopped = #"{"event":"dev_tunnel_stopped","public_hostname":"preview.example.com"}"#
        let malformed = #"{"event":"tunnel_started","public_hostname":"https://preview.example.com/path"}"#

        XCTAssertNil(TunnelPreviewResult.url(from: stopped))
        XCTAssertNil(TunnelPreviewResult.url(from: malformed))
    }
}

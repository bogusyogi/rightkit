#if canImport(ActivityKit) && os(iOS)
import ActivityKit
import XCTest
import RightKitLiveActivity

private struct QualificationActivity: ActivityAttributes, Sendable {
    struct ContentState: Codable, Hashable, Sendable {}
}

final class DisabledLiveActivityTests: XCTestCase {
    func testBothHostsReturnDisabledBeforeRequestingAnActivity() async throws {
        try XCTSkipIf(ActivityAuthorizationInfo().areActivitiesEnabled,
            "Requires native iOS runner with Live Activities disabled; an enabled device is a separate qualification row")
        let content = ActivityContent(state: QualificationActivity.ContentState(), staleDate: nil)
        let host = LiveActivityHost<QualificationActivity>()
        do {
            _ = try await host.start(attributes: QualificationActivity(), content: content, isMatchingOrphan: { _ in false })
            XCTFail("Disabled Live Activities must refuse before any system request")
        } catch LiveActivityStartError.disabled {} catch { XCTFail("Unexpected error: \(error)") }
        let keyed = KeyedLiveActivityHost<String, QualificationActivity>()
        do {
            _ = try await keyed.start(key: "qualification", attributes: QualificationActivity(), content: content, isMatchingOrphan: { _ in false })
            XCTFail("Disabled Live Activities must refuse before any system request")
        } catch LiveActivityStartError.disabled {} catch { XCTFail("Unexpected error: \(error)") }
        XCTAssertNil(host.activity)
        XCTAssertTrue(keyed.activities.isEmpty)
    }
}
#endif

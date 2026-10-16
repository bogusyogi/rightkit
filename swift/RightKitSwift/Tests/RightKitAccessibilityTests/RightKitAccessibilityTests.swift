import Foundation
import XCTest
@testable import RightKitAccessibility

#if os(macOS)
final class RightKitAccessibilityTests: XCTestCase {
    func testFailureCodesMapToCases() {
        XCTAssertEqual(RightKitAXFailure(code: -25211), .apiDisabled)
        XCTAssertEqual(RightKitAXFailure(code: -25212), .noValue)
        XCTAssertEqual(RightKitAXFailure(code: -25202), .invalidElement)
        XCTAssertEqual(RightKitAXFailure(code: -25205), .attributeUnsupported)
        XCTAssertEqual(RightKitAXFailure(code: -25204), .cannotComplete)
        XCTAssertEqual(RightKitAXFailure(code: -1), .other(-1))
    }

    func testWindowLifetimeNotificationsIncludeCreateAndDestroy() {
        let names = RightKitAXNotifications.windowLifetime
        XCTAssertTrue(names.contains("AXWindowCreated"))
        XCTAssertTrue(names.contains("AXUIElementDestroyed"))
        XCTAssertEqual(Set(names).count, names.count)
    }

    /// Reading the process's own element needs no permission to construct, and `pid` does not
    /// message the target, so this runs unattended without Accessibility trust.
    func testApplicationElementReportsItsPid() {
        let pid = ProcessInfo.processInfo.processIdentifier
        let app = RightKitAXElement.application(pid: pid)
        XCTAssertEqual(app.pid, pid)
        XCTAssertEqual(app, RightKitAXElement.application(pid: pid))
    }
}
#endif

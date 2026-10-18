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

    // MARK: Parameterized attributes and settability (no Accessibility trust needed)

    func testCFRangeAXValueRoundTrip() throws {
        let range = CFRange(location: 7, length: 12)
        let value = try XCTUnwrap(RightKitAXElement.axValue(range: range))
        XCTAssertEqual(AXValueGetType(value), .cfRange)
        let back = try XCTUnwrap(RightKitAXElement.range(from: value))
        XCTAssertEqual(back.location, 7)
        XCTAssertEqual(back.length, 12)
    }

    func testCGRectAXValueRoundTrip() throws {
        var rect = CGRect(x: 10.5, y: 20, width: 300, height: 40.25)
        let value = try XCTUnwrap(AXValueCreate(.cgRect, &rect))
        XCTAssertEqual(RightKitAXElement.rect(from: value), rect)
    }

    func testValueUnwrapRejectsWrongTypes() throws {
        var point = CGPoint(x: 1, y: 2)
        let pointValue = try XCTUnwrap(AXValueCreate(.cgPoint, &point))
        XCTAssertNil(RightKitAXElement.rect(from: pointValue))
        XCTAssertNil(RightKitAXElement.range(from: pointValue))
        XCTAssertNil(RightKitAXElement.rect(from: "text" as NSString))
        XCTAssertNil(RightKitAXElement.range(from: NSNumber(value: 3)))
    }

    /// A pid that does not exist yields an element no call can resolve: every new call degrades to
    /// false or nil rather than throwing or trapping, whether or not this process is trusted.
    func testCallsOnUnresolvableElementDegrade() {
        let element = RightKitAXElement.application(pid: 0x7FFF_FFF0)
        XCTAssertFalse(element.isSettable(kAXValueAttribute))
        XCTAssertNil(element.boundsForRange(CFRange(location: 0, length: 1)))
        XCTAssertNil(element.parameterized(kAXBoundsForRangeParameterizedAttribute, NSNumber(value: 0)))
        XCTAssertNil(element.range(kAXSelectedTextRangeAttribute))
        XCTAssertNil(element.int(kAXNumberOfCharactersAttribute))
        XCTAssertNil(element.bool(kAXFocusedAttribute))
        XCTAssertNil(element.role)
        XCTAssertTrue(element.children.isEmpty)
    }
}
#endif

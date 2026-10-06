import XCTest
@testable import RightKitOverlay
#if os(macOS)
import AppKit
import CoreGraphics
#endif

final class OverlayTests: XCTestCase {
    func testDefaultHitboxFollowsFrameAfterPlacement() {
        var model = RightKitOverlayModel(kind: .heardRightPill, frame: RightKitOverlayRect(x: 900, y: 900, width: 64, height: 10))
        model.apply(.recording)
        model.place(in: RightKitOverlayWorkArea(left: 0, top: 0, right: 800, bottom: 600))
        XCTAssertEqual(model.frame, RightKitOverlayRect(x: 736, y: 590, width: 64, height: 10))
        XCTAssertTrue(model.hitTest(x: 760, y: 595))
        XCTAssertFalse(model.hitTest(x: 920, y: 905), "old pre-placement frame must not stay hittable")
    }

    func testExplicitHitboxKeepsOffsetThroughPlacement() {
        var model = RightKitOverlayModel(kind: .heardRightPill, frame: RightKitOverlayRect(x: 900, y: 900, width: 64, height: 10))
        model.hitbox = RightKitOverlayRect(x: 890, y: 890, width: 84, height: 30)
        model.apply(.recording)
        model.place(in: RightKitOverlayWorkArea(left: 0, top: 0, right: 800, bottom: 600))
        XCTAssertEqual(model.hitbox, RightKitOverlayRect(x: 726, y: 580, width: 84, height: 30))
    }

#if os(macOS)
    @MainActor
    func testPanelIsNonActivatingAndStatusLevel() {
        let panel = RightKitOverlayPanel(contentRect: NSRect(x: 10, y: 10, width: 64, height: 10))
        XCTAssertTrue(panel.styleMask.contains(.nonactivatingPanel))
        XCTAssertEqual(panel.level, .statusBar)
        XCTAssertTrue(panel.collectionBehavior.contains(.canJoinAllSpaces))
        XCTAssertTrue(panel.collectionBehavior.contains(.stationary))
        XCTAssertFalse(panel.collectionBehavior.contains(.fullScreenAuxiliary))
        XCTAssertFalse(panel.canBecomeKey)
        XCTAssertFalse(panel.canBecomeMain)
    }

    @MainActor
    func testCockpitNotchMayJoinFullscreenAuxiliarySpace() {
        let panel = RightKitOverlayPanel(contentRect: NSRect(x: 0, y: 0, width: 360, height: 64), kind: .cockpitNotch)
        XCTAssertTrue(panel.collectionBehavior.contains(.fullScreenAuxiliary))
    }

    @MainActor
    func testOrderingPanelDoesNotActivateRightKit() {
        let before = NSWorkspace.shared.frontmostApplication?.processIdentifier
        let screenFrames = NSScreen.screens.map(\.frame)
        guard let minimumScreenX = screenFrames.map(\.minX).min() else {
            XCTFail("no NSScreen frames available for offscreen fixture")
            return
        }
        let visibleWorkArea = NSScreen.screens
            .map(\.visibleFrame)
            .reduce(CGRect.null) { $0.union($1) }
        let uniqueOffset = CGFloat(ProcessInfo.processInfo.processIdentifier % 1_024 + 1)
        let fixtureY = visibleWorkArea.isNull ? 16 : visibleWorkArea.minY + 16
        let fixtureRect = NSRect(
            x: minimumScreenX - 64 - uniqueOffset,
            y: fixtureY,
            width: 64,
            height: 10
        )
        let panel = RightKitOverlayPanel(contentRect: fixtureRect)
        panel.setFrame(fixtureRect, display: false)
        defer { panel.orderOut(nil) }
        guard screenFrames.allSatisfy({ panel.frame.maxX < $0.minX }) else {
            XCTFail("fixture must be left of every NSScreen frame before ordering")
            return
        }
        let content = NSView(frame: NSRect(origin: .zero, size: panel.contentRect(forFrameRect: panel.frame).size))
        content.wantsLayer = true
        panel.contentView = content
        panel.showWithoutActivating()
        RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.1))
        XCTAssertEqual(NSWorkspace.shared.frontmostApplication?.processIdentifier, before)

        let windows = (CGWindowListCopyWindowInfo(
            [.optionAll],
            kCGNullWindowID
        ) as? [[String: Any]]) ?? []
        guard let info = windows.first(where: {
            ($0[kCGWindowNumber as String] as? NSNumber)?.intValue == panel.windowNumber
        }) else {
            XCTFail("ordered panel was not visible in CGWindowList")
            return
        }
        guard let bounds = CGRect(dictionaryRepresentation: info[kCGWindowBounds as String] as! CFDictionary) else {
            XCTFail("ordered panel had no readable CGWindow bounds")
            return
        }
        XCTAssertEqual(bounds.width, panel.frame.width, accuracy: 1)
        XCTAssertEqual(bounds.height, panel.frame.height, accuracy: 1)
        XCTAssertTrue(
            screenFrames.allSatisfy { bounds.maxX < $0.minX },
            "ordered panel must remain left of every NSScreen frame"
        )
        let layer = (info[kCGWindowLayer as String] as? NSNumber)?.intValue
        XCTAssertGreaterThanOrEqual(layer ?? Int.min, panel.level.rawValue)
    }
#endif
}

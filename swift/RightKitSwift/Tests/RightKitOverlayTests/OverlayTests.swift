import XCTest
@testable import RightKitOverlay
#if os(macOS)
import AppKit
import CoreGraphics
#endif

final class OverlayTests: XCTestCase {
    func testDefaultHitboxFollowsFrameAfterPlacement() {
        var model = RightKitOverlayModel(kind: .pill, frame: RightKitOverlayRect(x: 900, y: 900, width: 64, height: 10))
        model.apply(.recording)
        model.place(in: RightKitOverlayWorkArea(left: 0, top: 0, right: 800, bottom: 600))
        XCTAssertEqual(model.frame, RightKitOverlayRect(x: 736, y: 590, width: 64, height: 10))
        XCTAssertTrue(model.hitTest(x: 760, y: 595))
        XCTAssertFalse(model.hitTest(x: 920, y: 905), "old pre-placement frame must not stay hittable")
    }

    func testKindKeepsItsPersistedRawValuesAndDecodesLegacyJSON() throws {
        XCTAssertEqual(RightKitOverlayKind.pill.rawValue, "heardRightPill")
        XCTAssertEqual(RightKitOverlayKind.notch.rawValue, "cockpitNotch")
        let decoded = try JSONDecoder().decode([RightKitOverlayKind].self, from: Data(#"["heardRightPill","cockpitNotch"]"#.utf8))
        XCTAssertEqual(decoded, [.pill, .notch])
        XCTAssertEqual(try JSONEncoder().encode(RightKitOverlayKind.notch), Data(#""cockpitNotch""#.utf8))
    }

    @available(*, deprecated, message: "exercises the deprecated product-named aliases")
    func testDeprecatedProductNamedAliasesStayEqualToTheNeutralCases() {
        XCTAssertEqual(RightKitOverlayKind.heardRightPill, .pill)
        XCTAssertEqual(RightKitOverlayKind.cockpitNotch, .notch)
        let kind: RightKitOverlayKind = .notch
        switch kind {
        case .heardRightPill: XCTFail("a notch is not a pill")
        case .cockpitNotch: break
        default: XCTFail("unexpected kind")
        }
    }

    func testExplicitHitboxKeepsOffsetThroughPlacement() {
        var model = RightKitOverlayModel(kind: .pill, frame: RightKitOverlayRect(x: 900, y: 900, width: 64, height: 10))
        model.hitbox = RightKitOverlayRect(x: 890, y: 890, width: 84, height: 30)
        model.apply(.recording)
        model.place(in: RightKitOverlayWorkArea(left: 0, top: 0, right: 800, bottom: 600))
        XCTAssertEqual(model.hitbox, RightKitOverlayRect(x: 726, y: 580, width: 84, height: 30))
    }

    func testHitRegionsTakeTheMouseOnlyInsideAnyRect() {
        let regions = RightKitOverlayHitRegions([
            CGRect(x: 0, y: 0, width: 64, height: 10),
            CGRect(x: 100, y: 50, width: 40, height: 40),
        ])
        XCTAssertEqual(regions.ignoresMouseEvents(at: CGPoint(x: 10, y: 5)), false)
        XCTAssertEqual(regions.ignoresMouseEvents(at: CGPoint(x: 120, y: 70)), false, "second rect also takes the mouse")
        XCTAssertEqual(regions.ignoresMouseEvents(at: CGPoint(x: 80, y: 30)), true, "gap between rects is click-through")
        XCTAssertEqual(regions.ignoresMouseEvents(at: CGPoint(x: -1, y: 5)), true)
        XCTAssertEqual(regions.ignoresMouseEvents(at: CGPoint(x: Double.nan, y: 5)), true, "non-finite pointer never takes the mouse")
    }

    func testEmptyHitRegionsLeaveClickThroughAlone() {
        let regions = RightKitOverlayHitRegions()
        XCTAssertFalse(regions.isActive)
        XCTAssertNil(regions.ignoresMouseEvents(at: CGPoint(x: 1, y: 1)))
    }

#if os(macOS)
    @MainActor
    func testPanelDefaultsToInteractiveAndSetClickThroughToggles() {
        let panel = RightKitOverlayPanel(contentRect: NSRect(x: 10, y: 10, width: 64, height: 10))
        XCTAssertFalse(panel.ignoresMouseEvents)
        XCTAssertTrue(panel.hitRegions.isEmpty)
        panel.setClickThrough(true)
        XCTAssertTrue(panel.ignoresMouseEvents)
        panel.setClickThrough(false)
        XCTAssertFalse(panel.ignoresMouseEvents)
    }

    @MainActor
    func testUpdateClickThroughTogglesPerRegion() {
        let origin = CGPoint(x: 400, y: 300)
        let panel = RightKitOverlayPanel(contentRect: NSRect(origin: origin, size: CGSize(width: 200, height: 100)))
        // Window coordinates: a 60x20 card at the lower left and a 30x30 card at the right.
        panel.hitRegions = [CGRect(x: 0, y: 0, width: 60, height: 20), CGRect(x: 150, y: 50, width: 30, height: 30)]
        func screen(_ x: CGFloat, _ y: CGFloat) -> CGPoint { panel.convertPoint(toScreen: CGPoint(x: x, y: y)) }

        XCTAssertFalse(panel.updateClickThrough(forMouseAt: screen(10, 10)))
        XCTAssertFalse(panel.ignoresMouseEvents)
        XCTAssertTrue(panel.updateClickThrough(forMouseAt: screen(100, 40)), "transparent gap passes the mouse through")
        XCTAssertTrue(panel.ignoresMouseEvents)
        XCTAssertFalse(panel.updateClickThrough(forMouseAt: screen(160, 60)))
        XCTAssertFalse(panel.ignoresMouseEvents)
        XCTAssertTrue(panel.updateClickThrough(forMouseAt: CGPoint(x: -5000, y: -5000)))
    }

    @MainActor
    func testUpdateClickThroughWithoutRegionsKeepsExplicitState() {
        let panel = RightKitOverlayPanel(contentRect: NSRect(x: 10, y: 10, width: 64, height: 10))
        panel.setClickThrough(true)
        XCTAssertTrue(panel.updateClickThrough(forMouseAt: panel.convertPoint(toScreen: CGPoint(x: 5, y: 5))))
        panel.setClickThrough(false)
        XCTAssertFalse(panel.updateClickThrough(forMouseAt: CGPoint(x: -5000, y: -5000)))
    }

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
        let panel = RightKitOverlayPanel(contentRect: NSRect(x: 0, y: 0, width: 360, height: 64), kind: .notch)
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

    #if os(macOS)
    /// A host subclass compiles against the panel and inherits its configuration-owned key behaviour.
    @MainActor
    func testPanelCanBeSubclassedAndKeepsConfigurationOwnedKeyBehaviour() {
        final class HostPanel: RightKitOverlayPanel {
            var mouseDowns = 0
            override func mouseDown(with event: NSEvent) { mouseDowns += 1; super.mouseDown(with: event) }
        }
        let panel = HostPanel(contentRect: NSRect(x: 0, y: 0, width: 10, height: 10), kind: .notch,
                              configuration: RightKitOverlayPanelConfiguration(canBecomeKey: true))
        XCTAssertTrue(panel.canBecomeKey)
        XCTAssertFalse(panel.canBecomeMain)
        XCTAssertEqual(panel.overlayKind, .notch)
        XCTAssertEqual(panel.mouseDowns, 0)
    }
    #endif
}

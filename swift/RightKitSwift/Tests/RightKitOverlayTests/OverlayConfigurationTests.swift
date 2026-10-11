import XCTest
@testable import RightKitOverlay
#if os(macOS)
import AppKit
import CoreGraphics
#endif

// Isolation: every window here is 64 x 10 or smaller and sits left of the whole virtual screen.
// Nothing posts or synthesizes pointer or keyboard input into the session, takes a screenshot, or
// raises a window on a visible monitor. `isKeyWindow` is never asserted (unknown under XCTest).

final class OverlayHitRegionOriginTests: XCTestCase {
    func testLowerLeftIsTheDefaultAndKeepsTodaysMeaning() {
        let regions = RightKitOverlayHitRegions([CGRect(x: 0, y: 0, width: 20, height: 10)])
        XCTAssertEqual(regions.origin, .lowerLeft)
        XCTAssertTrue(regions.contains(windowPoint: CGPoint(x: 5, y: 5), windowHeight: 100))
        XCTAssertFalse(regions.contains(windowPoint: CGPoint(x: 5, y: 95), windowHeight: 100))
    }

    func testTopLeftAndLowerLeftAgreeOnTheSameRectanglesOnceConverted() {
        // Window 100 high. The rectangle hugs the TOP edge: y 0..10 from the top-left origin.
        let rect = CGRect(x: 10, y: 0, width: 20, height: 10)
        let top = RightKitOverlayHitRegions([rect], origin: .topLeft)
        // The same area in lower-left space is y 90..100.
        let lower = RightKitOverlayHitRegions([CGRect(x: 10, y: 90, width: 20, height: 10)], origin: .lowerLeft)
        for (x, y) in [(15.0, 95.0), (15.0, 5.0), (0.0, 95.0), (29.0, 91.0), (31.0, 95.0), (15.0, 89.0)] {
            let p = CGPoint(x: x, y: y)
            XCTAssertEqual(top.contains(windowPoint: p, windowHeight: 100),
                           lower.contains(windowPoint: p, windowHeight: 100), "point \(p)")
        }
        XCTAssertTrue(top.contains(windowPoint: CGPoint(x: 15, y: 95), windowHeight: 100))
        XCTAssertFalse(top.contains(windowPoint: CGPoint(x: 15, y: 5), windowHeight: 100))
    }

    func testNonFinitePointsAndHeightsNeverTakeTheMouse() {
        let top = RightKitOverlayHitRegions([CGRect(x: 0, y: 0, width: 50, height: 50)], origin: .topLeft)
        XCTAssertFalse(top.contains(windowPoint: CGPoint(x: CGFloat.nan, y: 5), windowHeight: 100))
        XCTAssertFalse(top.contains(windowPoint: CGPoint(x: 5, y: 5), windowHeight: CGFloat.infinity))
        XCTAssertEqual(top.ignoresMouseEvents(atWindowPoint: CGPoint(x: 5, y: 95), windowHeight: 100), false)
        XCTAssertEqual(top.ignoresMouseEvents(atWindowPoint: CGPoint(x: 5, y: 5), windowHeight: 100), true)
        XCTAssertNil(RightKitOverlayHitRegions(origin: .topLeft)
            .ignoresMouseEvents(atWindowPoint: .zero, windowHeight: 100))
    }
}

final class OverlayDisplaysAndFullScreenTests: XCTestCase {
    func testDisplayPlanAddsRemovesAndFlagsDuplicates() {
        let plan = RightKitOverlayDisplays.plan(current: [1, 2, 3], desired: [2, 3, 4])
        XCTAssertEqual(plan.add, [4])
        XCTAssertEqual(plan.remove, [1])
        XCTAssertFalse(plan.hasDuplicates)

        let same = RightKitOverlayDisplays.plan(current: [1, 2], desired: [2, 1])
        XCTAssertTrue(same.add.isEmpty && same.remove.isEmpty && !same.hasDuplicates)

        let dup = RightKitOverlayDisplays.plan(current: [], desired: [7, 7, 8])
        XCTAssertEqual(dup.add, [7, 8])
        XCTAssertTrue(dup.hasDuplicates)

        let none = RightKitOverlayDisplays.plan(current: [5], desired: [])
        XCTAssertEqual(none.remove, [5])
        XCTAssertTrue(none.add.isEmpty)
    }

    // The geometry cases below are Pulse's (FullScreenAutoFoldTests / FullScreenDetector).
    private let screen = CGRect(x: 0, y: 0, width: 1728, height: 1117)
    private let pid: pid_t = 12345
    typealias W = (pid: pid_t, layer: Int, bounds: CGRect)

    func testFindsFullScreenWindowMatchingScreenBounds() {
        let windows: [W] = [
            (pid: 9999, layer: 0, bounds: CGRect(x: 50, y: 50, width: 800, height: 600)),
            (pid: pid, layer: 0, bounds: screen),
            (pid: pid, layer: 24, bounds: screen),
        ]
        XCTAssertTrue(RightKitOverlayFullScreen.isFullScreen(screenBounds: screen, frontmostPID: pid, windows: windows))
    }

    func testFindsFullScreenWindowOnNotchedDisplay() {
        let windows: [W] = [(pid: pid, layer: 0, bounds: CGRect(x: 0, y: 44, width: 1728, height: 1073))]
        XCTAssertTrue(RightKitOverlayFullScreen.isFullScreen(
            screenBounds: screen, frontmostPID: pid, windows: windows, safeAreaTopInset: 44))
    }

    func testRejectsWindowNotMatchingScreenBounds() {
        let windows: [W] = [(pid: pid, layer: 0, bounds: CGRect(x: 100, y: 100, width: 1200, height: 800))]
        XCTAssertFalse(RightKitOverlayFullScreen.isFullScreen(screenBounds: screen, frontmostPID: pid, windows: windows))
    }

    func testRejectsFullScreenWindowOfNonFrontmostApp() {
        let windows: [W] = [(pid: 67890, layer: 0, bounds: screen)]
        XCTAssertFalse(RightKitOverlayFullScreen.isFullScreen(screenBounds: screen, frontmostPID: pid, windows: windows))
    }

    func testIgnoresNonZeroLayersAndEmptyWindowLists() {
        XCTAssertFalse(RightKitOverlayFullScreen.isFullScreen(
            screenBounds: screen, frontmostPID: pid, windows: [(pid: pid, layer: 25, bounds: screen)]))
        XCTAssertFalse(RightKitOverlayFullScreen.isFullScreen(screenBounds: screen, frontmostPID: pid, windows: []))
    }

    func testToleranceIsFourPointsOnEachEdge() {
        func check(_ r: CGRect) -> Bool {
            RightKitOverlayFullScreen.isFullScreen(screenBounds: screen, frontmostPID: pid,
                                                   windows: [(pid: pid, layer: 0, bounds: r)])
        }
        XCTAssertTrue(check(CGRect(x: 4, y: 4, width: 1724, height: 1113)))
        XCTAssertFalse(check(CGRect(x: 5, y: 0, width: 1728, height: 1117)), "5 pt off in x fails")
        XCTAssertFalse(check(CGRect(x: 0, y: 0, width: 1722, height: 1117)), "6 pt narrow fails")
    }

    func testMenuBarWithoutNotchNeedsTheDefaultInsetToCount() {
        // Starts 30 pt down (menu bar), reaches the bottom: accepted via case 2 with the 40 pt floor.
        let windows: [W] = [(pid: pid, layer: 0, bounds: CGRect(x: 0, y: 30, width: 1728, height: 1087))]
        XCTAssertTrue(RightKitOverlayFullScreen.isFullScreen(screenBounds: screen, frontmostPID: pid, windows: windows))
        // Starts 200 pt down: a tall window under a toolbar, not full screen.
        let low: [W] = [(pid: pid, layer: 0, bounds: CGRect(x: 0, y: 200, width: 1728, height: 917))]
        XCTAssertFalse(RightKitOverlayFullScreen.isFullScreen(screenBounds: screen, frontmostPID: pid, windows: low))
    }

#if os(macOS)
    @MainActor
    func testCurrentDisplaysMatchTheAttachedScreens() {
        let displays = RightKitOverlayDisplays.current()
        XCTAssertEqual(displays.count, NSScreen.screens.count)
        for d in displays { XCTAssertGreaterThan(d.frame.width, 0) }
    }
#endif
}

#if os(macOS)
@MainActor
final class OverlayPanelConfigurationTests: XCTestCase {
    /// Left of every screen, 64 x 10, offset per process, as the existing fixture does.
    private func offscreenRect(width: CGFloat = 64, height: CGFloat = 10) -> NSRect {
        let minX = NSScreen.screens.map(\.frame.minX).min() ?? 0
        let offset = CGFloat(ProcessInfo.processInfo.processIdentifier % 1_024 + 1)
        return NSRect(x: minX - width - offset, y: 16, width: width, height: height)
    }

    private func onScreenListContains(_ windowNumber: Int) -> Bool {
        let infos = (CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]]) ?? []
        return infos.contains { ($0[kCGWindowNumber as String] as? NSNumber)?.intValue == windowNumber }
    }

    func testStandardReproducesTodaysValues() {
        let pill = RightKitOverlayPanelConfiguration.standard(kind: .pill)
        XCTAssertEqual(pill.level, .statusBar)
        XCTAssertEqual(pill.collectionBehavior, [.canJoinAllSpaces, .stationary])
        XCTAssertFalse(pill.canBecomeKey)
        XCTAssertTrue(pill.becomesKeyOnlyIfNeeded)
        XCTAssertFalse(pill.isFloatingPanel)
        XCTAssertFalse(pill.hasShadow)
        XCTAssertTrue(pill.isMovable)
        XCTAssertFalse(pill.isMovableByWindowBackground)
        XCTAssertEqual(pill.animationBehavior, .default)
        XCTAssertFalse(pill.startsClickThrough)
        XCTAssertEqual(pill.hitRegionOrigin, .lowerLeft)

        let notch = RightKitOverlayPanelConfiguration.standard(kind: .notch, level: .popUpMenu)
        XCTAssertEqual(notch.level, .popUpMenu)
        XCTAssertEqual(notch.collectionBehavior, [.canJoinAllSpaces, .stationary, .fullScreenAuxiliary])
    }

    func testPresets() {
        XCTAssertEqual(RightKitOverlayPanelConfiguration.stationaryAllSpaces, [.canJoinAllSpaces, .stationary])
        XCTAssertEqual(RightKitOverlayPanelConfiguration.stationaryFullScreenAuxiliary,
                       [.canJoinAllSpaces, .stationary, .fullScreenAuxiliary])
        XCTAssertEqual(RightKitOverlayPanelConfiguration.transientFullScreenAuxiliary,
                       [.canJoinAllSpaces, .fullScreenAuxiliary, .transient])
    }

    /// An untouched existing-initializer panel matches the same properties a fresh NSPanel would
    /// have had before this change: the delegation changed nothing observable.
    func testExistingInitializerMatchesStandardConfiguration() {
        for kind in [RightKitOverlayKind.pill, .notch] {
            let panel = RightKitOverlayPanel(contentRect: offscreenRect(), kind: kind, level: .floating)
            let standard = RightKitOverlayPanelConfiguration.standard(kind: kind, level: .floating)
            XCTAssertEqual(panel.level, standard.level)
            XCTAssertEqual(panel.collectionBehavior, standard.collectionBehavior)
            XCTAssertEqual(panel.hasShadow, standard.hasShadow)
            XCTAssertEqual(panel.isMovable, standard.isMovable)
            XCTAssertEqual(panel.isMovableByWindowBackground, standard.isMovableByWindowBackground)
            XCTAssertEqual(panel.isFloatingPanel, standard.isFloatingPanel)
            XCTAssertEqual(panel.becomesKeyOnlyIfNeeded, standard.becomesKeyOnlyIfNeeded)
            XCTAssertEqual(panel.animationBehavior, standard.animationBehavior)
            XCTAssertEqual(panel.ignoresMouseEvents, standard.startsClickThrough)
            XCTAssertEqual(panel.hitRegionOrigin, .lowerLeft)
            XCTAssertEqual(panel.canBecomeKey, standard.canBecomeKey)
            XCTAssertFalse(panel.canBecomeMain)
            XCTAssertTrue(panel.styleMask.contains(.nonactivatingPanel))
            XCTAssertFalse(panel.isOpaque)
        }
    }

    func testEveryConfigurationFieldIsApplied() {
        let config = RightKitOverlayPanelConfiguration(
            level: .popUpMenu,
            collectionBehavior: RightKitOverlayPanelConfiguration.transientFullScreenAuxiliary,
            canBecomeKey: true,
            becomesKeyOnlyIfNeeded: false,
            isFloatingPanel: true,
            hasShadow: true,
            isMovable: false,
            isMovableByWindowBackground: true,
            animationBehavior: .none,
            startsClickThrough: true,
            hitRegionOrigin: .topLeft)
        let panel = RightKitOverlayPanel(contentRect: offscreenRect(), kind: .pill, configuration: config)
        XCTAssertEqual(panel.level, .popUpMenu)
        XCTAssertEqual(panel.collectionBehavior, [.canJoinAllSpaces, .fullScreenAuxiliary, .transient])
        XCTAssertTrue(panel.canBecomeKey)
        XCTAssertFalse(panel.becomesKeyOnlyIfNeeded)
        XCTAssertTrue(panel.isFloatingPanel)
        XCTAssertTrue(panel.hasShadow)
        XCTAssertFalse(panel.isMovable)
        XCTAssertTrue(panel.isMovableByWindowBackground)
        XCTAssertEqual(panel.animationBehavior, .none)
        XCTAssertTrue(panel.ignoresMouseEvents)
        XCTAssertEqual(panel.hitRegionOrigin, .topLeft)
        XCTAssertEqual(panel.overlayKind, .pill)
        XCTAssertEqual(panel.configuration.level, .popUpMenu)
    }

    func testCanBecomeMainIsFalseInEveryConfiguration() {
        for key in [false, true] {
            for kind in [RightKitOverlayKind.pill, .notch] {
                var config = RightKitOverlayPanelConfiguration.standard(kind: kind)
                config.canBecomeKey = key
                config.isFloatingPanel = key
                let panel = RightKitOverlayPanel(contentRect: offscreenRect(), kind: kind, configuration: config)
                XCTAssertEqual(panel.canBecomeKey, key)
                XCTAssertFalse(panel.canBecomeMain)
            }
        }
    }

    func testHideLeavesTheOnScreenList() {
        let panel = RightKitOverlayPanel(contentRect: offscreenRect())
        defer { panel.orderOut(nil) }
        panel.contentView = NSView(frame: NSRect(origin: .zero, size: panel.frame.size))
        panel.showWithoutActivating()
        RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.1))
        XCTAssertTrue(onScreenListContains(panel.windowNumber), "precondition: ordered panel is listed")
        panel.hide()
        // WindowServer drops the entry asynchronously; wait up to 2 s.
        let deadline = Date(timeIntervalSinceNow: 2)
        while onScreenListContains(panel.windowNumber), Date() < deadline {
            RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.05))
        }
        XCTAssertFalse(onScreenListContains(panel.windowNumber))
    }

    func testShowAsKeyLeavesTheFrontmostApplicationAlone() {
        let before = NSWorkspace.shared.frontmostApplication?.processIdentifier
        var config = RightKitOverlayPanelConfiguration.standard(kind: .pill)
        config.canBecomeKey = true
        let panel = RightKitOverlayPanel(contentRect: offscreenRect(), kind: .pill, configuration: config)
        defer { panel.orderOut(nil) }
        panel.contentView = NSView(frame: NSRect(origin: .zero, size: panel.frame.size))
        panel.showAsKey()
        RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.1))
        XCTAssertEqual(NSWorkspace.shared.frontmostApplication?.processIdentifier, before)
        XCTAssertTrue(panel.isVisible)
    }

    // MARK: hit regions and shape

    func testPanelHonoursBothOriginsAgainstTheSameRectangles() {
        let rect = CGRect(x: 0, y: 0, width: 30, height: 10)   // 64x40 panel: hugs one edge
        let lower = RightKitOverlayPanel(contentRect: offscreenRect(height: 40))
        lower.hitRegions = [rect]
        let top = RightKitOverlayPanel(contentRect: offscreenRect(height: 40),
                                       configuration: {
            var c = RightKitOverlayPanelConfiguration.standard(kind: .pill)
            c.hitRegionOrigin = .topLeft
            return c
        }())
        top.hitRegions = [rect]
        let nearBottom = CGPoint(x: 5, y: 5), nearTop = CGPoint(x: 5, y: 35)
        XCTAssertTrue(lower.takesMouse(atWindowPoint: nearBottom))
        XCTAssertFalse(lower.takesMouse(atWindowPoint: nearTop))
        XCTAssertFalse(top.takesMouse(atWindowPoint: nearBottom))
        XCTAssertTrue(top.takesMouse(atWindowPoint: nearTop))
        // updateClickThrough goes through the same decision.
        XCTAssertFalse(top.updateClickThrough(forMouseAt: top.convertPoint(toScreen: nearTop)))
        XCTAssertTrue(top.updateClickThrough(forMouseAt: top.convertPoint(toScreen: nearBottom)))
    }

    func testHitShapeReplacesRegionsAndFlipsWithTheShape() {
        let panel = RightKitOverlayPanel(contentRect: offscreenRect(height: 40))
        panel.hitRegions = [CGRect(x: 0, y: 0, width: 64, height: 40)]
        XCTAssertTrue(panel.takesMouse(atWindowPoint: CGPoint(x: 50, y: 20)))

        var open = false
        panel.hitShape = { _ in open }
        XCTAssertFalse(panel.takesMouse(atWindowPoint: CGPoint(x: 50, y: 20)), "shape wins over a covering region")
        XCTAssertTrue(panel.updateClickThrough(forMouseAt: panel.convertPoint(toScreen: CGPoint(x: 50, y: 20))))
        open = true
        XCTAssertTrue(panel.takesMouse(atWindowPoint: CGPoint(x: 50, y: 20)), "the same point flips with the shape")
        XCTAssertFalse(panel.updateClickThrough(forMouseAt: panel.convertPoint(toScreen: CGPoint(x: 50, y: 20))))

        panel.hitShape = nil
        XCTAssertTrue(panel.takesMouse(atWindowPoint: CGPoint(x: 50, y: 20)), "regions answer again")
    }

    func testHitShapeReceivesPointsInTheRegionSpace() {
        let config: RightKitOverlayPanelConfiguration = {
            var c = RightKitOverlayPanelConfiguration.standard(kind: .pill)
            c.hitRegionOrigin = .topLeft
            return c
        }()
        let panel = RightKitOverlayPanel(contentRect: offscreenRect(height: 40), kind: .pill, configuration: config)
        var seen: CGPoint?
        panel.hitShape = { seen = $0; return true }
        _ = panel.takesMouse(atWindowPoint: CGPoint(x: 7, y: 35))
        XCTAssertEqual(seen, CGPoint(x: 7, y: 5))
        panel.hitRegionOrigin = .lowerLeft
        _ = panel.takesMouse(atWindowPoint: CGPoint(x: 7, y: 35))
        XCTAssertEqual(seen, CGPoint(x: 7, y: 35))
    }

    func testNothingSetMeansNothingRestricted() {
        let panel = RightKitOverlayPanel(contentRect: offscreenRect())
        XCTAssertTrue(panel.takesMouse(atWindowPoint: CGPoint(x: 1, y: 1)))
        panel.setClickThrough(true)
        XCTAssertTrue(panel.updateClickThrough(forMouseAt: CGPoint(x: -5000, y: -5000)), "explicit state is kept")
        panel.hitShape = { _ in true }
        XCTAssertFalse(panel.takesMouse(atWindowPoint: CGPoint(x: CGFloat.nan, y: 1)))
    }

    func testContentViewNeverReturnsItselfAndFollowsTheHitArea() {
        let panel = RightKitOverlayPanel(contentRect: offscreenRect(height: 40))
        let size = panel.contentRect(forFrameRect: panel.frame).size
        let hosted = NSView(frame: .zero)
        let child = NSView(frame: NSRect(x: 0, y: 0, width: 20, height: 40))
        hosted.addSubview(child)
        let container = RightKitOverlayContentView(frame: NSRect(origin: .zero, size: size), hosting: hosted)
        panel.contentView = container

        XCTAssertTrue(container.content === hosted)
        XCTAssertEqual(hosted.frame, container.bounds, "content fills the container")
        XCTAssertEqual(hosted.autoresizingMask, [.width, .height])

        // No hit area configured: subviews are reachable, never the container.
        XCTAssertTrue(container.hitTest(CGPoint(x: 5, y: 5)) === child)
        XCTAssertTrue(container.hitTest(CGPoint(x: 50, y: 5)) === hosted)
        XCTAssertNil(container.hitTest(CGPoint(x: -5, y: 5)), "outside the bounds")
        XCTAssertNil(container.hitTest(CGPoint(x: 200, y: 5)))

        // With a region over the child only.
        panel.hitRegions = [CGRect(x: 0, y: 0, width: 20, height: 40)]
        XCTAssertTrue(container.hitTest(CGPoint(x: 5, y: 5)) === child)
        XCTAssertNil(container.hitTest(CGPoint(x: 50, y: 5)), "outside the regions: nil, not the container")

        // A changing shape flips the answer for the same point.
        var open = false
        panel.hitShape = { _ in open }
        XCTAssertNil(container.hitTest(CGPoint(x: 5, y: 5)))
        open = true
        XCTAssertTrue(container.hitTest(CGPoint(x: 5, y: 5)) === child)

        // Top-left origin: the child's area (window-lower-left) is y 0..40 here, so use a region at the top.
        panel.hitShape = nil
        panel.hitRegionOrigin = .topLeft
        panel.hitRegions = [CGRect(x: 0, y: 0, width: 64, height: 10)]   // top 10 pt
        XCTAssertTrue(container.hitTest(CGPoint(x: 5, y: 35)) === child)
        XCTAssertNil(container.hitTest(CGPoint(x: 5, y: 5)))
    }

    func testContentViewWithoutAWindowStillNeverReturnsItself() {
        let hosted = NSView(frame: .zero)
        let container = RightKitOverlayContentView(frame: NSRect(x: 0, y: 0, width: 64, height: 10), hosting: hosted)
        XCTAssertTrue(container.hitTest(CGPoint(x: 5, y: 5)) === hosted)
        XCTAssertFalse(container.hitTest(CGPoint(x: 5, y: 5)) === container)
    }

    // MARK: events

    private final class CountingView: NSView {
        var mouseDowns = 0
        override func mouseDown(with event: NSEvent) { mouseDowns += 1 }
    }

    private func constructedMouseDown(in panel: NSWindow) -> NSEvent? {
        NSEvent.mouseEvent(with: .leftMouseDown, location: CGPoint(x: 5, y: 5), modifierFlags: [],
                           timestamp: 0, windowNumber: panel.windowNumber, context: nil,
                           eventNumber: 1, clickCount: 1, pressure: 1)
    }

    func testEventFilterSeesEveryEventFirstAndCanConsumeIt() throws {
        let panel = RightKitOverlayPanel(contentRect: offscreenRect())
        let view = CountingView(frame: NSRect(origin: .zero, size: panel.frame.size))
        panel.contentView = view
        defer { panel.orderOut(nil) }
        panel.showWithoutActivating()   // off-screen; a window that was never ordered gets no mouse events
        let event = try XCTUnwrap(constructedMouseDown(in: panel))

        var filtered = 0
        var consume = true
        panel.eventFilter = { _ in filtered += 1; return consume }
        panel.sendEvent(event)
        XCTAssertEqual(filtered, 1)
        XCTAssertEqual(view.mouseDowns, 0, "a consumed event never reaches the window")

        consume = false
        panel.sendEvent(event)
        XCTAssertEqual(filtered, 2)
        XCTAssertEqual(view.mouseDowns, 1, "an unconsumed event goes on to the window")

        panel.eventFilter = nil
        panel.sendEvent(event)
        XCTAssertEqual(view.mouseDowns, 2)
    }

    func testOnResignKeyFiresOnResignKey() {
        let panel = RightKitOverlayPanel(contentRect: offscreenRect())
        var fired = 0
        panel.onResignKey = { fired += 1 }
        panel.resignKey()
        XCTAssertEqual(fired, 1)
        panel.onResignKey = nil
        panel.resignKey()
        XCTAssertEqual(fired, 1)
    }

    // MARK: tracker

    func testTrackerStartAndStopAreIdempotent() {
        let panel = RightKitOverlayPanel(contentRect: offscreenRect())
        let tracker = RightKitOverlayPointerTracker(panel: panel, pollInterval: 0.05)
        tracker.locationProvider = { CGPoint(x: -5000, y: -5000) }
        XCTAssertFalse(tracker.isRunning)

        tracker.start()
        XCTAssertTrue(tracker.isRunning)
        XCTAssertTrue(tracker.hasLocalMonitor)
        XCTAssertTrue(tracker.hasTimer)
        tracker.start()   // second start changes nothing
        XCTAssertTrue(tracker.isRunning)

        tracker.stop()
        XCTAssertFalse(tracker.isRunning)
        XCTAssertFalse(tracker.hasGlobalMonitor)
        XCTAssertFalse(tracker.hasLocalMonitor)
        XCTAssertFalse(tracker.hasTimer)
        tracker.stop()    // second stop is harmless
        XCTAssertFalse(tracker.isRunning)

        tracker.start()
        XCTAssertTrue(tracker.isRunning, "restartable")
        tracker.stop()
    }

    func testTrackerTickWithInjectedLocationTogglesClickThrough() {
        let panel = RightKitOverlayPanel(contentRect: offscreenRect(height: 40))
        panel.hitRegions = [CGRect(x: 0, y: 0, width: 20, height: 40)]
        var location = panel.convertPoint(toScreen: CGPoint(x: 5, y: 5))
        var reported: [CGPoint] = []
        let tracker = RightKitOverlayPointerTracker(panel: panel, pollInterval: 0.05) { reported.append($0) }
        tracker.locationProvider = { location }

        tracker.tick()
        XCTAssertFalse(panel.ignoresMouseEvents, "inside a region: takes the mouse")
        location = panel.convertPoint(toScreen: CGPoint(x: 50, y: 5))
        tracker.tick()
        XCTAssertTrue(panel.ignoresMouseEvents, "outside every region: click-through")
        XCTAssertEqual(reported.count, 2)
        XCTAssertEqual(reported.last, location)
    }

    func testTrackerPollsWhileRunningAndStopsPolling() {
        let panel = RightKitOverlayPanel(contentRect: offscreenRect())
        var count = 0
        let tracker = RightKitOverlayPointerTracker(panel: panel, pollInterval: 0.02) { _ in count += 1 }
        tracker.locationProvider = { CGPoint(x: -5000, y: -5000) }
        tracker.start()
        RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.3))
        tracker.stop()
        let atStop = count
        XCTAssertGreaterThanOrEqual(atStop, 3, "the poll ticks while running")
        RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.15))
        XCTAssertEqual(count, atStop, "no tick after stop")
    }

    func testTrackerStopsItselfWhenThePanelIsGone() {
        let tracker: RightKitOverlayPointerTracker = autoreleasepool {
            let panel = RightKitOverlayPanel(contentRect: offscreenRect())
            let tracker = RightKitOverlayPointerTracker(panel: panel, pollInterval: 0.05)
            tracker.locationProvider = { .zero }
            tracker.start()
            return tracker
        }
        tracker.tick()
        XCTAssertFalse(tracker.isRunning)
    }

    func testDroppingARunningTrackerLeavesNothingBehind() {
        let panel = RightKitOverlayPanel(contentRect: offscreenRect())
        var tracker: RightKitOverlayPointerTracker? = RightKitOverlayPointerTracker(panel: panel, pollInterval: 0.02)
        weak let weakTracker = tracker
        tracker?.start()
        tracker = nil
        RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.1))
        XCTAssertNil(weakTracker)
    }
}
#endif

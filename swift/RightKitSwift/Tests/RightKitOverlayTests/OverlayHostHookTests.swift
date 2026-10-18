import XCTest
@testable import RightKitOverlay
#if os(macOS)
import AppKit
import CoreGraphics
#endif

// Isolation (as OverlayConfigurationTests): every window is 64 x 10 or smaller and sits left of the
// whole virtual screen. Nothing posts input into the session, takes a screenshot, or raises a window
// on a visible monitor. Events are constructed and handed straight to the panel, never posted.

final class OverlayOrderedPlanTests: XCTestCase {
    func testAdditionsKeepTheAskedOrderWithoutRepeatsAndRemovalsAreSorted() {
        let plan = RightKitOverlayDisplays.orderedPlan(current: [9, 3, 7, 1], desired: [8, 2, 8, 3, 5, 2])
        XCTAssertEqual(plan.add, [8, 2, 5], "desired order, first occurrence, de-duplicated")
        XCTAssertEqual(plan.remove, [1, 7, 9], "ascending by display id")
        XCTAssertTrue(plan.hasDuplicates)
    }

    func testAcceptsAnyCollectionForCurrentAndCountsRepeatsOnce() {
        let fromArray = RightKitOverlayDisplays.orderedPlan(current: [4, 4, 2], desired: [2])
        XCTAssertEqual(fromArray.add, [])
        XCTAssertEqual(fromArray.remove, [4])
        XCTAssertFalse(fromArray.hasDuplicates)
        let empty = RightKitOverlayDisplays.orderedPlan(current: Set<CGDirectDisplayID>(), desired: [])
        XCTAssertTrue(empty.add.isEmpty && empty.remove.isEmpty && !empty.hasDuplicates)
    }

    func testAgreesWithTheSetPlanAndTheSetPlanIsUnchanged() {
        for (current, desired) in [([1, 2, 3], [2, 3, 4]), ([], [7, 7, 8]), ([5], []), ([1, 2], [2, 1])] as [([CGDirectDisplayID], [CGDirectDisplayID])] {
            let sets = RightKitOverlayDisplays.plan(current: Set(current), desired: desired)
            let ordered = RightKitOverlayDisplays.orderedPlan(current: current, desired: desired)
            XCTAssertEqual(Set(ordered.add), sets.add)
            XCTAssertEqual(Set(ordered.remove), sets.remove)
            XCTAssertEqual(ordered.hasDuplicates, sets.hasDuplicates)
            XCTAssertEqual(ordered.add.count, sets.add.count)
            XCTAssertEqual(ordered.remove.count, sets.remove.count)
        }
        let plan = RightKitOverlayDisplays.plan(current: [1, 2, 3], desired: [2, 3, 4])
        XCTAssertEqual(plan.add, [4])   // still a Set<CGDirectDisplayID>
        XCTAssertEqual(plan.remove, [1])
    }
}

final class OverlayFullScreenRulesTests: XCTestCase {
    func testIgnoreSetsAreSeparateForGeometryAndVisibleFrame() {
        typealias F = RightKitOverlayFullScreen
        XCTAssertTrue(F.rulesToApply(frontmostBundleID: "a").geometry && F.rulesToApply(frontmostBundleID: "a").visibleFrame)
        XCTAssertTrue(F.rulesToApply(frontmostBundleID: nil, ignoringBundleIDs: ["a"]).geometry)
        let both = F.rulesToApply(frontmostBundleID: "a", ignoringBundleIDs: ["a"], geometryIgnoring: [], visibleFrameIgnoring: [])
        XCTAssertFalse(both.geometry || both.visibleFrame, "ignoringBundleIDs skips both rules")
        // Pulse's Finder case: skip only the visible-frame fallback, still apply geometry.
        let finder = F.rulesToApply(frontmostBundleID: "com.apple.finder", visibleFrameIgnoring: ["com.apple.finder"])
        XCTAssertTrue(finder.geometry)
        XCTAssertFalse(finder.visibleFrame)
        let geometryOnlySkipped = F.rulesToApply(frontmostBundleID: "x", geometryIgnoring: ["x"])
        XCTAssertFalse(geometryOnlySkipped.geometry)
        XCTAssertTrue(geometryOnlySkipped.visibleFrame)
        let other = F.rulesToApply(frontmostBundleID: "y", geometryIgnoring: ["x"], visibleFrameIgnoring: ["x"])
        XCTAssertTrue(other.geometry && other.visibleFrame, "only the named apps are skipped")
    }

    func testVisibleFrameFallbackToleratesOnePoint() {
        let frame = CGRect(x: 0, y: 0, width: 1728, height: 1117)
        XCTAssertTrue(RightKitOverlayFullScreen.visibleFrameCoversScreen(frame: frame, visibleFrame: frame))
        XCTAssertTrue(RightKitOverlayFullScreen.visibleFrameCoversScreen(frame: frame, visibleFrame: CGRect(x: 0, y: 0, width: 1727, height: 1116)))
        XCTAssertFalse(RightKitOverlayFullScreen.visibleFrameCoversScreen(frame: frame, visibleFrame: CGRect(x: 0, y: 0, width: 1728, height: 1095)))
    }
}

#if os(macOS)
@MainActor
final class OverlayHostHookTests: XCTestCase {
    private func offscreenRect(width: CGFloat = 64, height: CGFloat = 10) -> NSRect {
        let minX = NSScreen.screens.map(\.frame.minX).min() ?? 0
        let offset = CGFloat(ProcessInfo.processInfo.processIdentifier % 1_024 + 1)
        return NSRect(x: minX - width - offset, y: 16, width: width, height: height)
    }

    private final class HandlingView: NSView {
        var mouseDowns = 0
        override func mouseDown(with event: NSEvent) { mouseDowns += 1 }
    }

    private func event(_ type: NSEvent.EventType, in panel: NSWindow) -> NSEvent? {
        NSEvent.mouseEvent(with: type, location: CGPoint(x: 5, y: 5), modifierFlags: [], timestamp: 0,
                           windowNumber: panel.windowNumber, context: nil, eventNumber: 1, clickCount: 1, pressure: 1)
    }

    // MARK: unhandled mouse events

    func testHookSeesAPressNoViewTookAndNotOneAViewTook() throws {
        let panel = RightKitOverlayPanel(contentRect: offscreenRect())
        defer { panel.orderOut(nil) }
        var seen: [NSEvent.EventType] = []
        panel.unhandledMouseEvent = { seen.append($0.type); return true }

        // A view with no mouseDown: the press travels the responder chain to the window.
        panel.contentView = NSView(frame: NSRect(origin: .zero, size: panel.frame.size))
        panel.showWithoutActivating()
        panel.sendEvent(try XCTUnwrap(event(.leftMouseDown, in: panel)))
        XCTAssertEqual(seen, [.leftMouseDown])

        // A view that takes the press: the hook is never reached.
        let taker = HandlingView(frame: NSRect(origin: .zero, size: panel.frame.size))
        panel.contentView = taker
        panel.sendEvent(try XCTUnwrap(event(.leftMouseDown, in: panel)))
        XCTAssertEqual(taker.mouseDowns, 1)
        XCTAssertEqual(seen, [.leftMouseDown], "a view took it, so the hook stays silent")
    }

    func testHookIsCalledForDragUpAndRightMouseDownAndCanDeclineAllOfThem() throws {
        let panel = RightKitOverlayPanel(contentRect: offscreenRect())
        var seen: [NSEvent.EventType] = []
        var handle = true
        panel.unhandledMouseEvent = { seen.append($0.type); return handle }
        let types: [NSEvent.EventType] = [.leftMouseDown, .leftMouseDragged, .leftMouseUp, .rightMouseDown]
        let events = try types.map { try XCTUnwrap(event($0, in: panel)) }

        panel.mouseDown(with: events[0]); panel.mouseDragged(with: events[1])
        panel.mouseUp(with: events[2]); panel.rightMouseDown(with: events[3])
        XCTAssertEqual(seen, types)

        handle = false   // declining lets the event continue up the chain; nothing crashes or loops
        panel.mouseDown(with: events[0]); panel.mouseDragged(with: events[1])
        panel.mouseUp(with: events[2]); panel.rightMouseDown(with: events[3])
        XCTAssertEqual(seen.count, 8)

        panel.unhandledMouseEvent = nil   // no hook: the same calls are harmless
        panel.mouseDown(with: events[0]); panel.rightMouseDown(with: events[3])
        XCTAssertEqual(seen.count, 8)
    }

    func testEventFilterStillRunsFirstAndHookDoesNotChangeKeyOrMainControl() throws {
        let panel = RightKitOverlayPanel(contentRect: offscreenRect())
        defer { panel.orderOut(nil) }
        panel.contentView = NSView(frame: NSRect(origin: .zero, size: panel.frame.size))
        panel.showWithoutActivating()
        var order: [String] = []
        panel.eventFilter = { _ in order.append("filter"); return false }
        panel.unhandledMouseEvent = { _ in order.append("hook"); return true }
        panel.sendEvent(try XCTUnwrap(event(.leftMouseDown, in: panel)))
        XCTAssertEqual(order, ["filter", "hook"])

        panel.eventFilter = { _ in order.append("filter"); return true }   // consumed: the hook never runs
        panel.sendEvent(try XCTUnwrap(event(.leftMouseDown, in: panel)))
        XCTAssertEqual(order, ["filter", "hook", "filter"])

        XCTAssertFalse(panel.canBecomeKey, "configuration seeds canBecomeKey")
        XCTAssertFalse(panel.canBecomeMain)
        var config = RightKitOverlayPanelConfiguration.standard(kind: .pill)
        config.canBecomeKey = true
        let keyable = RightKitOverlayPanel(contentRect: offscreenRect(), kind: .pill, configuration: config)
        keyable.unhandledMouseEvent = { _ in true }
        XCTAssertTrue(keyable.canBecomeKey)
        XCTAssertFalse(keyable.canBecomeMain)
    }

    // MARK: hidesOnDeactivate

    func testHidesOnDeactivateDefaultsToTheCurrentValueAndIsApplied() {
        XCTAssertFalse(RightKitOverlayPanelConfiguration().hidesOnDeactivate)
        for kind in [RightKitOverlayKind.pill, .notch] {
            XCTAssertFalse(RightKitOverlayPanelConfiguration.standard(kind: kind).hidesOnDeactivate)
            XCTAssertFalse(RightKitOverlayPanel(contentRect: offscreenRect(), kind: kind, level: .floating).hidesOnDeactivate)
        }
        var config = RightKitOverlayPanelConfiguration.standard(kind: .pill)
        config.hidesOnDeactivate = true
        XCTAssertTrue(RightKitOverlayPanel(contentRect: offscreenRect(), kind: .pill, configuration: config).hidesOnDeactivate)
        XCTAssertTrue(RightKitOverlayPanelConfiguration(hidesOnDeactivate: true).hidesOnDeactivate)
    }

    // MARK: tracker without a panel

    func testPanelessTrackerReportsThePointerAndDoesNotStopItself() {
        var reported: [CGPoint] = []
        let tracker = RightKitOverlayPointerTracker(pollInterval: 0.05) { reported.append($0) }
        var location = CGPoint(x: -5000, y: -5000)
        tracker.locationProvider = { location }
        XCTAssertFalse(tracker.isRunning)

        tracker.start()
        XCTAssertTrue(tracker.isRunning)
        XCTAssertEqual(reported, [location], "start evaluates once")
        location = CGPoint(x: -4000, y: -4000)
        tracker.tick()
        XCTAssertEqual(reported.last, location)
        XCTAssertTrue(tracker.isRunning, "no panel to lose, so it keeps running")
        XCTAssertTrue(tracker.hasTimer && tracker.hasLocalMonitor)

        tracker.stop()
        XCTAssertFalse(tracker.isRunning)
        tracker.stop()
    }

    func testPanelTrackerStillStopsItselfWhenItsPanelIsGone() {
        var tracker: RightKitOverlayPointerTracker?
        autoreleasepool {
            let panel = RightKitOverlayPanel(contentRect: offscreenRect())
            tracker = RightKitOverlayPointerTracker(panel: panel, pollInterval: 0.05)
            tracker?.locationProvider = { CGPoint(x: -5000, y: -5000) }
            tracker?.start()
            XCTAssertTrue(tracker?.isRunning == true)
        }
        tracker?.tick()
        XCTAssertEqual(tracker?.isRunning, false)
    }

    // MARK: full screen with a nil screen

    func testNilScreenAndIgnoreSetsAreDeterministicWhenTheFrontmostAppIsIgnored() throws {
        guard let id = NSWorkspace.shared.frontmostApplication?.bundleIdentifier else { throw XCTSkip("no frontmost app bundle id") }
        let screen = NSScreen.main
        XCTAssertFalse(RightKitOverlayFullScreen.frontmostAppIsFullScreen(
            on: nil, ignoringBundleIDs: [id], geometryIgnoring: [], visibleFrameIgnoring: []), "ignored app: both rules skipped")
        XCTAssertFalse(RightKitOverlayFullScreen.frontmostAppIsFullScreen(
            on: nil, geometryIgnoring: [id], visibleFrameIgnoring: [id]))
        if let screen {
            // With geometry skipped, only the visible-frame fallback decides, on the main screen.
            XCTAssertEqual(
                RightKitOverlayFullScreen.frontmostAppIsFullScreen(on: nil, geometryIgnoring: [id], visibleFrameIgnoring: []),
                RightKitOverlayFullScreen.visibleFrameCoversScreen(frame: screen.frame, visibleFrame: screen.visibleFrame))
            // With the fallback skipped and geometry skipped, nothing can match.
            XCTAssertFalse(RightKitOverlayFullScreen.frontmostAppIsFullScreen(on: screen, geometryIgnoring: [id], visibleFrameIgnoring: [id]))
            // The existing signature still compiles and keeps its meaning.
            XCTAssertFalse(RightKitOverlayFullScreen.frontmostAppIsFullScreen(on: screen, ignoringBundleIDs: [id]))
        }
    }
}
#endif

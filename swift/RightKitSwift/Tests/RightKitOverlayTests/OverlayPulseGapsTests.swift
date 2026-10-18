import XCTest
@testable import RightKitOverlay
#if os(macOS)
import AppKit
import CoreGraphics

/// Issues 361 (key eligibility, subclassing), 521 (runtime key permission), 522 (styleMask),
/// 523 (full-screen context/evaluate split).
final class OverlayPulseGapsTests: XCTestCase {
    private func rect() -> NSRect { NSRect(x: -20000, y: -20000, width: 64, height: 10) }

    // MARK: 361 / 521 runtime key permission

    @MainActor
    func testAllowsKeyIsSeededFromConfigurationAndMutable() {
        let panel = RightKitOverlayPanel(contentRect: rect())
        XCTAssertFalse(panel.allowsKey)
        XCTAssertFalse(panel.canBecomeKey)
        panel.allowsKey = true
        XCTAssertTrue(panel.canBecomeKey)
        panel.allowsKey = false
        XCTAssertFalse(panel.canBecomeKey)

        var config = RightKitOverlayPanelConfiguration.standard(kind: .pill)
        config.canBecomeKey = true
        let keyable = RightKitOverlayPanel(contentRect: rect(), kind: .pill, configuration: config)
        XCTAssertTrue(keyable.allowsKey)
        keyable.allowsKey = false
        XCTAssertFalse(keyable.canBecomeKey)
        XCTAssertFalse(keyable.canBecomeMain, "never main, whatever allowsKey says")
    }

    @MainActor
    func testSubclassCanOverrideCanBecomeKey() {
        final class AlwaysKey: RightKitOverlayPanel {
            override var canBecomeKey: Bool { true }
        }
        final class FlagAndSuper: RightKitOverlayPanel {
            var extra = false
            override var canBecomeKey: Bool { super.canBecomeKey || extra }
        }
        XCTAssertTrue(AlwaysKey(contentRect: rect()).canBecomeKey)
        let panel = FlagAndSuper(contentRect: rect())
        XCTAssertFalse(panel.canBecomeKey)
        panel.extra = true
        XCTAssertTrue(panel.canBecomeKey)
    }

    // MARK: 522 styleMask

    @MainActor
    func testStyleMaskDefaultsToNonActivating() {
        XCTAssertEqual(RightKitOverlayPanelConfiguration().styleMask, [.borderless, .nonactivatingPanel])
        let panel = RightKitOverlayPanel(contentRect: rect())
        XCTAssertTrue(panel.styleMask.contains(.nonactivatingPanel))
    }

    @MainActor
    func testStyleMaskCanDropNonActivating() {
        let config = RightKitOverlayPanelConfiguration(canBecomeKey: true, styleMask: [.borderless])
        let panel = RightKitOverlayPanel(contentRect: rect(), kind: .pill, configuration: config)
        XCTAssertFalse(panel.styleMask.contains(.nonactivatingPanel))
        XCTAssertTrue(panel.styleMask.contains(.borderless))
        XCTAssertTrue(panel.canBecomeKey)
    }

    // MARK: 523 full-screen split

    private let screen = CGRect(x: 0, y: 0, width: 1000, height: 800)

    func testEvaluateMatchesIsFullScreen() {
        let context = RightKitOverlayFullScreen.Context(screenBounds: screen, frontmostPID: 7)
        XCTAssertTrue(context.needsWindowList)
        XCTAssertTrue(RightKitOverlayFullScreen.evaluate(context, windows: [(pid: 7, layer: 0, bounds: screen)]))
        XCTAssertFalse(RightKitOverlayFullScreen.evaluate(context, windows: [(pid: 8, layer: 0, bounds: screen)]))
        XCTAssertFalse(RightKitOverlayFullScreen.evaluate(context, windows: []))
    }

    func testEvaluateHonoursFallbackAndGeometryFlag() {
        let covered = RightKitOverlayFullScreen.Context(screenBounds: screen, frontmostPID: 7, visibleFrameCoversScreen: true)
        XCTAssertFalse(covered.needsWindowList)
        XCTAssertTrue(RightKitOverlayFullScreen.evaluate(covered, windows: []))

        let noGeometry = RightKitOverlayFullScreen.Context(screenBounds: screen, frontmostPID: 7, appliesGeometryRule: false)
        XCTAssertFalse(noGeometry.needsWindowList)
        XCTAssertFalse(RightKitOverlayFullScreen.evaluate(noGeometry, windows: [(pid: 7, layer: 0, bounds: screen)]))
    }

    func testContextIsSendableAcrossThreads() async {
        let context = RightKitOverlayFullScreen.Context(screenBounds: screen, frontmostPID: 7)
        let result = await Task.detached {
            RightKitOverlayFullScreen.evaluate(context, windows: [(pid: 7, layer: 0, bounds: CGRect(x: 0, y: 0, width: 1000, height: 800))])
        }.value
        XCTAssertTrue(result)
    }

    @MainActor
    func testOneCallAgreesWithSplitForIgnoredFrontmostApp() throws {
        let front = try XCTUnwrap(NSWorkspace.shared.frontmostApplication, "needs a frontmost app")
        let id = try XCTUnwrap(front.bundleIdentifier)
        let screen = try XCTUnwrap(NSScreen.main, "needs a main screen")
        XCTAssertFalse(RightKitOverlayFullScreen.frontmostAppIsFullScreen(on: screen, ignoringBundleIDs: [id]))
        let context = try XCTUnwrap(RightKitOverlayFullScreen.context(on: screen, ignoringBundleIDs: [id]))
        XCTAssertFalse(context.appliesGeometryRule)
        XCTAssertFalse(context.visibleFrameCoversScreen)
        XCTAssertFalse(RightKitOverlayFullScreen.evaluate(context, windows: []))
    }

    func testOnScreenWindowsRunsOffMain() async {
        // Hardware/session-bound: only checks the call is callable off the main thread.
        let windows = await Task.detached { RightKitOverlayFullScreen.onScreenWindows() }.value
        XCTAssertGreaterThanOrEqual(windows.count, 0)
    }
}
#endif

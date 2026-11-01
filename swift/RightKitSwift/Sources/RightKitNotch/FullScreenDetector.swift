#if os(macOS)
import AppKit
import CoreGraphics
import RightKitOverlay

/// Detects whether a full-screen application window is active on a given display.
enum FullScreenDetector {
    /// What the main thread knows about the question, so the blocking window-list
    /// call can run elsewhere and the WindowServer round trip stays off the main thread.
    /// RightKit's one-call `frontmostAppIsFullScreen` reads the window list on the caller,
    /// so it cannot replace this split.
    struct Context: Sendable {
        let screenBounds: CGRect
        let frontmostPID: pid_t
        let safeAreaTopInset: CGFloat
        /// The menu-bar-autohide fallback already answers yes: no window list needed.
        let hiddenMenuBarFullScreen: Bool
    }

    /// Main-thread half: NSScreen and NSWorkspace facts. Nil means "not full screen".
    static func context(on screen: NSScreen?) -> Context? {
        guard let screen = screen ?? NSScreen.main else { return nil }
        guard let frontApp = NSWorkspace.shared.frontmostApplication else { return nil }
        guard frontApp.bundleIdentifier != Bundle.main.bundleIdentifier else { return nil }

        let primaryHeight = NSScreen.screens.first?.frame.height ?? screen.frame.height
        let cgScreenBounds = CGRect(
            x: screen.frame.minX,
            y: primaryHeight - screen.frame.maxY,
            width: screen.frame.width,
            height: screen.frame.height
        )
        let safeTop: CGFloat
        if #available(macOS 12.0, *) {
            safeTop = screen.safeAreaInsets.top
        } else {
            safeTop = 0
        }
        let hidden = RightKitOverlayFullScreen.visibleFrameCoversScreen(
            frame: screen.frame, visibleFrame: screen.visibleFrame
        ) && frontApp.bundleIdentifier != "com.apple.finder"
        return Context(screenBounds: cgScreenBounds, frontmostPID: frontApp.processIdentifier,
                       safeAreaTopInset: safeTop, hiddenMenuBarFullScreen: hidden)
    }

    /// Blocking WindowServer call; safe to run on any thread.
    static func onScreenWindows() -> [(pid: pid_t, layer: Int, bounds: CGRect)] {
        var extractedWindows: [(pid: pid_t, layer: Int, bounds: CGRect)] = []
        if let windowInfoList = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] {
            for info in windowInfoList {
                guard let pid = info[kCGWindowOwnerPID as String] as? pid_t,
                      let layer = info[kCGWindowLayer as String] as? Int,
                      let boundsDict = info[kCGWindowBounds as String] as? NSDictionary,
                      let bounds = CGRect(dictionaryRepresentation: boundsDict as CFDictionary)
                else { continue }
                extractedWindows.append((pid: pid, layer: layer, bounds: bounds))
            }
        }
        return extractedWindows
    }

    static func evaluate(_ context: Context, windows: [(pid: pid_t, layer: Int, bounds: CGRect)]) -> Bool {
        RightKitOverlayFullScreen.isFullScreen(
            screenBounds: context.screenBounds,
            frontmostPID: context.frontmostPID,
            windows: windows,
            safeAreaTopInset: context.safeAreaTopInset
        ) || context.hiddenMenuBarFullScreen
    }
}
#endif

#if os(macOS)
import AppKit
import ApplicationServices
import RightKitAccessibility

/// Pulse: moves windows for a `WindowAction` through the Accessibility API.
///
/// This is the one engine. The hotkeys call `perform(_:)` on the focused
/// window; the green-button maximizer calls `toggleMaximize(_:)` on the window
/// under its button. Both keep their memory of earlier frames here, so Restore
/// and a second green-button click return a window to the same place.
///
/// Every frame is in Accessibility coordinates (origin at the top-left of the
/// primary display) and sized against the screen's visible frame, which leaves
/// out the menu bar and the Dock. Needs the Accessibility permission; without
/// it every call returns false and nothing moves.
enum WindowActions {
    /// One display: its whole frame and its visible frame, both in
    /// Accessibility coordinates.
    struct Screen {
        let full: CGRect
        let visible: CGRect
    }

    private static let lock = NSLock()
    /// Frames before the last move, keyed by the window element's hash.
    nonisolated(unsafe) private static var previous: [Int: CGRect] = [:]
    /// Bounds the memory; a window that never moves again costs one entry.
    private static let memoryLimit = 128

    // MARK: - Public

    /// Runs an action on the focused window of the frontmost app.
    @discardableResult
    static func perform(_ action: WindowAction) -> Bool {
        guard let window = focusedWindow() else { return false }
        return perform(action, on: window)
    }

    /// Runs an action on one window. False when the window cannot be moved
    /// (Pulse's own windows, full-screen windows, no Accessibility) or the
    /// action has nothing to do (Restore with no memory, a single display).
    @discardableResult
    static func perform(_ action: WindowAction, on window: RightKitAXElement) -> Bool {
        guard isMovable(window), let current = window.frame else { return false }
        let all = screens()
        guard let index = screenIndex(for: current, in: all) else { return false }
        let key = window.hashValue
        switch action {
        case .restore:
            guard let saved = remembered(key) else { return false }
            forget(key)
            return setFrame(saved, of: window)
        case .nextDisplay, .previousDisplay:
            guard all.count > 1 else { return false }
            let step = action == .nextDisplay ? 1 : all.count - 1
            let there = all[(index + step) % all.count]
            let target = WindowGeometry.moved(current, from: all[index].visible, to: there.visible)
            return move(window, from: current, to: target, key: key)
        default:
            guard let target = WindowGeometry.target(for: action, current: current,
                                                     visible: all[index].visible)
            else { return false }
            return move(window, from: current, to: target, key: key)
        }
    }

    /// The green button's job: fill the visible frame, or return to the frame
    /// from before when the window already fills it and one was remembered.
    @discardableResult
    static func toggleMaximize(_ window: RightKitAXElement) -> Bool {
        guard isMovable(window), let current = window.frame else { return false }
        let all = screens()
        guard let index = screenIndex(for: current, in: all) else { return false }
        let visible = all[index].visible.integral
        let key = window.hashValue
        if near(current, visible), let saved = remembered(key) {
            forget(key)
            return setFrame(saved, of: window)
        }
        return move(window, from: current, to: visible, key: key)
    }

    /// The focused window of the frontmost app, or nil.
    static func focusedWindow() -> RightKitAXElement? {
        let system = RightKitAXElement.systemWide
        system.setMessagingTimeout(0.15)
        guard let focusedApp = system.element("AXFocusedApplication"), let pid = focusedApp.pid else { return nil }
        let application = RightKitAXElement.application(pid: pid)
        application.setMessagingTimeout(0.25)
        return application.focusedWindow
    }

    // MARK: - Screens

    /// Every display, in the order of `NSScreen.screens`, in Accessibility
    /// coordinates. The primary display's height is the flip point.
    static func screens() -> [Screen] {
        guard let primary = NSScreen.screens.first else { return [] }
        let height = primary.frame.height
        func flip(_ rect: CGRect) -> CGRect {
            CGRect(x: rect.minX, y: height - rect.maxY, width: rect.width, height: rect.height)
        }
        return NSScreen.screens.map { Screen(full: flip($0.frame), visible: flip($0.visibleFrame)) }
    }

    /// The display where most of the window is. A window off every display
    /// belongs to the first one.
    static func screenIndex(for window: CGRect, in screens: [Screen]) -> Int? {
        var best: Int?
        var bestArea: CGFloat = 0
        for (index, screen) in screens.enumerated() {
            let overlap = screen.full.intersection(window)
            let area = overlap.isNull ? 0 : overlap.width * overlap.height
            if area > bestArea {
                bestArea = area
                best = index
            }
        }
        return best ?? (screens.isEmpty ? nil : 0)
    }

    // MARK: - Moving

    private static func move(_ window: RightKitAXElement, from current: CGRect, to target: CGRect,
                             key: Int) -> Bool {
        if !near(current, target) { remember(current, key) }
        return setFrame(target, of: window)
    }

    private static func setFrame(_ rect: CGRect, of window: RightKitAXElement) -> Bool {
        (try? window.setFrame(rect)) != nil
    }

    /// Pulse's own windows and full-screen or fixed windows are left alone.
    private static func isMovable(_ window: RightKitAXElement) -> Bool {
        guard (window.pid ?? 0) != ProcessInfo.processInfo.processIdentifier else { return false }
        guard window.isFullScreen != true else { return false }
        return AX.isSettable(window, "AXPosition") && AX.isSettable(window, "AXSize")
    }

    private static func near(_ a: CGRect, _ b: CGRect) -> Bool {
        abs(a.minX - b.minX) < 3 && abs(a.minY - b.minY) < 3
            && abs(a.width - b.width) < 3 && abs(a.height - b.height) < 3
    }

    // MARK: - Memory

    private static func remember(_ frame: CGRect, _ key: Int) {
        lock.lock(); defer { lock.unlock() }
        if previous.count >= memoryLimit, previous[key] == nil { previous.removeAll() }
        previous[key] = frame
    }

    private static func remembered(_ key: Int) -> CGRect? {
        lock.lock(); defer { lock.unlock() }
        return previous[key]
    }

    private static func forget(_ key: Int) {
        lock.lock(); defer { lock.unlock() }
        previous[key] = nil
    }
}
#endif

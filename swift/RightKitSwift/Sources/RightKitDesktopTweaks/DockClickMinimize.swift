#if os(macOS)
import AppKit
import ApplicationServices
import RightKitAccessibility

/// the app: clicking the Dock icon of the app that is already frontmost
/// minimizes its windows. The click itself is never swallowed, so the Dock
/// behaves as usual for every other icon and for an app with nothing to
/// minimize (its minimized windows still come back).
///
/// Nothing slow runs on the event tap's thread (#436): the press is only
/// numbered and handed to `queue`, which does the Accessibility hit test; the
/// release is handed to the same serial queue, so it is judged only after that
/// press's check has finished, and the tap never waits for either.
final class DockClickMinimize {
    private let lock = NSLock()
    private let queue = DispatchQueue(label: "dev.orthic.pulse.dock.click", qos: .userInitiated)
    /// The number of the latest press; a check or release for an older one does nothing.
    private var press = 0
    /// Set by `queue` when press `id` was on the front app's own Dock icon.
    private var onFrontIcon: (id: Int, pid: pid_t, location: CGPoint)?

    func handle(_ type: CGEventType, _ event: CGEvent) -> TapDecision {
        switch type {
        case .leftMouseDown:
            lock.lock(); press += 1; let id = press; onFrontIcon = nil; lock.unlock()
            let blockers: CGEventFlags = [.maskCommand, .maskAlternate, .maskControl, .maskShift]
            guard event.flags.intersection(blockers).isEmpty else { return .pass }
            let location = event.location
            queue.async { [weak self] in self?.check(id, at: location) }
        case .leftMouseUp:
            lock.lock(); let id = press; lock.unlock()
            let here = event.location
            queue.async { [weak self] in self?.release(id, at: here) }
        default:
            break
        }
        return .pass
    }

    func reset() {
        lock.lock(); press += 1; onFrontIcon = nil; lock.unlock()
    }

    /// On `queue`: whether press `id` landed on the Dock icon of the app in front.
    private func check(_ id: Int, at location: CGPoint) {
        lock.lock(); let current = press == id; lock.unlock()
        guard current,
              let front = NSWorkspace.shared.frontmostApplication,
              front.processIdentifier != ProcessInfo.processInfo.processIdentifier,
              let appURL = front.bundleURL,
              let found = AX.elementAt(location),
              case let hit = RightKitAXElement(found),
              hit.string("AXRole") == "AXDockItem",
              let itemURL = (try? hit.value("AXURL")) as? URL,
              itemURL.standardizedFileURL == appURL.standardizedFileURL,
              !Self.visibleWindows(of: front.processIdentifier).isEmpty
        else { return }
        lock.lock()
        if press == id { onFrontIcon = (id, front.processIdentifier, location) }
        lock.unlock()
    }

    /// On `queue`, after press `id`'s check: minimizes when that press was on the icon.
    private func release(_ id: Int, at here: CGPoint) {
        lock.lock()
        let click = onFrontIcon?.id == id ? onFrontIcon : nil
        if click != nil { onFrontIcon = nil }
        lock.unlock()
        guard let click else { return }
        // A drag that started on the icon is a rearrangement, not a click.
        guard hypot(here.x - click.location.x, here.y - click.location.y) < 6 else { return }
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.05) {
            for window in Self.visibleWindows(of: click.pid) {
                try? RightKitAXWindowAction.setMinimized(window, true)
            }
        }
    }

    /// Standard windows that are on screen now. Unknown reads as none.
    private static func visibleWindows(of pid: pid_t) -> [RightKitAXElement] {
        let application = RightKitAXElement.application(pid: pid)
        application.setMessagingTimeout(0.25)
        return application.windows.filter {
            $0.subrole == "AXStandardWindow" && $0.isMinimized == false
        }
    }
}
#endif

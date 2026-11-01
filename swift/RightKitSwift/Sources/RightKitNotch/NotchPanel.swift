#if os(macOS)
import AppKit
import RightKitOverlay

/// Borderless, non-activating panel that floats over everything, including the
/// menu bar and full-screen apps. Non-activating matters: glancing at your
/// usage must never take focus off what you were actually doing.
final class NotchPanel: RightKitOverlayPanel {
    /// Supplies the right-click menu. Handled here rather than on the content
    /// view because `NSWindow.sendEvent` sees every event first — the hosting
    /// view's hit test resolves to a SwiftUI-owned subview, which has no menu
    /// of its own and may consume the click before it reaches us.
    var contextMenuProvider: (() -> NSMenu?)?
    /// A left click on the visible chrome. Handled here for the same reason the
    /// menu is: the hit test lands on a SwiftUI subview that may consume it.
    var onClick: ((CGPoint) -> Void)?
    /// ⌥-drag on the chrome, reported as the raw pointer delta since the last
    /// event — not a cumulative offset, so the caller decides what "along the
    /// edge" means for the current one. Chosen over a plain click-and-hold
    /// threshold so an ordinary click never risks being read as a tiny nudge.
    /// Starts a drag. The point is where the press began, in screen coordinates,
    /// when the drag began later than the press (the bulb waits for 3pt);
    /// nil for an ⌥-drag, which begins at the press.
    var onDragStart: ((CGPoint?) -> Void)?
    /// Whether ⌥-drag moves the notch (Settings > Option-drag to move the notch). On unless switched off.
    var allowsOptionDrag: () -> Bool = { true }
    var onDrag: ((CGFloat, CGFloat) -> Void)?
    /// The ⌥-drag ended. Where to persist the offset the drags above moved to.
    var onDragEnd: (() -> Void)?

    /// Whether a press at this point, in the window's own coordinates, is on
    /// something that carries the notch without ⌥ — the handle's bulb. Such a
    /// press is a click until the pointer has moved `dragThreshold`.
    var startsDrag: ((CGPoint) -> Bool)?

    /// How far the pointer moves from a press on the bulb before it is a drag
    /// and not a click.
    static let dragThreshold: CGFloat = 3

    /// The press that starts a click or a drag. It runs only when no view took the
    /// event, so the SwiftUI chrome keeps its own clicks.
    private func handlePress(_ event: NSEvent) -> Bool {
        guard event.type == .leftMouseDown else { return false }
        guard let view = contentView, view.hitTest(event.locationInWindow) != nil else {
            return false
        }
        let optionDrag = event.modifierFlags.contains(.option) && allowsOptionDrag()
        let onBulb = !optionDrag && startsDrag?(event.locationInWindow) == true
        guard optionDrag || onBulb, onDrag != nil else {
            onClick?(event.locationInWindow)
            return true
        }
        if onBulb {
            trackPressOrDrag(from: event)
            return true
        }
        onDragStart?(nil)
        trackOptionDrag()
        return true
    }

    /// **A press on the bulb**: a click if the button comes up within 3pt of
    /// where it went down, and otherwise a drag, from the first move past 3pt.
    /// The drag is told the whole distance since the press at once and the
    /// steps after it, so the notch is where the pointer has taken it from the
    /// press — there is no catching up and no jump. Distances are summed from
    /// the events' own deltas rather than read off the window, which the carry
    /// may move under the pointer. `dy` is positive down, as `NSEvent.deltaY` is.
    private func trackPressOrDrag(from press: NSEvent) {
        let pressed = convertPoint(toScreen: press.locationInWindow)
        var dx: CGFloat = 0
        var dy: CGFloat = 0
        var dragging = false
        while let event = nextEvent(matching: [.leftMouseDragged, .leftMouseUp]) {
            switch event.type {
            case .leftMouseDragged:
                if dragging {
                    onDrag?(event.deltaX, event.deltaY)
                    continue
                }
                dx += event.deltaX
                dy += event.deltaY
                guard hypot(dx, dy) > Self.dragThreshold else { continue }
                dragging = true
                onDragStart?(pressed)
                onDrag?(dx, dy)
            case .leftMouseUp:
                if dragging {
                    onDragEnd?()
                } else {
                    onClick?(press.locationInWindow)
                }
                return
            default:
                return
            }
        }
    }

    /// Blocks on this window's own event stream until the button lifts, the
    /// standard AppKit pattern for a custom drag started from `mouseDown`.
    /// Never falls through to `onClick` on release: an ⌥-drag is a distinct
    /// gesture from the start, not a click that grew into one, so there is
    /// nothing to reinterpret once the button comes up.
    private func trackOptionDrag() {
        while let event = nextEvent(matching: [.leftMouseDragged, .leftMouseUp]) {
            switch event.type {
            case .leftMouseDragged:
                onDrag?(event.deltaX, event.deltaY)
            case .leftMouseUp:
                onDragEnd?()
                return
            default:
                return
            }
        }
    }

    init(contentRect: NSRect) {
        // The standard notch panel (statusBar level, joins all spaces and full-screen
        // spaces, stationary, never key), with the window fixed in place.
        var configuration = RightKitOverlayPanelConfiguration.standard(kind: .notch)
        configuration.isMovable = false
        super.init(contentRect: contentRect, kind: .notch, configuration: configuration)
        // The right-click menu. The filter sees the event before the window does, so a
        // consumed event never reaches `sendEvent`'s default handling.
        eventFilter = { [weak self] event in
            guard event.type == .rightMouseDown,
                  let self,
                  let menu = self.contextMenuProvider?(),
                  let view = self.contentView,
                  // Only over the visible chrome; elsewhere the panel is a hole.
                  view.hitTest(event.locationInWindow) != nil
            else { return false }
            NSMenu.popUpContextMenu(menu, with: event, for: view)
            return true
        }
        unhandledMouseEvent = { [weak self] event in self?.handlePress(event) ?? false }
    }
}
#endif

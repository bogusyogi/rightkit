#if os(macOS)
import AppKit

/// Which screen a point is on, in AppKit's screen order. Panels that open where the pointer is
/// (a launcher, a strip, a wheel, a card) all ask the same question; the host keeps its own
/// fallback (`NSScreen.main`, the first screen) and its placement.
public enum RightKitDisplayLookup {
    /// How a screen's frame is tested against the point.
    public enum Containment: Sendable {
        /// `CGRect.contains`: the minimum edges are inside, the maximum edges are not.
        case frameContains
        /// `NSMouseInRect(point, frame, false)`, the rule for a mouse location in unflipped
        /// screen coordinates.
        case mouseInRect
    }

    /// The first screen, in `NSScreen.screens` order, whose frame holds the point; nil when none
    /// does or there are no screens.
    ///
    /// `point` is evaluated for each screen visited and not at all when there are no screens, so a
    /// caller can pass `NSEvent.mouseLocation` to read the pointer as each screen is tested, or a
    /// point it captured earlier. The search stops at the first match.
    @MainActor
    public static func screen(
        at point: @autoclosure () -> NSPoint,
        containment: Containment = .frameContains
    ) -> NSScreen? {
        NSScreen.screens.first { screen in
            switch containment {
            case .frameContains: screen.frame.contains(point())
            case .mouseInRect: NSMouseInRect(point(), screen.frame, false)
            }
        }
    }
}
#endif

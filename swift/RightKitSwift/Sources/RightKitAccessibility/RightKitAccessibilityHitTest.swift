import Foundation

#if os(macOS)
import ApplicationServices

extension RightKitAXElement {
    /// The deepest accessibility element at a point in global display coordinates (top-left origin),
    /// across all apps, or nil when nothing is there or Accessibility is not trusted. Pass `timeout`
    /// (seconds) to cap how long a hung app can block the call.
    public static func element(atPosition point: CGPoint, timeout: Float? = nil) -> RightKitAXElement? {
        let system = AXUIElementCreateSystemWide()
        if let timeout { AXUIElementSetMessagingTimeout(system, timeout) }
        var out: AXUIElement?
        let error = AXUIElementCopyElementAtPosition(system, Float(point.x), Float(point.y), &out)
        guard error == .success, let out else { return nil }
        return RightKitAXElement(out)
    }
}
#endif

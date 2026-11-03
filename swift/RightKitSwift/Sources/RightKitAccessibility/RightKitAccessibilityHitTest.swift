import Foundation

#if os(macOS)
import ApplicationServices

extension RightKitAXElement {
    /// The deepest accessibility element at a point in global display coordinates (top-left origin),
    /// across all apps, or nil when nothing is there or Accessibility is not trusted. Pass `timeout`
    /// (seconds) to cap how long a hung app can block the call.
    ///
    /// `timeout` is PROCESS-WIDE: a timeout set on a system-wide element becomes the default for every
    /// AX element in the process, so this changes the bound of every other AX call that has not set
    /// its own. To bound one hit test only, use the instance form on an application element and set
    /// `setMessagingTimeout` on that element.
    public static func element(atPosition point: CGPoint, timeout: Float? = nil) -> RightKitAXElement? {
        let system = AXUIElementCreateSystemWide()
        if let timeout { AXUIElementSetMessagingTimeout(system, timeout) }
        var out: AXUIElement?
        let error = AXUIElementCopyElementAtPosition(system, Float(point.x), Float(point.y), &out)
        guard error == .success, let out else { return nil }
        return RightKitAXElement(out)
    }

    /// The deepest accessibility element at a point in global display coordinates (top-left origin)
    /// inside THIS element: an application element made with `application(pid:)` asks that one app,
    /// the system-wide element asks across all apps. nil when nothing is there or Accessibility is not
    /// trusted. Sets no timeout: the bound is whatever the caller gave this element through
    /// `setMessagingTimeout`, so no other element's bound changes.
    public func element(atPosition point: CGPoint) -> RightKitAXElement? {
        var out: AXUIElement?
        let error = AXUIElementCopyElementAtPosition(raw, Float(point.x), Float(point.y), &out)
        guard error == .success, let out else { return nil }
        return RightKitAXElement(out)
    }
}
#endif

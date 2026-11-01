#if os(macOS)
import ApplicationServices
import AppKit
import RightKitAccessibility

/// Pulse: Accessibility helpers for the conveniences that still hold a raw `AXUIElement`
/// (the event-tap callers). Everything is `RightKitAXElement` underneath; only the hit test
/// and the settable check stay on raw AX, because RightKit has neither (rightkit#189, #424).
/// Every read has a short messaging timeout so a hung app cannot stall the
/// event tap, and every failure reads as "unknown" (nil), never as a guess.
enum AX {
    static let systemWide: AXUIElement = {
        let element = RightKitAXElement.systemWide
        element.setMessagingTimeout(0.15)
        return element.raw
    }()

    static func application(_ pid: pid_t, timeout: Float = 0.25) -> AXUIElement {
        let element = RightKitAXElement.application(pid: pid)
        element.setMessagingTimeout(timeout)
        return element.raw
    }

    static func attribute(_ element: AXUIElement, _ name: String) -> CFTypeRef? {
        try? RightKitAXElement(element).value(name)
    }

    static func string(_ element: AXUIElement, _ name: String) -> String? {
        RightKitAXElement(element).string(name)
    }

    static func element(_ element: AXUIElement, _ name: String) -> AXUIElement? {
        RightKitAXElement(element).element(name)?.raw
    }

    /// RightKit has no `isSettable` (rightkit#424): raw AX.
    static func isSettable(_ element: RightKitAXElement, _ name: String) -> Bool {
        var settable = DarwinBoolean(false)
        guard AXUIElementIsAttributeSettable(element.raw, name as CFString, &settable) == .success else { return false }
        return settable.boolValue
    }

    /// `AXChildren` of an element (RightKit 0.8.1 has `elements(_:)` but no `children`, and its
    /// `elements` answers `[]` for an AX error too). Nil on any AX error, so a caller can tell
    /// "could not read" from "no children"; no value or no such attribute is a leaf, `[]`.
    static func children(_ element: RightKitAXElement) -> [RightKitAXElement]? {
        var out: CFTypeRef?
        let error = AXUIElementCopyAttributeValue(element.raw, "AXChildren" as CFString, &out)
        switch error {
        case .success:
            guard let list = out as? [AXUIElement] else { return nil }
            return list.map(RightKitAXElement.init)
        case .noValue, .attributeUnsupported:
            return []
        default:
            return nil
        }
    }

    /// A numeric attribute (RightKit 0.8.1 has `string` and `bool` only); nil when absent or not a number.
    static func int(_ element: RightKitAXElement, _ name: String) -> Int? {
        guard let value = try? element.value(name), CFGetTypeID(value) == CFNumberGetTypeID() else { return nil }
        return (value as? NSNumber)?.intValue
    }

    /// RightKit has no hit test (rightkit#189): raw AX.
    static func elementAt(_ location: CGPoint) -> AXUIElement? {
        var found: AXUIElement?
        let result = AXUIElementCopyElementAtPosition(systemWide, Float(location.x), Float(location.y), &found)
        return result == .success ? found : nil
    }

    /// The element at a point inside one app, for a caller on the event tap's thread (#436).
    /// It asks through an application element of its own with a short messaging timeout:
    /// a timeout set on any system-wide element becomes the global default for every
    /// element, so `systemWide` and the global value are never touched here.
    static func elementAt(_ location: CGPoint, in pid: pid_t, timeout: Float) -> AXUIElement? {
        var found: AXUIElement?
        let result = AXUIElementCopyElementAtPosition(
            application(pid, timeout: timeout), Float(location.x), Float(location.y), &found)
        return result == .success ? found : nil
    }
}
#endif

#if os(macOS)
import CoreGraphics
import Foundation

public enum RightKitWindowVisibility {
    /// Synchronously probes one WindowServer ID. Returns nil if visibility is unavailable.
    public static func isOnScreen(_ id: CGWindowID) -> Bool? {
        guard id != kCGNullWindowID else { return nil }
        // WindowServer expects the numeric ID in a pointer slot, with no retain/release callbacks.
        var slot = UnsafeRawPointer(bitPattern: UInt(id))
        guard let ids = CFArrayCreate(nil, &slot, 1, nil),
              let rows = CGWindowListCreateDescriptionFromArray(ids) as? [[String: Any]],
              let row = rows.first(where: { ($0[kCGWindowNumber as String] as? UInt32) == id })
        else { return nil }
        return row[kCGWindowIsOnscreen as String] as? Bool
    }
}
#endif

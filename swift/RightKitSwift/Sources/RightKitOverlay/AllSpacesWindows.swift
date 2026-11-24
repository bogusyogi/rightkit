#if os(macOS)
import CoreGraphics
import Foundation

public enum RightKitAllSpacesWindows {
    /// Reads WindowServer synchronously, including hidden and minimized windows.
    /// Returns nil when the list cannot be read. Missing dimensions are passed to the caller.
    public static func contains(
        ownerPID: pid_t,
        layer: Int,
        matchingSize: (_ width: CGFloat?, _ height: CGFloat?) -> Bool
    ) -> Bool? {
        guard let rows = CGWindowListCopyWindowInfo(.optionAll, kCGNullWindowID) as? [[String: Any]]
        else { return nil }
        return rows.contains { row in
            guard (row[kCGWindowOwnerPID as String] as? Int32) == ownerPID,
                  (row[kCGWindowLayer as String] as? Int) == layer else { return false }
            let bounds = row[kCGWindowBounds as String] as? [String: CGFloat]
            return matchingSize(bounds?["Width"], bounds?["Height"])
        }
    }
}
#endif

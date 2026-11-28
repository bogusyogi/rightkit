#if os(macOS)
import AppKit
import Foundation

public enum RightKitTextMetrics {
    /// The rendered width of `text` in `font`, rounded up to a whole point: measures glyphs, not
    /// character count. The host adds its own padding and allowances.
    @MainActor
    public static func width(_ text: String, font: NSFont) -> CGFloat {
        ceil((text as NSString).size(withAttributes: [.font: font]).width)
    }
}
#endif

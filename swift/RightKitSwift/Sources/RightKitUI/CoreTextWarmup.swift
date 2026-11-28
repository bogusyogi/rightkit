#if canImport(CoreText)
import CoreGraphics
import CoreText
import Foundation

public enum CoreTextWarmup {
    /// CoreText's first glyph draw derives language metadata for a bundled font, which otherwise lands
    /// on the first main-thread frame. This draws `sample` in `fontName` at `size` into a private
    /// bitmap sized to the text, touching no AppKit or SwiftUI state, so it is safe on a background
    /// queue (the caller picks the queue). A font name CoreText cannot resolve warms its fallback.
    public static func prime(fontName: String, size: CGFloat, sample: String) {
        autoreleasepool {
            let font = CTFontCreateWithName(fontName as CFString, size, nil)
            let attributes: [CFString: Any] = [kCTFontAttributeName: font]
            let line = CTLineCreateWithAttributedString(
                CFAttributedStringCreate(nil, sample as CFString, attributes as CFDictionary)
            )
            let bounds = CTLineGetBoundsWithOptions(line, [])
            guard let context = CGContext(
                data: nil,
                width: max(1, Int(ceil(bounds.width + 4))),
                height: max(1, Int(ceil(bounds.height + 4))),
                bitsPerComponent: 8,
                bytesPerRow: 0,
                space: CGColorSpaceCreateDeviceRGB(),
                bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
            ) else { return }
            context.textPosition = CGPoint(x: 2 - bounds.minX, y: 2 - bounds.minY)
            CTLineDraw(line, context)
        }
    }
}
#endif

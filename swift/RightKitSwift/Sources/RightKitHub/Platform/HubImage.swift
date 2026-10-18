import Foundation
import SwiftUI

#if canImport(CoreGraphics)
import CoreGraphics
#elseif os(Windows) && canImport(SwiftRightHost)
import SwiftRightHost
#endif

/// A decoded picture, made by `HubPlatformServices.decodeImage` and drawn by `HubImageView`.
///
/// It is a value a platform's services hand to the base: the base never decodes bytes, and a view
/// never touches the platform's image types. On Apple platforms it holds a `CGImage` (the Mac
/// services build it with ImageIO, in `PlatformServicesMac.swift`) and draws it with
/// `Image(decorative:scale:)`. A platform without CoreGraphics supplies its own backing when it is
/// ported; on Windows that is SwiftRight's `RGBAImage` (when the build links SwiftRightHost). Elsewhere
/// `image(fitting:)` returns nil and `HubImageView` shows its placeholder.
///
/// `@unchecked Sendable`: the backing image is immutable once decoded and is only read.
public struct HubImage: @unchecked Sendable {
    /// Width of the decoded bitmap in pixels.
    public let pixelWidth: Int
    /// Height of the decoded bitmap in pixels.
    public let pixelHeight: Int

    #if canImport(CoreGraphics)
    private let backing: CGImage

    /// Wraps a decoded bitmap.
    public init(cgImage: CGImage) {
        backing = cgImage
        pixelWidth = cgImage.width
        pixelHeight = cgImage.height
    }
    #elseif os(Windows) && canImport(SwiftRightHost)
    private let backing: RGBAImage

    /// Wraps a bitmap decoded by SwiftRight's host (`ImageDecoder`), the Windows backing.
    public init(rgba: RGBAImage) {
        backing = rgba
        pixelWidth = rgba.width
        pixelHeight = rgba.height
    }
    #endif

    /// The SwiftUI image, at its pixel size divided by a scale that makes it fit inside `box` (in
    /// points) with its proportions kept, and never larger than one pixel per point. With no box,
    /// one pixel per point. Nil when this platform has no way to draw the backing image.
    ///
    /// The scale is how the image is sized without `.resizable`: a bitmap decoded for twice the box
    /// draws at the box's size, sharp on a 2x display.
    public func image(fitting box: CGSize? = nil) -> Image? {
        #if canImport(CoreGraphics)
        var scale: CGFloat = 1
        if let box, box.width > 0, box.height > 0 {
            scale = max(1, CGFloat(pixelWidth) / box.width, CGFloat(pixelHeight) / box.height)
        }
        return Image(decorative: backing, scale: scale)
        #elseif os(Windows) && canImport(SwiftRightHost)
        var scale: CGFloat = 1
        if let box, box.width > 0, box.height > 0 {
            scale = max(1, CGFloat(pixelWidth) / box.width, CGFloat(pixelHeight) / box.height)
        }
        return Image(decorative: backing, scale: scale)
        #else
        return nil
        #endif
    }
}

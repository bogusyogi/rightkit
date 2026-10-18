import SwiftUI

/// What an image was decoded from, cheap to compare: the byte count, a hash of the bytes and the
/// pixel limit. The bytes themselves are not kept.
struct HubImageKey: Hashable, Sendable {
    let count: Int
    let digest: Int
    let maxPixel: Int

    init(_ data: Data, maxPixel: Int) {
        count = data.count
        digest = data.hashValue
        self.maxPixel = maxPixel
    }
}

/// Decoded images already made, so a row that scrolls back into view draws at once and a picture
/// taken after `HubImages.preload` does not wait on a task. Main actor only. When it holds more
/// than `limit` images it is emptied: it is a convenience, never the only copy.
@MainActor
final class HubImageCache {
    static let shared = HubImageCache()
    private let limit = 256
    private var images: [HubImageKey: HubImage] = [:]

    func image(for key: HubImageKey) -> HubImage? {
        images[key]
    }

    func store(_ image: HubImage, for key: HubImageKey) {
        if images.count >= limit { images.removeAll() }
        images[key] = image
    }
}

/// Decodes images ahead of drawing them.
public enum HubImages {
    /// Decodes `data` through `services` and keeps the result, so the next `HubImageView` of the same
    /// bytes and `maxPixel` draws without waiting. Returns false when the bytes did not decode.
    /// A model calls this before it publishes bytes to its view; the off-screen renderer relies on it
    /// because a picture is taken once, with no time for a view's own task.
    @MainActor
    public static func preload(_ data: Data, maxPixel: Int, services: any HubPlatformServices) async -> Bool {
        let key = HubImageKey(data, maxPixel: maxPixel)
        if HubImageCache.shared.image(for: key) != nil { return true }
        guard let image = await services.decodeImage(data, maxPixel: maxPixel) else { return false }
        HubImageCache.shared.store(image, for: key)
        return true
    }
}

/// Shows an image from bytes (a PNG the engine sent, say). The platform services in the environment
/// (`\.hubServices`, set by `HubShell`) decode it; the view uses no AppKit, UIKit or ImageIO.
///
/// While the image is decoding, when the bytes are not an image, and on a platform that cannot decode,
/// the view shows `placeholder`, so a caller gives it the size of the final image to keep the layout
/// still. The image is sized by `fitting` (see `HubImage.image(fitting:)`); decode no more pixels
/// than the box needs at 2x, via `maxPixel`.
///
///     HubImageView(data: bytes, maxPixel: 64, fitting: CGSize(width: 28, height: 28)) {
///         Image(systemName: "app.dashed")
///     }
public struct HubImageView<Placeholder: View>: View {
    private struct Decoded {
        let key: HubImageKey
        let image: HubImage
    }

    private let data: Data
    private let maxPixel: Int
    private let box: CGSize?
    private let placeholder: Placeholder
    @Environment(\.hubServices) private var services
    @State private var decoded: Decoded?

    /// - Parameters:
    ///   - data: The encoded image.
    ///   - maxPixel: The longest side of the decoded bitmap, in pixels. Never scaled up.
    ///   - fitting: The box, in points, the image is sized to fit; nil draws one pixel per point.
    ///   - placeholder: Shown until there is an image, and in place of one that cannot be decoded.
    public init(
        data: Data,
        maxPixel: Int,
        fitting box: CGSize? = nil,
        @ViewBuilder placeholder: () -> Placeholder
    ) {
        self.data = data
        self.maxPixel = maxPixel
        self.box = box
        self.placeholder = placeholder()
    }

    private var key: HubImageKey { HubImageKey(data, maxPixel: maxPixel) }

    public var body: some View {
        let current = key
        let ready = HubImageCache.shared.image(for: current) ?? (decoded?.key == current ? decoded?.image : nil)
        content(ready)
            .task(id: current) {
                await decode(current)
            }
    }

    @ViewBuilder
    private func content(_ ready: HubImage?) -> some View {
        if let image = ready?.image(fitting: box) {
            image
        } else {
            placeholder
        }
    }

    @MainActor
    private func decode(_ current: HubImageKey) async {
        if HubImageCache.shared.image(for: current) != nil { return }
        guard let image = await services.decodeImage(data, maxPixel: maxPixel), !Task.isCancelled else { return }
        HubImageCache.shared.store(image, for: current)
        decoded = Decoded(key: current, image: image)
    }
}

public extension HubImageView where Placeholder == Color {
    /// With nothing drawn while the image is not there.
    init(data: Data, maxPixel: Int, fitting box: CGSize? = nil) {
        self.init(data: data, maxPixel: maxPixel, fitting: box) { Color.clear }
    }
}

#if os(macOS)
import RightKitNotchCore
import AppKit
import Foundation

// Reading an item's payload, its thumbnail and its app colour (docs/clipboard-spec.md 2.5, 3.2,
// 3.6). Small files are read inline; anything that decodes a picture runs on the store's
// background queue. A card is drawn from `thumbnail(for:)` only, never from the full image.

extension LauncherClipboard {
    // MARK: Payload

    /// The bytes of one of the item's files under `items/` (`txt`, `png`, `html`, `rtf`).
    func payload(of item: ClipItem, _ ext: String) -> Data? {
        ClipSeal.read(ClipDisk.payloadURL(directory, item.id, ext))
    }

    /// The stored text of a text, link, colour, code or rich item; nil for files and images.
    func text(for item: ClipItem) -> String? {
        switch item.kind {
        case .image, .file: return nil
        default: break
        }
        guard let data = payload(of: item, "txt") else { return nil }
        return String(data: data, encoding: .utf8)
    }

    /// The full paths of a file item, as copied.
    func files(for item: ClipItem) -> [String] {
        guard item.kind == .file, let data = payload(of: item, "files.json"),
              let list = ClipJSON.parse(data)?.objectValue?["paths"]?.arrayValue else { return [] }
        return list.compactMap { $0.stringValue }
    }

    /// An image item's PNG bytes (drag out, the paste).
    func imageData(for item: ClipItem) -> Data? {
        item.kind == .image ? payload(of: item, "png") : nil
    }

    /// Names the drag-out code uses.
    func html(for item: ClipItem) -> Data? { htmlData(for: item) }
    func rtf(for item: ClipItem) -> Data? { rtfData(for: item) }
    func pngData(for item: ClipItem) -> Data? { imageData(for: item) }

    func htmlData(for item: ClipItem) -> Data? { item.kind == .rich ? payload(of: item, "html") : nil }
    func rtfData(for item: ClipItem) -> Data? { item.kind == .rich ? payload(of: item, "rtf") : nil }

    // MARK: Thumbnails

    /// The cached 320 px thumbnail, or nil while it is being made (the card draws a flat tile).
    /// A missing thumbnail file is rebuilt on the background queue and `onChange` fires when it
    /// is ready. The full image is never decoded here.
    func thumbnail(for item: ClipItem) -> NSImage? {
        guard item.kind == .image else { return nil }
        let key = item.id as NSString
        if let cached = thumbCache.object(forKey: key) { return cached }
        guard persistent else { return nil }
        if let data = ClipSeal.read(ClipDisk.thumbURL(directory, item.id)), let image = NSImage(data: data) {
            thumbCache.setObject(image, forKey: key)
            return image
        }
        rebuildThumbnail(item.id)
        return nil
    }

    private func rebuildThumbnail(_ id: String) {
        guard writable, thumbPending.insert(id).inserted else { return }
        let dir = directory
        io.async {
            var size: Int?
            if let png = ClipSeal.read(ClipDisk.payloadURL(dir, id, "png")) {
                size = ClipDisk.writeThumbnail(png: png, id: id, dir: dir)
            }
            DispatchQueue.main.async { [weak self] in
                MainActor.assumeIsolated {
                    guard let self else { return }
                    self.thumbPending.remove(id)
                    if let size { self.thumbSizes[id] = size }
                    self.scheduleChange()
                }
            }
        }
    }

    /// Gives a fixture image item its thumbnail without the disk (the picture-rendering run).
    func installFixtureThumbnail(_ image: NSImage, for id: String) {
        thumbCache.setObject(image, forKey: id as NSString)
    }

    /// The preview panel's picture. Decodes the whole image: not for drawing cards.
    func fullImage(for item: ClipItem) -> NSImage? {
        guard item.kind == .image else { return nil }
        return ClipSeal.read(ClipDisk.payloadURL(directory, item.id, "png")).flatMap { NSImage(data: $0) }
    }

    // MARK: App colour (3.6)

    /// The card band's colour for a source app: the icon's dominant colour, computed once per
    /// app id on the background queue and kept in memory and in `appcolours.json`. Neutral grey
    /// until it is known and for an unknown source.
    func appColourHex(for app: ClipSourceApp?) -> String {
        guard let app else { return ClipAppColour.neutral }
        if let known = appColours[app.id] { return known }
        computeAppColour(app.id)
        return ClipAppColour.neutral
    }

    private func computeAppColour(_ id: String) {
        guard !Runtime.isUnderTest, appColourPending.insert(id).inserted else { return }
        var icon: CGImage?
        if let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: id) {
            let picture = NSWorkspace.shared.icon(forFile: url.path)
            var rect = CGRect(x: 0, y: 0, width: 64, height: 64)
            icon = picture.cgImage(forProposedRect: &rect, context: nil, hints: nil)
        }
        let box = ClipImageBox(image: icon)
        io.async {
            let hex = box.image.map { ClipAppColour.dominant(of: $0) } ?? ClipAppColour.neutral
            DispatchQueue.main.async { [weak self] in
                MainActor.assumeIsolated {
                    guard let self else { return }
                    self.appColourPending.remove(id)
                    self.appColours[id] = hex
                    self.persistColours()
                    self.scheduleChange()
                }
            }
        }
    }
}

/// A `CGImage` handed to the background queue (CGImage is immutable and safe to share).
final class ClipImageBox: @unchecked Sendable {
    let image: CGImage?
    init(image: CGImage?) { self.image = image }
}
#endif

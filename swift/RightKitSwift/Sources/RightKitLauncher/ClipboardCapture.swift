import CoreGraphics
import Foundation

// Capture, the part that runs off the main thread (docs/clipboard-spec.md 3.2 to 3.5):
// the main thread reads the pasteboard and hands a `ClipCaptureInput` over; `prepare` applies
// the size limits, the secrets filter and detection, converts and hashes, and returns what to
// store. Pure functions: nothing here touches the pasteboard or logs a value.

/// What the main thread read from the pasteboard once the skip rules passed.
struct ClipCaptureInput: Sendable {
    /// `text`, `rich`, `file` or `image`, decided from the type list (3.5 step 5).
    var kind: ClipKind
    var text: String?
    var html: Data?
    var rtf: Data?
    var image: Data?
    var imageIsTIFF = false
    var paths: [String] = []
    var source: ClipSourceApp?
    var at: Double
    var secretFilter = true
    /// the app's own Snip, Screen and Window screenshots (4).
    var forceScreenshot = false
}

struct ClipPart: Sendable {
    var ext: String
    var data: Data
}

/// An item ready to store, without the things that need the history (id, lane, stack).
struct ClipPrepared: Sendable {
    var kind: ClipKind
    var preview: String
    var meta: ClipMeta
    var digest: String
    /// The files to write under `items/`.
    var payload: [ClipPart]
    var size: Int
    var source: ClipSourceApp?
    var at: Double
    var forceScreenshot: Bool
}

enum ClipCapture {
    static let maxText = 1024 * 1024
    static let maxRich = 1024 * 1024
    static let maxImageBytes = 20 * 1024 * 1024
    static let maxPixels = 8192
    static let maxPaths = 200

    /// nil means skip (size limits, empty or tiny text, secrets, an image that cannot be read).
    static func prepare(_ input: ClipCaptureInput) -> ClipPrepared? {
        switch input.kind {
        case .file: return prepareFiles(input)
        case .image: return prepareImage(input)
        default: return prepareText(input)
        }
    }

    private static func prepareText(_ input: ClipCaptureInput) -> ClipPrepared? {
        guard let text = input.text, text.utf8.count <= maxText else { return nil }
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed.count >= 2 else { return nil }
        if input.secretFilter, ClipSecrets.matches(trimmed) { return nil }

        var html = input.html
        var rtf = input.rtf
        if let data = html, data.isEmpty || data.count > maxRich { html = nil }
        if let data = rtf, data.isEmpty || data.count > maxRich { rtf = nil }

        let textBytes = Data(text.utf8)
        var payload = [ClipPart(ext: "txt", data: textBytes)]
        if input.kind == .rich, html != nil || rtf != nil {
            var meta = ClipMeta()
            meta.chars = text.unicodeScalars.count
            meta.hasHtml = html != nil
            meta.hasRtf = rtf != nil
            if let html { payload.append(ClipPart(ext: "html", data: html)) }
            if let rtf { payload.append(ClipPart(ext: "rtf", data: rtf)) }
            return ClipPrepared(
                kind: .rich, preview: ClipPreview.text(text), meta: meta,
                digest: ClipDigest.rich(text: text, html: html), payload: payload,
                size: payload.reduce(0) { $0 + $1.data.count }, source: input.source, at: input.at,
                forceScreenshot: false)
        }
        // Plain text, or rich whose formats are both gone: detection applies (3.3).
        let found = ClipDetect.classify(text)
        return ClipPrepared(
            kind: found.kind, preview: found.preview, meta: found.meta,
            digest: ClipDigest.text(text), payload: payload, size: textBytes.count,
            source: input.source, at: input.at, forceScreenshot: false)
    }

    private static func prepareImage(_ input: ClipCaptureInput) -> ClipPrepared? {
        guard let data = input.image, !data.isEmpty, data.count <= maxImageBytes else { return nil }
        let png: Data
        if input.imageIsTIFF {
            guard let converted = ClipImage.png(fromImageData: data) else { return nil }
            png = converted
        } else {
            png = data
        }
        guard png.count <= maxImageBytes, let size = ClipImage.dimensions(of: png),
              size.width > 0, size.height > 0, size.width <= maxPixels, size.height <= maxPixels
        else { return nil }
        var meta = ClipMeta()
        meta.width = size.width
        meta.height = size.height
        meta.format = "png"
        return ClipPrepared(
            kind: .image, preview: "", meta: meta, digest: ClipDigest.sha256(png),
            payload: [ClipPart(ext: "png", data: png)], size: png.count, source: input.source,
            at: input.at, forceScreenshot: input.forceScreenshot)
    }

    private static func prepareFiles(_ input: ClipCaptureInput) -> ClipPrepared? {
        guard !input.paths.isEmpty else { return nil }
        let stored = Array(input.paths.prefix(maxPaths))
        let names = stored.map { ($0 as NSString).lastPathComponent }
        var meta = ClipMeta()
        // The true number, even when only 200 paths are kept.
        meta.count = input.paths.count
        meta.names = Array(names.prefix(8))
        meta.anyMissing = stored.contains { !FileManager.default.fileExists(atPath: $0) }
        let json = ClipJSONWriter.object([("paths", ClipJSONWriter.strings(stored))])
        let bytes = Data(json.utf8)
        return ClipPrepared(
            kind: .file, preview: ClipPreview.files(names: names), meta: meta,
            digest: ClipDigest.files(stored), payload: [ClipPart(ext: "files.json", data: bytes)],
            size: bytes.count, source: input.source, at: input.at, forceScreenshot: false)
    }
}

/// 3.6: the colour of a source app's card band, from its icon.
enum ClipAppColour {
    static let neutral = "#8E8E93"

    /// 32 by 32, ignore alpha below 0.5 and lightness below 0.1 or above 0.95, the most common
    /// of 24 hue buckets, the average of its pixels.
    static func dominant(of image: CGImage) -> String {
        let side = 32
        var pixels = [UInt8](repeating: 0, count: side * side * 4)
        let drawn: Bool = pixels.withUnsafeMutableBytes { buffer in
            guard let space = CGColorSpace(name: CGColorSpace.sRGB),
                  let context = CGContext(
                    data: buffer.baseAddress, width: side, height: side, bitsPerComponent: 8,
                    bytesPerRow: side * 4, space: space,
                    bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return false }
            context.interpolationQuality = .high
            context.draw(image, in: CGRect(x: 0, y: 0, width: side, height: side))
            return true
        }
        guard drawn else { return neutral }

        var counts = [Int](repeating: 0, count: 24)
        var sums = [[Double]](repeating: [0, 0, 0], count: 24)
        for index in 0..<(side * side) {
            let base = index * 4
            let alpha = Double(pixels[base + 3]) / 255
            guard alpha >= 0.5 else { continue }
            let red = min(1, Double(pixels[base]) / 255 / alpha)
            let green = min(1, Double(pixels[base + 1]) / 255 / alpha)
            let blue = min(1, Double(pixels[base + 2]) / 255 / alpha)
            let high = max(red, green, blue)
            let low = min(red, green, blue)
            let lightness = (high + low) / 2
            guard lightness >= 0.1, lightness <= 0.95 else { continue }
            var hue = 0.0
            let delta = high - low
            if delta > 0 {
                if high == red { hue = (green - blue) / delta }
                else if high == green { hue = 2 + (blue - red) / delta }
                else { hue = 4 + (red - green) / delta }
                hue *= 60
                if hue < 0 { hue += 360 }
            }
            let bucket = min(23, Int(hue / 15))
            counts[bucket] += 1
            sums[bucket][0] += red
            sums[bucket][1] += green
            sums[bucket][2] += blue
        }
        guard let best = counts.indices.max(by: { counts[$0] < counts[$1] || (counts[$0] == counts[$1] && $0 > $1) }),
              counts[best] > 0 else { return neutral }
        let total = Double(counts[best])
        let r = Int((sums[best][0] / total * 255).rounded())
        let g = Int((sums[best][1] / total * 255).rounded())
        let b = Int((sums[best][2] / total * 255).rounded())
        return String(format: "#%02lX%02lX%02lX", r, g, b)
    }
}

#if os(iOS)
// Provenance: direct port from baryhuang/claude-code-by-agents at
// 7655042eb986bf146079032726a7347ac4547c0e,
// OpenAgents/Models/Attachment.swift. Kept under Porting/Attachments until
// the CodeRight composer and message transport integration is owned elsewhere.

import Foundation
import ImageIO
import UIKit

/// A file the user has selected (or pasted) in the input bar but hasn't sent yet.
/// Held in composer state until the user hits send, at which point the transport
/// uploads it and references it in the outgoing message via a markdown link.
struct PendingAttachment: Identifiable, Sendable, Equatable {
    let id: UUID
    let filename: String
    let contentType: String
    let data: Data

    init(id: UUID = UUID(), filename: String, contentType: String, data: Data) {
        self.id = id
        self.filename = filename
        self.contentType = contentType
        self.data = data
    }

    var isImage: Bool { contentType.hasPrefix("image/") }
    var size: Int { data.count }

    /// Decode a small thumbnail via ImageIO so we never pay the full-decode
    /// cost of the original bytes (the user may have pasted a multi-megabyte
    /// screenshot).
    func makeThumbnail(maxSide: CGFloat = 40) -> UIImage? {
        guard isImage,
              let cg = Self.makeThumbnailCG(data: data, maxSide: maxSide) else { return nil }
        return UIImage(cgImage: cg)
    }

    private static func makeThumbnailCG(data: Data, maxSide: CGFloat) -> CGImage? {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil) else { return nil }
        let pixelSize = Int(maxSide * UIScreen.main.scale)
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceShouldCacheImmediately: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: pixelSize,
        ]
        return CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary)
    }
}
#endif

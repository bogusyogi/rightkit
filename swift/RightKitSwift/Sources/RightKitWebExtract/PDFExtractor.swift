#if os(iOS)
import Foundation
import PDFKit
import UIKit

enum PDFError: LocalizedError {
    case unreadableDocument
    case emptyDocument
    var errorDescription: String? {
        switch self {
        case .unreadableDocument: return "Could not open the PDF."
        case .emptyDocument: return "The PDF has no pages."
        }
    }
}

/// Desktop parity: text-layer PDFs are extracted directly; scanned /
/// image-only PDFs are rasterized then OCR'd (desktop uses PDFium + PP-OCR;
/// iOS uses PDFKit + Vision).
enum PDFExtractor {
    /// Below this many characters per page on average, the "text layer" is
    /// treated as decorative (page numbers, watermarks) and the document is
    /// rasterized for OCR instead.
    private static let textLayerCharsPerPageThreshold = 24

    /// Longest rasterized edge. ~2.5x mediaBox keeps OCR sharp without
    /// allocating desktop-sized bitmaps on the phone.
    private static let maxRasterEdge: CGFloat = 3500

    static func extract(fileURL: URL) throws -> String {
        guard let document = PDFDocument(url: fileURL) else { throw PDFError.unreadableDocument }
        guard document.pageCount > 0 else { throw PDFError.emptyDocument }

        var pageTexts: [String] = []
        var totalChars = 0
        for index in 0..<document.pageCount {
            let text = document.page(at: index)?.string?
                .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
            totalChars += text.count
            pageTexts.append(text)
        }
        if totalChars >= textLayerCharsPerPageThreshold * document.pageCount {
            return pageTexts.joined(separator: "\n\n")
        }
        return try ocrPages(of: document)
    }

    private static func ocrPages(of document: PDFDocument) throws -> String {
        var pageTexts: [String] = []
        for index in 0..<document.pageCount {
            guard let page = document.page(at: index) else { continue }
            let bounds = page.bounds(for: .mediaBox)
            let scale = min(2.5, maxRasterEdge / max(bounds.width, bounds.height, 1))
            let size = CGSize(width: bounds.width * scale, height: bounds.height * scale)
            let image = page.thumbnail(of: size, for: .mediaBox)
            guard let cgImage = image.cgImage else { continue }
            pageTexts.append(try OCRExtractor.recognizeText(in: cgImage))
        }
        return pageTexts.joined(separator: "\n\n")
    }
}
#endif

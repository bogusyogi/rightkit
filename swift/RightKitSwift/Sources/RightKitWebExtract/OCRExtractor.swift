import Foundation
import ImageIO
import Vision

enum OCRError: LocalizedError {
    case unreadableImage
    var errorDescription: String? { "Could not decode the image." }
}

/// Local OCR. The desktop ships PP-OCRv6 via ONNX Runtime; on iOS the
/// on-device runtime is Apple Vision — zero download, ANE-accelerated, and
/// the same privacy posture (nothing leaves the device). PP-OCR parity on
/// iOS is a benchmark question, not an architecture one: `extract` is the
/// single seam where a ported PP-OCR engine would slot in.
enum OCRExtractor {
    static func extract(fileURL: URL) throws -> String {
        guard let source = CGImageSourceCreateWithURL(fileURL as CFURL, nil),
              let image = CGImageSourceCreateImageAtIndex(source, 0, nil) else {
            throw OCRError.unreadableImage
        }
        return try recognizeText(in: image)
    }

    static func recognizeText(in image: CGImage) throws -> String {
        let request = VNRecognizeTextRequest()
        request.recognitionLevel = .accurate
        request.usesLanguageCorrection = true
        let handler = VNImageRequestHandler(cgImage: image, options: [:])
        try handler.perform([request])
        let lines = (request.results ?? []).compactMap { $0.topCandidates(1).first?.string }
        return lines.joined(separator: "\n")
    }
}

#if os(macOS)
// Apple Vision OCR runner — the counterpart to ScrapeRight's PP-OCRv6 `ocr_test`
// binary, for the bundled-vs-native comparison.
//
// Emits JSON per image: recognized text, wall time, and peak resident memory, so
// the comparison covers the axes that actually matter (accuracy, latency,
// footprint) rather than bundle size.
//
// Vision's revision is pinned explicitly. Leaving it implicit means a macOS
// update silently changes the engine under a recorded benchmark, which would
// make these numbers unreproducible.
//
// Build: swiftc -O vision_ocr.swift -o vision_ocr
// Run:   ./vision_ocr --images <dir-or-file> [--level accurate|fast] [--out r.json]

import AppKit
import Foundation
import Vision

struct Result: Codable {
    let image: String
    let text: String
    let lines: Int
    let ms: Double
    let meanConfidence: Double
}

func fail(_ m: String) -> Never {
    FileHandle.standardError.write(Data("vision_ocr: \(m)\n".utf8))
    exit(1)
}

func peakResidentMB() -> Double {
    var info = rusage()
    getrusage(RUSAGE_SELF, &info)
    // ru_maxrss is BYTES on Darwin (kilobytes on Linux).
    return Double(info.ru_maxrss) / 1_048_576.0
}

// --- args -------------------------------------------------------------------
var imagesArg: String?
var out: String?
var level = "accurate"
var langCorrection = true
var i = 1
let a = CommandLine.arguments
while i < a.count {
    switch a[i] {
    case "--images": i += 1; imagesArg = i < a.count ? a[i] : nil
    case "--out": i += 1; out = i < a.count ? a[i] : nil
    case "--level": i += 1; level = i < a.count ? a[i] : level
    case "--no-language-correction": langCorrection = false
    default: fail("unknown argument \(a[i])")
    }
    i += 1
}
guard let imagesArg else {
    fail("usage: vision_ocr --images <dir|file> [--level accurate|fast] [--out r.json] [--no-language-correction]")
}

let fm = FileManager.default
var isDir: ObjCBool = false
guard fm.fileExists(atPath: imagesArg, isDirectory: &isDir) else { fail("no such path \(imagesArg)") }
var files: [String]
if isDir.boolValue {
    files = (try? fm.contentsOfDirectory(atPath: imagesArg))?
        .filter { ["png", "jpg", "jpeg", "tiff"].contains(($0 as NSString).pathExtension.lowercased()) }
        .sorted()
        .map { (imagesArg as NSString).appendingPathComponent($0) } ?? []
} else {
    files = [imagesArg]
}
guard !files.isEmpty else { fail("no images found in \(imagesArg)") }

let recognitionLevel: VNRequestTextRecognitionLevel = (level == "fast") ? .fast : .accurate

// --- run --------------------------------------------------------------------
var results: [Result] = []
for path in files {
    guard let image = NSImage(contentsOfFile: path),
          let cg = image.cgImage(forProposedRect: nil, context: nil, hints: nil)
    else {
        FileHandle.standardError.write(Data("  skip (unreadable): \(path)\n".utf8))
        continue
    }

    let request = VNRecognizeTextRequest()
    request.recognitionLevel = recognitionLevel
    request.usesLanguageCorrection = langCorrection
    request.revision = VNRecognizeTextRequestRevision3

    let t0 = Date()
    let handler = VNImageRequestHandler(cgImage: cg, options: [:])
    do {
        try handler.perform([request])
    } catch {
        FileHandle.standardError.write(Data("  FAILED \(path): \(error)\n".utf8))
        continue
    }
    let ms = Date().timeIntervalSince(t0) * 1000

    let observations = request.results ?? []
    let lines = observations.compactMap { $0.topCandidates(1).first }
    let text = lines.map(\.string).joined(separator: "\n")
    let conf = lines.isEmpty ? 0 : Double(lines.map { Double($0.confidence) }.reduce(0, +)) / Double(lines.count)

    results.append(
        Result(
            image: (path as NSString).lastPathComponent,
            text: text,
            lines: lines.count,
            ms: ms,
            meanConfidence: conf
        ))
    FileHandle.standardError.write(
        Data("  \((path as NSString).lastPathComponent)  \(String(format: "%.0f", ms))ms  \(lines.count) lines\n".utf8))
}

let totalMs = results.map(\.ms).reduce(0, +)
FileHandle.standardError.write(
    Data(
        "vision_ocr: \(results.count) images, \(String(format: "%.0f", totalMs))ms total, peak RSS \(String(format: "%.0f", peakResidentMB()))MB\n"
            .utf8))

let encoder = JSONEncoder()
encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
let payload = try encoder.encode(results)
if let out {
    try payload.write(to: URL(fileURLWithPath: out))
    FileHandle.standardError.write(Data("vision_ocr: wrote \(out)\n".utf8))
} else {
    FileHandle.standardOutput.write(payload)
}
#endif

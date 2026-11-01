// PDFKit benchmark — text extraction and page rendering.
//
// Two consumers care about different halves of this:
//   * ScrapeRight replaces libpdfium + the pdf-extract crate for one-way TEXT
//     EXTRACTION (no UI, no round-trip).
//   * ViewRight would replace pdf.js for RENDERING, so per-page raster time and
//     memory are what matter there.
//
// Reports both, plus peak RSS, because the decision axis is resource usage and
// latency rather than bundle size.
//
// Build: swiftc -O pdfkit_bench.swift -o pdfkit_bench
// Run:   ./pdfkit_bench --pdfs <dir> [--render-dpi 150] [--out r.json]

import Foundation
import PDFKit

struct DocResult: Codable {
    let file: String
    let pages: Int
    let chars: Int
    let extractMs: Double
    let renderMsPerPage: Double
    let renderedPages: Int
}

func fail(_ m: String) -> Never {
    FileHandle.standardError.write(Data("pdfkit_bench: \(m)\n".utf8))
    exit(1)
}

func peakResidentMB() -> Double {
    var info = rusage()
    getrusage(RUSAGE_SELF, &info)
    return Double(info.ru_maxrss) / 1_048_576.0  // bytes on Darwin
}

var pdfsArg: String?
var out: String?
var renderDPI: Double = 150
var maxRenderPages = 5
var i = 1
let a = CommandLine.arguments
while i < a.count {
    switch a[i] {
    case "--pdfs": i += 1; pdfsArg = i < a.count ? a[i] : nil
    case "--out": i += 1; out = i < a.count ? a[i] : nil
    case "--render-dpi": i += 1; renderDPI = i < a.count ? (Double(a[i]) ?? 150) : 150
    case "--max-render-pages": i += 1; maxRenderPages = i < a.count ? (Int(a[i]) ?? 5) : 5
    default: fail("unknown argument \(a[i])")
    }
    i += 1
}
guard let pdfsArg else { fail("usage: pdfkit_bench --pdfs <dir> [--render-dpi 150] [--out r.json]") }

let fm = FileManager.default
var files: [String] = []
var isDir: ObjCBool = false
if fm.fileExists(atPath: pdfsArg, isDirectory: &isDir), isDir.boolValue {
    files = (try? fm.contentsOfDirectory(atPath: pdfsArg))?
        .filter { ($0 as NSString).pathExtension.lowercased() == "pdf" }
        .sorted()
        .map { (pdfsArg as NSString).appendingPathComponent($0) } ?? []
} else {
    files = [pdfsArg]
}
guard !files.isEmpty else { fail("no PDFs at \(pdfsArg)") }

var results: [DocResult] = []
for path in files {
    guard let doc = PDFDocument(url: URL(fileURLWithPath: path)) else {
        FileHandle.standardError.write(Data("  skip (unreadable): \(path)\n".utf8))
        continue
    }

    // --- text extraction (the ScrapeRight job) ---
    let t0 = Date()
    let text = doc.string ?? ""
    let extractMs = Date().timeIntervalSince(t0) * 1000

    // --- page rendering (the ViewRight job) ---
    let scale = renderDPI / 72.0
    let pagesToRender = min(doc.pageCount, maxRenderPages)
    let t1 = Date()
    for p in 0..<pagesToRender {
        guard let page = doc.page(at: p) else { continue }
        let bounds = page.bounds(for: .mediaBox)
        let size = NSSize(width: bounds.width * scale, height: bounds.height * scale)
        // thumbnail(of:for:) is the same path a viewer uses to raster a page.
        _ = page.thumbnail(of: size, for: .mediaBox)
    }
    let renderMs = Date().timeIntervalSince(t1) * 1000

    results.append(
        DocResult(
            file: (path as NSString).lastPathComponent,
            pages: doc.pageCount,
            chars: text.count,
            extractMs: extractMs,
            renderMsPerPage: pagesToRender > 0 ? renderMs / Double(pagesToRender) : 0,
            renderedPages: pagesToRender
        ))
    FileHandle.standardError.write(
        Data(
            "  \((path as NSString).lastPathComponent)  \(doc.pageCount)p  extract \(String(format: "%.1f", extractMs))ms  render \(String(format: "%.0f", renderMs / Double(max(pagesToRender, 1))))ms/pg\n"
                .utf8))
}

let totalExtract = results.map(\.extractMs).reduce(0, +)
let meanRender = results.isEmpty ? 0 : results.map(\.renderMsPerPage).reduce(0, +) / Double(results.count)
FileHandle.standardError.write(
    Data(
        "pdfkit_bench: \(results.count) docs, extract \(String(format: "%.0f", totalExtract))ms total, render mean \(String(format: "%.0f", meanRender))ms/pg @ \(Int(renderDPI))dpi, peak RSS \(String(format: "%.0f", peakResidentMB()))MB\n"
            .utf8))

let encoder = JSONEncoder()
encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
let payload = try encoder.encode(results)
if let out {
    try payload.write(to: URL(fileURLWithPath: out))
} else {
    FileHandle.standardOutput.write(payload)
}

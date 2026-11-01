// Render one specific page repeatedly to compare against pdf.js's per-page timing.
import Foundation
import PDFKit
let path = CommandLine.arguments[1]
let pageIndex = Int(CommandLine.arguments[2])! - 1
guard let doc = PDFDocument(url: URL(fileURLWithPath: path)), let page = doc.page(at: pageIndex) else {
    FileHandle.standardError.write(Data("cannot open\n".utf8)); exit(1)
}
let bounds = page.bounds(for: .mediaBox)
let scale = 150.0 / 72.0
let size = NSSize(width: bounds.width * scale, height: bounds.height * scale)
for run in 1...3 {
    let t0 = Date()
    _ = page.thumbnail(of: size, for: .mediaBox)
    print("run \(run): \(String(format: "%.0f", Date().timeIntervalSince(t0) * 1000))ms")
}

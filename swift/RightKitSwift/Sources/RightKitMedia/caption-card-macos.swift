#if os(macOS)
import AppKit
import Foundation

struct Request: Decodable {
  let outputPath: String
  let width: Int
  let height: Int
  let text: String
  let vertical: Bool
}

// swiftlint:disable:next function_body_length
func render(_ request: Request) throws {
  guard request.width > 0, request.height > 0 else {
    throw NSError(domain: "CutRight", code: 1, userInfo: [NSLocalizedDescriptionKey: "invalid caption size"])
  }
  let size = NSSize(width: request.width, height: request.height)
  guard let bitmap = NSBitmapImageRep(
    bitmapDataPlanes: nil,
    pixelsWide: request.width,
    pixelsHigh: request.height,
    bitsPerSample: 8,
    samplesPerPixel: 4,
    hasAlpha: true,
    isPlanar: false,
    colorSpaceName: .deviceRGB,
    bitmapFormat: .alphaFirst,
    bytesPerRow: 0,
    bitsPerPixel: 0
  ), let context = NSGraphicsContext(bitmapImageRep: bitmap) else {
    throw NSError(domain: "CutRight", code: 2, userInfo: [NSLocalizedDescriptionKey: "cannot create caption bitmap"])
  }
  NSGraphicsContext.saveGraphicsState()
  NSGraphicsContext.current = context
  defer { NSGraphicsContext.restoreGraphicsState() }
  NSColor.clear.setFill()
  NSRect(origin: .zero, size: size).fill()

  let fontSize = max(28, CGFloat(request.height) * 0.045)
  let attributes: [NSAttributedString.Key: Any] = [
    .font: NSFont.systemFont(ofSize: fontSize, weight: .bold),
    .foregroundColor: NSColor.white
  ]
  let maximumTextWidth = CGFloat(request.width) * 0.78
  let textSize = (request.text as NSString).boundingRect(
    with: NSSize(width: maximumTextWidth, height: .greatestFiniteMagnitude),
    options: [.usesLineFragmentOrigin, .usesFontLeading],
    attributes: attributes
  ).integral.size
  let horizontalPadding = fontSize * 0.7
  let verticalPadding = fontSize * 0.42
  let boxSize = NSSize(
    width: min(CGFloat(request.width) * 0.86, textSize.width + horizontalPadding * 2),
    height: textSize.height + verticalPadding * 2
  )
  let verticalPosition = request.vertical ? CGFloat(request.height) * 0.48 : CGFloat(request.height) * 0.10
  let box = NSRect(
    x: (CGFloat(request.width) - boxSize.width) / 2,
    y: min(verticalPosition, CGFloat(request.height) - boxSize.height - verticalPadding),
    width: boxSize.width,
    height: boxSize.height
  )
  NSColor.black.withAlphaComponent(0.72).setFill()
  NSBezierPath(roundedRect: box, xRadius: fontSize * 0.28, yRadius: fontSize * 0.28).fill()
  let textRect = NSRect(
    x: box.minX + horizontalPadding,
    y: box.minY + verticalPadding,
    width: box.width - horizontalPadding * 2,
    height: textSize.height
  )
  (request.text as NSString).draw(
    with: textRect,
    options: [.usesLineFragmentOrigin, .usesFontLeading],
    attributes: attributes
  )
  guard let png = bitmap.representation(using: .png, properties: [:]) else {
    throw NSError(domain: "CutRight", code: 3, userInfo: [NSLocalizedDescriptionKey: "cannot encode caption image"])
  }
  try FileManager.default.createDirectory(
    at: URL(fileURLWithPath: request.outputPath).deletingLastPathComponent(),
    withIntermediateDirectories: true
  )
  try png.write(to: URL(fileURLWithPath: request.outputPath))
}

do {
  let data = try FileHandle.standardInput.readToEnd() ?? Data()
  let decoder = JSONDecoder()
  decoder.keyDecodingStrategy = .convertFromSnakeCase
  try render(decoder.decode(Request.self, from: data))
} catch {
  fputs("caption-card-macos: \(error.localizedDescription)\n", stderr)
  exit(2)
}
#endif

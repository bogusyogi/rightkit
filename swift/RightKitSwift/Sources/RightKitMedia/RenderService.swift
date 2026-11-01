#if os(macOS)
import AppKit
import CoreImage
import Foundation
import Metal

struct MacCaptionRenderRequest: Codable {
  let outputPath: String
  let width: Int
  let height: Int
  let text: String
  let vertical: Bool
  let allowedRoots: [String]
}

struct MacPreviewRenderRequest: Codable {
  let inputPath: String
  let outputPath: String
  let cropX: Double?
  let cropY: Double?
  let cropWidth: Double?
  let cropHeight: Double?
  let rotationDegrees: Double?
  let allowedRoots: [String]
}

struct MacRenderArtifact: Codable {
  let outputPath: String
  let width: Int
  let height: Int
  let colorSpace: String
  let renderer: String
}

final class RenderService {
  private let device: MTLDevice?
  private let context: CIContext?

  init() {
    device = MTLCreateSystemDefaultDevice()
    context = device.map { CIContext(mtlDevice: $0, options: [.cacheIntermediates: true]) }
  }

  var supportsPreview: Bool { context != nil }

  func renderCaption(_ request: MacCaptionRenderRequest) throws -> MacRenderArtifact {
    guard request.width > 0, request.height > 0, !request.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
      throw MacMediaServiceError.render("invalid caption request")
    }
    let output = try validatedOutputPath(request.outputPath, allowedRoots: request.allowedRoots)
    let size = NSSize(width: request.width, height: request.height)
    guard let bitmap = NSBitmapImageRep(
      bitmapDataPlanes: nil, pixelsWide: request.width, pixelsHigh: request.height,
      bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
      colorSpaceName: .deviceRGB, bitmapFormat: .alphaFirst, bytesPerRow: 0, bitsPerPixel: 0
    ), let graphics = NSGraphicsContext(bitmapImageRep: bitmap) else {
      throw MacMediaServiceError.render("cannot create caption bitmap")
    }
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = graphics
    defer { NSGraphicsContext.restoreGraphicsState() }
    NSColor.clear.setFill()
    NSRect(origin: .zero, size: size).fill()
    let fontSize = max(28, CGFloat(request.height) * 0.045)
    let attributes: [NSAttributedString.Key: Any] = [
      .font: NSFont.systemFont(ofSize: fontSize, weight: .bold), .foregroundColor: NSColor.white
    ]
    let maximumWidth = CGFloat(request.width) * 0.78
    let textSize = (request.text as NSString).boundingRect(
      with: NSSize(width: maximumWidth, height: .greatestFiniteMagnitude),
      options: [.usesLineFragmentOrigin, .usesFontLeading], attributes: attributes
    ).integral.size
    let horizontalPadding = fontSize * 0.7
    let verticalPadding = fontSize * 0.42
    let boxSize = NSSize(
      width: min(CGFloat(request.width) * 0.86, textSize.width + horizontalPadding * 2),
      height: textSize.height + verticalPadding * 2
    )
    let preferredY = request.vertical ? CGFloat(request.height) * 0.48 : CGFloat(request.height) * 0.10
    let box = NSRect(
      x: (CGFloat(request.width) - boxSize.width) / 2,
      y: min(preferredY, CGFloat(request.height) - boxSize.height - verticalPadding),
      width: boxSize.width, height: boxSize.height
    )
    NSColor.black.withAlphaComponent(0.72).setFill()
    NSBezierPath(roundedRect: box, xRadius: fontSize * 0.28, yRadius: fontSize * 0.28).fill()
    (request.text as NSString).draw(
      with: NSRect(x: box.minX + horizontalPadding, y: box.minY + verticalPadding,
                   width: box.width - horizontalPadding * 2, height: textSize.height),
      options: [.usesLineFragmentOrigin, .usesFontLeading], attributes: attributes
    )
    guard let png = bitmap.representation(using: NSBitmapImageRep.FileType.png, properties: [:]) else {
      throw MacMediaServiceError.render("cannot encode caption PNG")
    }
    try png.write(to: output, options: Data.WritingOptions.atomic)
    return MacRenderArtifact(outputPath: output.path, width: request.width, height: request.height, colorSpace: "sRGB", renderer: "AppKit")
  }

  /// Preview-only image transform. Final delivery never reaches this method.
  func renderPreview(_ request: MacPreviewRenderRequest) throws -> MacRenderArtifact {
    guard let context else { throw MacMediaServiceError.unsupported("Metal/Core Image preview unavailable") }
    let input = try validateMacMediaFilePath(request.inputPath, allowedRoots: request.allowedRoots)
    let output = try validatedOutputPath(request.outputPath, allowedRoots: request.allowedRoots)
    guard var image = CIImage(contentsOf: input) else { throw MacMediaServiceError.render("cannot decode preview input") }
    let extent = image.extent
    if let width = request.cropWidth, let height = request.cropHeight, width > 0, height > 0 {
      image = image.cropped(to: CGRect(x: request.cropX ?? extent.minX, y: request.cropY ?? extent.minY, width: width, height: height))
    }
    if let degrees = request.rotationDegrees, degrees != 0 {
      image = image.transformed(by: CGAffineTransform(rotationAngle: CGFloat(degrees * .pi / 180)))
    }
    let colorSpace = CGColorSpace(name: CGColorSpace.sRGB)!
    do {
      try context.writePNGRepresentation(of: image, to: output, format: .RGBA8, colorSpace: colorSpace)
    } catch {
      throw MacMediaServiceError.render("cannot encode preview PNG: \(error.localizedDescription)")
    }
    return MacRenderArtifact(outputPath: output.path, width: Int(image.extent.width), height: Int(image.extent.height), colorSpace: "sRGB", renderer: "CoreImage/Metal")
  }

  private func validatedOutputPath(_ path: String, allowedRoots: [String]) throws -> URL {
    guard path.hasPrefix("/") else { throw MacMediaServiceError.invalidPath(path) }
    let output = URL(fileURLWithPath: path).resolvingSymlinksInPath().standardizedFileURL
    let parent = output.deletingLastPathComponent()
    let values = try parent.resourceValues(forKeys: [.isDirectoryKey])
    guard values.isDirectory == true else { throw MacMediaServiceError.invalidPath(path) }
    let permitted = allowedRoots.contains { root in
      let canonical = URL(fileURLWithPath: root).resolvingSymlinksInPath().standardizedFileURL.path
      return parent.path == canonical || parent.path.hasPrefix(canonical + "/")
    }
    guard permitted else { throw MacMediaServiceError.pathOutsideAllowedRoots(path) }
    return output
  }
}
#endif

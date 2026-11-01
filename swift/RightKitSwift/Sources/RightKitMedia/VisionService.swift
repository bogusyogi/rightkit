#if os(macOS)
import Foundation
import ImageIO
import Vision

struct MacVisionFrameRequest: Codable {
  let sourcePath: String
  let sourceFrameIndex: Int
  let timestamp: MacMediaRationalTime
  let sequenceId: String?
  let orientation: String?
}

struct MacVisionBox: Codable {
  let centerX: Double
  let centerY: Double
  let area: Double
  let confidence: Double
}

struct MacVisionOcrBox: Codable {
  let x0: Double
  let y0: Double
  let x1: Double
  let y1: Double
  let confidence: Double
}

struct MacVisionFrameObservation: Codable {
  let sourceFrameIndex: Int
  let timestamp: MacMediaRationalTime
  let orientationTransform: String
  let visionRevision: Int
  let faces: [MacVisionBox]
  let bodies: [MacVisionBox]
  let ocrBoxes: [MacVisionOcrBox]
  let saliency: MacVisionBox?
}

/// Vision batches execute in input order. Sequence handlers are retained only
/// for a continuous source sequence; a changed sequence id resets state.
final class VisionService {
  private var activeSequenceId: String?
  private var sequenceHandler = VNSequenceRequestHandler()

  func analyzeFrames(_ requests: [MacVisionFrameRequest], allowedRoots: [String]) throws -> [MacVisionFrameObservation] {
    try requests.map { request in
      let image = try validateMacMediaFilePath(request.sourcePath, allowedRoots: allowedRoots)
      if request.sequenceId != activeSequenceId {
        activeSequenceId = request.sequenceId
        sequenceHandler = VNSequenceRequestHandler()
      }
      return try analyzeFrame(request, imageURL: image)
    }
  }

  private func analyzeFrame(_ request: MacVisionFrameRequest, imageURL: URL) throws -> MacVisionFrameObservation {
    let face = VNDetectFaceRectanglesRequest()
    let body = VNDetectHumanRectanglesRequest()
    let text = VNRecognizeTextRequest()
    text.recognitionLevel = .fast
    text.usesLanguageCorrection = false
    let saliency = VNGenerateAttentionBasedSaliencyImageRequest()
    guard let source = CGImageSourceCreateWithURL(imageURL as CFURL, nil),
          let image = CGImageSourceCreateImageAtIndex(source, 0, nil) else {
      throw MacMediaServiceError.assetLoad("cannot decode Vision input")
    }
    do {
      try sequenceHandler.perform(
        [face, body, text, saliency],
        on: image,
        orientation: imageOrientation(request.orientation)
      )
    } catch {
      throw MacMediaServiceError.assetLoad(error.localizedDescription)
    }
    return MacVisionFrameObservation(
      sourceFrameIndex: request.sourceFrameIndex,
      timestamp: request.timestamp,
      orientationTransform: request.orientation ?? "up",
      visionRevision: face.revision,
      faces: boxes(face.results ?? []),
      bodies: boxes(body.results ?? []),
      ocrBoxes: ocrBoxes(text.results ?? []),
      saliency: saliencyBox(saliency)
    )
  }

  private func imageOrientation(_ value: String?) -> CGImagePropertyOrientation {
    switch value {
    case "down": return .down
    case "left": return .left
    case "right": return .right
    case "upMirrored": return .upMirrored
    case "downMirrored": return .downMirrored
    case "leftMirrored": return .leftMirrored
    case "rightMirrored": return .rightMirrored
    default: return .up
    }
  }

  private func boxes(_ observations: [VNDetectedObjectObservation]) -> [MacVisionBox] {
    observations.sorted { lhs, rhs in lhs.boundingBox.width * lhs.boundingBox.height > rhs.boundingBox.width * rhs.boundingBox.height }
      .prefix(4)
      .map(box)
  }

  private func box(_ observation: VNDetectedObjectObservation) -> MacVisionBox {
    let rect = observation.boundingBox
    return MacVisionBox(centerX: Double(rect.midX), centerY: Double(1 - rect.midY), area: Double(rect.width * rect.height), confidence: Double(observation.confidence))
  }

  private func ocrBoxes(_ observations: [VNRecognizedTextObservation]) -> [MacVisionOcrBox] {
    observations.prefix(8).compactMap { observation in
      guard let candidate = observation.topCandidates(1).first else { return nil }
      let rect = observation.boundingBox
      return MacVisionOcrBox(x0: Double(rect.minX), y0: Double(1 - rect.maxY), x1: Double(rect.maxX), y1: Double(1 - rect.minY), confidence: Double(candidate.confidence))
    }
  }

  private func saliencyBox(_ request: VNGenerateAttentionBasedSaliencyImageRequest) -> MacVisionBox? {
    guard let observation = request.results?.first?.salientObjects?.max(by: { $0.confidence < $1.confidence }) else { return nil }
    return box(observation)
  }
}
#endif

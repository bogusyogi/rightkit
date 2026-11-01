#if os(macOS)
import Foundation
import Vision

// REV2 plan §15.5 (Phase 7 — temporal visual perception and reframing):
// this worker used to answer one question ("where is the biggest face in
// this single frame?"). It now answers one *frame's* full multi-modal
// evidence in a single call, because the Rust caller (`reframe.rs` /
// `reframe_track.rs`) samples many frames per segment instead of one
// midpoint and needs every modality from that same frame, not one process
// spawn per modality per sample.
//
// Every detection is reported in NORMALIZED coordinates: x in [0,1] left to
// right, y in [0,1] TOP to bottom (Vision reports bottom-left-origin
// normalized rects; every y here is already flipped to top-left-origin to
// match the rest of the pipeline's convention, same as the original
// face-only worker).

struct Request: Decodable {
  let imagePath: String
}

struct DetectionBox: Encodable {
  let centerX: Double
  let centerY: Double
  let area: Double
  let confidence: Double
}

struct OcrBox: Encodable {
  let x0: Double
  let y0: Double
  let x1: Double
  let y1: Double
  let confidence: Double
}

struct Response: Encodable {
  // Ranked largest-first; empty means none detected (a gap for that
  // modality, not a zero — the Rust fusion layer treats an empty array as
  // "no evidence" rather than "centered at 0.5,0.5").
  let faces: [DetectionBox]
  let bodies: [DetectionBox]
  let ocrBoxes: [OcrBox]
  let saliency: DetectionBox?
}

func normalizedBox(_ box: CGRect) -> (centerX: Double, centerY: Double, area: Double) {
  (Double(box.midX), Double(1 - box.midY), Double(box.width * box.height))
}

func detect(_ request: Request) throws -> Response {
  let url = URL(fileURLWithPath: request.imagePath)
  let handler = VNImageRequestHandler(url: url)

  let faceRequest = VNDetectFaceRectanglesRequest()
  let bodyRequest = VNDetectHumanRectanglesRequest()
  let textRequest = VNRecognizeTextRequest()
  textRequest.recognitionLevel = .fast
  textRequest.usesLanguageCorrection = false
  let saliencyRequest = VNGenerateAttentionBasedSaliencyImageRequest()

  // One handler.perform call runs every request against the same decoded
  // frame — one process spawn, one image decode, five detectors.
  try handler.perform([faceRequest, bodyRequest, textRequest, saliencyRequest])

  let faces = (faceRequest.results ?? [])
    .sorted { $0.boundingBox.width * $0.boundingBox.height > $1.boundingBox.width * $1.boundingBox.height }
    .prefix(4)
    .map { face -> DetectionBox in
      let (cx, cy, area) = normalizedBox(face.boundingBox)
      return DetectionBox(centerX: cx, centerY: cy, area: area, confidence: Double(face.confidence))
    }

  let bodies = (bodyRequest.results ?? [])
    .sorted { $0.boundingBox.width * $0.boundingBox.height > $1.boundingBox.width * $1.boundingBox.height }
    .prefix(4)
    .map { body -> DetectionBox in
      let (cx, cy, area) = normalizedBox(body.boundingBox)
      return DetectionBox(centerX: cx, centerY: cy, area: area, confidence: Double(body.confidence))
    }

  let ocrBoxes = (textRequest.results ?? [])
    .prefix(8)
    .compactMap { observation -> OcrBox? in
      guard let candidate = observation.topCandidates(1).first else { return nil }
      let box = observation.boundingBox
      return OcrBox(
        x0: Double(box.minX),
        y0: Double(1 - box.maxY),
        x1: Double(box.maxX),
        y1: Double(1 - box.minY),
        confidence: Double(candidate.confidence)
      )
    }

  var saliency: DetectionBox? = nil
  if let saliencyResult = saliencyRequest.results?.first,
     let salientObject = saliencyResult.salientObjects?.max(by: { $0.confidence < $1.confidence }) {
    let (cx, cy, area) = normalizedBox(salientObject.boundingBox)
    saliency = DetectionBox(centerX: cx, centerY: cy, area: area, confidence: Double(salientObject.confidence))
  }

  return Response(faces: Array(faces), bodies: Array(bodies), ocrBoxes: Array(ocrBoxes), saliency: saliency)
}

do {
  let data = try FileHandle.standardInput.readToEnd() ?? Data()
  let decoder = JSONDecoder()
  decoder.keyDecodingStrategy = .convertFromSnakeCase
  let response = try detect(decoder.decode(Request.self, from: data))
  let encoder = JSONEncoder()
  encoder.keyEncodingStrategy = .convertToSnakeCase
  FileHandle.standardOutput.write(try encoder.encode(response))
} catch {
  fputs("vision-anchor-macos: \(error.localizedDescription)\n", stderr)
  exit(2)
}
#endif

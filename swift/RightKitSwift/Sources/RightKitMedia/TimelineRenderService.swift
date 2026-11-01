#if os(macOS)
import AVFoundation
import CryptoKit
import Foundation
import CoreImage

private struct TimelineCueMix {
  let id: String
  let samples: [Float]
  let startFrame: Int
  let transientOffsetMs: Int
}

private final class TimelineVisualAsset {
  let still: CIImage?
  let generator: AVAssetImageGenerator?
  init(url: URL) {
    still = CIImage(contentsOf: url)
    if still == nil {
      let generator = AVAssetImageGenerator(asset: AVURLAsset(url: url))
      generator.appliesPreferredTrackTransform = true
      generator.requestedTimeToleranceBefore = .zero
      generator.requestedTimeToleranceAfter = .zero
      self.generator = generator
    } else {
      generator = nil
    }
  }
  func image(at seconds: Double) -> CIImage? {
    if let still { return still }
    guard let generator, let cg = try? generator.copyCGImage(at: CMTime(seconds: max(0, seconds), preferredTimescale: 600), actualTime: nil) else { return nil }
    return CIImage(cgImage: cg)
  }
}

/// Bounded AVAssetReader/Writer timeline executor. Rust owns graph semantics;
/// this service owns clocks, framework decoding, and atomic artifact writes.
final class TimelineRenderService {
  private let maxNodes = 4096
  private let maxDuration = 60.0 * 60.0 * 4.0
  private let maxOutputBytes: UInt64 = 16 * 1024 * 1024 * 1024
  private let audioService = AudioFinishService()
  private let typographyService = TypographyService()

  func render(_ request: TimelineRenderRequest) throws -> TimelineRenderResult {
    guard request.schemaVersion == 1,
          request.lockedCutSha256.count == 64,
          request.lockedCutSha256.allSatisfy({ $0.isHexDigit }) else {
      throw MacMediaServiceError.render("invalid locked timeline request")
    }
    guard request.mode == "native" || request.mode == "shadow" else {
      throw MacMediaServiceError.unsupported("legacy timeline mode is owned by FFmpeg")
    }
    guard request.video.width > 0, request.video.height > 0,
          request.video.width <= 8192, request.video.height <= 8192,
          request.video.frameRateNum > 0, request.video.frameRateDen > 0,
          UInt64(request.video.frameRateNum) <= 240 * UInt64(request.video.frameRateDen),
          request.audio.sampleRate > 0, request.audio.sampleRate <= 192_000,
          request.audio.channels > 0, request.audio.channels <= 8 else {
      throw MacMediaServiceError.render("timeline output exceeds bounds")
    }
    let graphObject = try validatedGraph(request.graph)
    let output = try validatedOutput(request.outputPath, roots: request.allowedRoots)
    let temporary = output.deletingLastPathComponent().appendingPathComponent(".\(output.lastPathComponent).tmp.mp4")
    try? FileManager.default.removeItem(at: temporary)
    defer { try? FileManager.default.removeItem(at: temporary) }

    // The graph's source is supplied as its first input path by Rust. Keeping
    // this lookup strict avoids worker-side filesystem discovery.
    guard let sourceValue = graphObject["sourcePath"], let source = sourceValue.stringValue else {
      throw MacMediaServiceError.render("render graph has no sourcePath")
    }
    let sourceURL = try validateMacMediaFilePath(source, allowedRoots: request.allowedRoots)
    if case let .object(assets) = graphObject["assets"] {
      for value in assets.values {
        guard let path = value.stringValue else { throw MacMediaServiceError.render("asset path must be a string") }
        _ = try validateMacMediaFilePath(path, allowedRoots: request.allowedRoots)
      }
    }
    let asset = AVURLAsset(url: sourceURL)
    guard let graphDuration = rationalSeconds(graphObject["duration"]) else {
      throw MacMediaServiceError.render("render graph has no duration")
    }
    let cueMixes = try graphAudioCues(
      request.graph,
      allowedRoots: request.allowedRoots,
      sampleRate: Int(request.audio.sampleRate),
      channels: Int(request.audio.channels)
    )
    let visualAssets = try graphVisualAssets(request.graph, allowedRoots: request.allowedRoots)
    let reader = try AVAssetReader(asset: asset)
    reader.timeRange = CMTimeRange(
      start: .zero,
      duration: CMTime(seconds: graphDuration, preferredTimescale: 600)
    )
    let writer = try AVAssetWriter(outputURL: temporary, fileType: .mp4)
    writer.metadata = fixedCreationMetadata()
    guard let videoTrack = asset.tracks(withMediaType: .video).first else {
      throw MacMediaServiceError.unsupported("source has no video track")
    }
    let videoOutput = AVAssetReaderTrackOutput(track: videoTrack, outputSettings: [
      kCVPixelBufferPixelFormatTypeKey as String: Int(kCVPixelFormatType_32BGRA)
    ])
    let videoInput = AVAssetWriterInput(mediaType: .video, outputSettings: [
      AVVideoCodecKey: AVVideoCodecType.h264,
      AVVideoWidthKey: Int(request.video.width), AVVideoHeightKey: Int(request.video.height),
      AVVideoCompressionPropertiesKey: [
        AVVideoAverageBitRateKey: 2_000_000,
        AVVideoExpectedSourceFrameRateKey: Int(request.video.frameRateNum / request.video.frameRateDen),
        AVVideoMaxKeyFrameIntervalKey: Int(request.video.frameRateNum / request.video.frameRateDen),
        AVVideoAllowFrameReorderingKey: false,
        AVVideoProfileLevelKey: AVVideoProfileLevelH264HighAutoLevel
      ]
    ])
    videoInput.metadata = fixedCreationMetadata()
    let adaptor = AVAssetWriterInputPixelBufferAdaptor(assetWriterInput: videoInput, sourcePixelBufferAttributes: [
      kCVPixelBufferPixelFormatTypeKey as String: Int(kCVPixelFormatType_32BGRA),
      kCVPixelBufferWidthKey as String: Int(request.video.width),
      kCVPixelBufferHeightKey as String: Int(request.video.height)
    ])
    let ciContext = CIContext(options: [.useSoftwareRenderer: false])
    videoInput.expectsMediaDataInRealTime = false
    guard reader.canAdd(videoOutput), writer.canAdd(videoInput) else { throw MacMediaServiceError.render("cannot configure video reader/writer") }
    reader.add(videoOutput); writer.add(videoInput)
    var audioInput: AVAssetWriterInput?
    var audioOutput: AVAssetReaderTrackOutput?
    var decodedAudioFrames = 0
    var transientFrame = 0
    let splitFrame = graphAudioSplitFrame(request.graph, sampleRate: Int(request.audio.sampleRate))
    let tailDelay = max(Int(request.audio.channels), Int(Double(request.audio.sampleRate) * 0.055) * Int(request.audio.channels))
    var tailHistory = Array(repeating: Float.zero, count: tailDelay)
    var tailIndex = 0
    if let audioTrack = asset.tracks(withMediaType: .audio).first {
      let output = AVAssetReaderTrackOutput(track: audioTrack, outputSettings: [
        AVFormatIDKey: kAudioFormatLinearPCM, AVSampleRateKey: Int(request.audio.sampleRate),
        AVNumberOfChannelsKey: Int(request.audio.channels), AVLinearPCMBitDepthKey: 32,
        AVLinearPCMIsFloatKey: true, AVLinearPCMIsBigEndianKey: false,
        AVLinearPCMIsNonInterleaved: false
      ])
      let input = AVAssetWriterInput(mediaType: .audio, outputSettings: [AVFormatIDKey: kAudioFormatMPEG4AAC, AVSampleRateKey: Int(request.audio.sampleRate), AVNumberOfChannelsKey: Int(request.audio.channels)])
      input.metadata = fixedCreationMetadata()
      if reader.canAdd(output), writer.canAdd(input) { reader.add(output); writer.add(input); audioInput = input; audioOutput = output }
    }
    guard writer.startWriting() else { throw MacMediaServiceError.render(writer.error?.localizedDescription ?? "writer did not start") }
    writer.startSession(atSourceTime: .zero)
    guard reader.startReading() else { throw MacMediaServiceError.render(reader.error?.localizedDescription ?? "reader did not start") }
    var renderedFrames: UInt64 = 0
    var pixelHash = SHA256()
    var audioHash = SHA256()
    var videoDone = false
    var audioDone = audioInput == nil || audioOutput == nil
    var lastProgress = Date()
    while !videoDone || !audioDone {
      if writer.status == .failed || writer.status == .cancelled {
        throw MacMediaServiceError.render(writer.error?.localizedDescription ?? "writer stopped")
      }
      var progressed = false
      if !videoDone, videoInput.isReadyForMoreMediaData {
        if let sample = videoOutput.copyNextSampleBuffer() {
          guard let sourceBuffer = CMSampleBufferGetImageBuffer(sample), let pool = adaptor.pixelBufferPool else { throw MacMediaServiceError.render("pixel buffer unavailable") }
          var destination: CVPixelBuffer?
          CVPixelBufferPoolCreatePixelBuffer(nil, pool, &destination)
          guard let destination else { throw MacMediaServiceError.render("pixel buffer allocation failed") }
          let inputImage = CIImage(cvPixelBuffer: sourceBuffer)
          let pts = CMSampleBufferGetPresentationTimeStamp(sample)
          let motion = graphMotion(request.graph, at: pts.seconds)
          let outputWidth = CGFloat(request.video.width)
          let outputHeight = CGFloat(request.video.height)
          let sourceExtent = inputImage.extent
          let baseScale = max(outputWidth / sourceExtent.width, outputHeight / sourceExtent.height)
          var transformed = inputImage
            .transformed(by: CGAffineTransform(translationX: -sourceExtent.minX, y: -sourceExtent.minY))
            .transformed(by: CGAffineTransform(scaleX: baseScale, y: baseScale))
          transformed = transformed.transformed(by: CGAffineTransform(
            translationX: (outputWidth - transformed.extent.width) / 2 - transformed.extent.minX,
            y: (outputHeight - transformed.extent.height) / 2 - transformed.extent.minY
          ))
          let center = CGPoint(x: outputWidth * motion.centerX, y: outputHeight * motion.centerY)
          let transform = CGAffineTransform(translationX: center.x, y: center.y)
            .scaledBy(x: motion.scale, y: motion.scale)
            .translatedBy(x: -center.x, y: -center.y)
          transformed = transformed.transformed(by: transform)
          if motion.blur > 0 { transformed = transformed.clampedToExtent().applyingFilter("CIGaussianBlur", parameters: [kCIInputRadiusKey: motion.blur]) }
          transformed = transformed.cropped(to: CGRect(x: 0, y: 0, width: Int(request.video.width), height: Int(request.video.height)))
          for overlay in graphTextOverlays(request.graph, at: pts.seconds, width: CGFloat(request.video.width), height: CGFloat(request.video.height)) {
            transformed = overlay.composited(over: transformed)
          }
          for overlay in graphAssetOverlays(request.graph, assets: visualAssets, at: pts.seconds, width: CGFloat(request.video.width), height: CGFloat(request.video.height)) {
            transformed = overlay.composited(over: transformed)
          }
          ciContext.render(transformed, to: destination)
          hashPixels(destination, into: &pixelHash)
          if !adaptor.append(destination, withPresentationTime: pts) { throw MacMediaServiceError.render(writer.error?.localizedDescription ?? "video append failed") }
          renderedFrames += 1
        } else {
          videoInput.markAsFinished()
          videoDone = true
        }
        progressed = true
      }
      if !audioDone, let audioInput, let audioOutput, audioInput.isReadyForMoreMediaData {
        if let sample = audioOutput.copyNextSampleBuffer() {
          if let block = CMSampleBufferGetDataBuffer(sample) {
            var length = 0; var pointer: UnsafeMutablePointer<Int8>?
            if CMBlockBufferGetDataPointer(block, atOffset: 0, lengthAtOffsetOut: nil, totalLengthOut: &length, dataPointerOut: &pointer) == kCMBlockBufferNoErr, let pointer {
              let count = length / MemoryLayout<Float>.size
              let floats = UnsafeMutableBufferPointer(start: UnsafeMutableRawPointer(pointer).assumingMemoryBound(to: Float.self), count: count)
              if transientFrame == 0 && count > 2 { transientFrame = audioService.transientIndex(samples: Array(floats)) }
              let channels = max(1, Int(request.audio.channels))
              for index in 0..<count {
                let frame = decodedAudioFrames + index / channels
                let channel = index % channels
                for cue in cueMixes {
                  let cueIndex = (frame - cue.startFrame) * channels + channel
                  if cueIndex >= 0 && cueIndex < cue.samples.count {
                    floats[index] = max(-1, min(1, floats[index] + cue.samples[cueIndex]))
                  }
                }
                if splitFrame > 0 && frame >= splitFrame {
                  let wet = tailHistory[tailIndex] * 0.28
                  floats[index] = max(-1, min(1, floats[index] + wet))
                  tailHistory[tailIndex] = floats[index]
                  tailIndex = (tailIndex + 1) % tailHistory.count
                }
              }
              decodedAudioFrames += count / channels
              audioHash.update(data: Data(bytes: floats.baseAddress!, count: count * MemoryLayout<Float>.size))
            }
          }
          if !audioInput.append(sample) { throw MacMediaServiceError.render(writer.error?.localizedDescription ?? "audio append failed") }
        } else {
          audioInput.markAsFinished()
          audioDone = true
        }
        progressed = true
      }
      if progressed {
        lastProgress = Date()
      } else if Date().timeIntervalSince(lastProgress) > 30 {
        throw MacMediaServiceError.render("writer back-pressure timed out")
      } else {
        Thread.sleep(forTimeInterval: 0.001)
      }
    }
    guard reader.status == .completed else { throw MacMediaServiceError.render(reader.error?.localizedDescription ?? "reader failed") }
    awaitWriter(writer)
    guard writer.status == .completed else { throw MacMediaServiceError.render(writer.error?.localizedDescription ?? "writer failed") }
    let temporarySize = (try FileManager.default.attributesOfItem(atPath: temporary.path)[.size] as? NSNumber)?.uint64Value ?? 0
    guard temporarySize > 0, temporarySize <= maxOutputBytes else { throw MacMediaServiceError.render("timeline output exceeds byte cap") }
    try normalizeContainerTimes(temporary)
    let temporaryHandle = try FileHandle(forWritingTo: temporary)
    try temporaryHandle.synchronize()
    try temporaryHandle.close()
    if FileManager.default.fileExists(atPath: output.path) {
      _ = try FileManager.default.replaceItemAt(output, withItemAt: temporary)
    } else {
      try FileManager.default.moveItem(at: temporary, to: output)
    }
    let digest = try sha256Hex(output)
    let renderedAsset = AVURLAsset(url: output)
    let duration = try MacMediaRationalTime(renderedAsset.duration)
    let audioFrames = UInt64(max(0, decodedAudioFrames))
    let transientMs = Int((Double(transientFrame) * 1000 / Double(request.audio.sampleRate)).rounded())
    let receipts = graphReceipts(request.graph, cueMixes: cueMixes, transientMs: transientMs, splitFrame: splitFrame)
    let pixelDigest = pixelHash.finalize().map { String(format: "%02x", $0) }.joined()
    let audioDigest = audioHash.finalize().map { String(format: "%02x", $0) }.joined()
    return TimelineRenderResult(schemaVersion: 1, artifactSha256: digest, pixelSha256: pixelDigest, audioSha256: audioDigest, duration: duration, renderedFrames: renderedFrames, audioFrames: audioFrames, nodeReceipts: receipts)
  }

  private func graphMotion(_ graph: JSONValue, at time: Double) -> (scale: CGFloat, blur: CGFloat, centerX: CGFloat, centerY: CGFloat) {
    guard case let .object(values) = graph, let nodes = values["nodes"], case let .array(items) = nodes else { return (1, 0, 0.5, 0.5) }
    var result: (CGFloat, CGFloat, CGFloat, CGFloat) = (1, 0, 0.5, 0.5)
    for item in items {
      guard case let .object(node) = item,
            let start = rationalSeconds(node["start"]), let end = rationalSeconds(node["end"]),
            end > start, time >= start, time <= end,
            case let .object(kind) = node["node"], let name = kind["kind"]?.stringValue else { continue }
      let progress = max(0, min(1, (time - start) / (end - start)))
      let smooth = progress * progress * (3 - 2 * progress)
      switch name {
      case "pullback", "biasedPushIn":
        guard case let .number(a) = kind["startScale"], case let .number(b) = kind["endScale"] else { continue }
        let x = kind["centerX"]?.numberValue ?? kind["biasX"]?.numberValue ?? 0.5
        let y = kind["centerY"]?.numberValue ?? kind["biasY"]?.numberValue ?? 0.5
        result = (CGFloat(a + (b - a) * smooth), CGFloat(sin(.pi * progress) * 2), CGFloat(x), CGFloat(y))
      case "punchWave":
        guard case let .number(peak) = kind["peakScale"], let peakTime = rationalSeconds(kind["peakTime"]), let width = rationalSeconds(kind["width"]), width > 0 else { continue }
        let distance = min(1, abs(time - peakTime) / width)
        result = (CGFloat(1 + (peak - 1) * (1 - distance)), CGFloat((1 - distance) * 2), 0.5, 0.5)
      default: break
      }
    }
    return result
  }

  private func graphTextOverlays(_ graph: JSONValue, at time: Double, width: CGFloat, height: CGFloat) -> [CIImage] {
    guard case let .object(values) = graph, case let .array(items) = values["nodes"] else { return [] }
    var overlays: [CIImage] = []
    for item in items {
      guard case let .object(row) = item,
            let start = rationalSeconds(row["start"]), let end = rationalSeconds(row["end"]), end > start,
            time >= start, time <= end,
            case let .object(node) = row["node"], let kind = node["kind"]?.stringValue else { continue }
      if kind == "textBloom", let text = node["text"]?.stringValue {
        let progress = max(0, min(1, (time - start) / (end - start)))
        let sample = typographyService.bloom(
          progress: progress,
          startBlur: node["startBlur"]?.numberValue ?? 0,
          risePixels: node["risePx"]?.numberValue ?? 0
        )
        if let image = placedText(text, sample: sample, line: 0, width: width, height: height) { overlays.append(image) }
      } else if kind == "authorityStack", case let .array(lines) = node["lines"], let stagger = rationalSeconds(node["stagger"]) {
        for (index, value) in lines.enumerated() {
          guard let text = value.stringValue else { continue }
          let lineStart = start + Double(index) * stagger
          let progress = max(0, min(1, (time - lineStart) / max(0.001, end - lineStart)))
          if progress > 0, let image = placedText(text, sample: typographyService.bloom(progress: progress, startBlur: 12, risePixels: 18), line: index, width: width, height: height) {
            overlays.append(image)
          }
        }
      }
    }
    return overlays
  }

  private func placedText(_ text: String, sample: TypographySample, line: Int, width: CGFloat, height: CGFloat) -> CIImage? {
    guard var image = typographyService.textImage(text) else { return nil }
    if sample.blur > 0 { image = image.applyingFilter("CIGaussianBlur", parameters: [kCIInputRadiusKey: sample.blur]) }
    image = image.applyingFilter("CIColorMatrix", parameters: ["inputAVector": CIVector(x: 0, y: 0, z: 0, w: sample.opacity)])
    let fit = min(1, max(0.1, (width - 48) / image.extent.width))
    image = image.transformed(by: CGAffineTransform(scaleX: fit, y: fit))
    let x = max(24, (width - image.extent.width) / 2)
    let y = height * 0.18 + CGFloat(line) * 82 + CGFloat(sample.rise)
    return image.transformed(by: CGAffineTransform(translationX: x - image.extent.minX, y: y - image.extent.minY))
  }

  private func graphVisualAssets(_ graph: JSONValue, allowedRoots: [String]) throws -> [String: TimelineVisualAsset] {
    guard case let .object(values) = graph, case let .object(assets) = values["assets"] else { return [:] }
    var result: [String: TimelineVisualAsset] = [:]
    for (id, value) in assets {
      guard let path = value.stringValue else { continue }
      let url = try validateMacMediaFilePath(path, allowedRoots: allowedRoots)
      result[id] = TimelineVisualAsset(url: url)
    }
    return result
  }

  private func graphAssetOverlays(
    _ graph: JSONValue,
    assets: [String: TimelineVisualAsset],
    at time: Double,
    width: CGFloat,
    height: CGFloat
  ) -> [CIImage] {
    guard case let .object(values) = graph, case let .array(items) = values["nodes"] else { return [] }
    var overlays: [CIImage] = []
    for item in items {
      guard case let .object(row) = item,
            let start = rationalSeconds(row["start"]), let end = rationalSeconds(row["end"]), end > start,
            time >= start, time <= end,
            case let .object(node) = row["node"], let kind = node["kind"]?.stringValue,
            let assetId = node["assetId"]?.stringValue,
            let asset = assets[assetId], var image = asset.image(at: time - start) else { continue }
      if kind == "editorTakeover" {
        let scale = max(width / image.extent.width, height / image.extent.height)
        image = image.transformed(by: CGAffineTransform(scaleX: scale, y: scale))
        let x = (width - image.extent.width) / 2 - image.extent.minX
        let y = (height - image.extent.height) / 2 - image.extent.minY
        overlays.append(image.transformed(by: CGAffineTransform(translationX: x, y: y)).cropped(to: CGRect(x: 0, y: 0, width: width, height: height)))
      } else if kind == "assetPlacement" {
        let maxWidth = width * 0.44, maxHeight = height * 0.44
        let scale = min(maxWidth / image.extent.width, maxHeight / image.extent.height)
        image = image.transformed(by: CGAffineTransform(scaleX: scale, y: scale))
        let cell = node["cell"]?.stringValue ?? "topRight"
        let progress = CGFloat(max(0, min(1, (time - start) / (end - start))))
        let parallax = CGFloat(node["parallax"]?.numberValue ?? 0) * (progress - 0.5) * width * 0.1
        let left = cell == "topLeft" || cell == "bottomLeft"
        let top = cell == "topLeft" || cell == "topRight"
        let x = (left ? width * 0.04 : width * 0.96 - image.extent.width) + parallax - image.extent.minX
        let y = (top ? height * 0.52 : height * 0.04) - image.extent.minY
        overlays.append(image.transformed(by: CGAffineTransform(translationX: x, y: y)))
      }
    }
    return overlays
  }

  private func graphAudioSplitFrame(_ graph: JSONValue, sampleRate: Int) -> Int {
    guard case let .object(values) = graph, let nodes = values["nodes"], case let .array(items) = nodes else { return 0 }
    for item in items {
      guard case let .object(node) = item, case let .object(kind) = node["node"] else { continue }
      if kind["kind"]?.stringValue == "reverbThrow", let seconds = rationalSeconds(kind["split"]) {
        return max(0, Int((seconds * Double(sampleRate)).rounded()))
      }
    }
    return 0
  }

  private func graphAudioCues(
    _ graph: JSONValue,
    allowedRoots: [String],
    sampleRate: Int,
    channels: Int
  ) throws -> [TimelineCueMix] {
    guard case let .object(values) = graph,
          case let .object(assets) = values["assets"],
          case let .array(nodes) = values["nodes"] else { return [] }
    var mixes: [TimelineCueMix] = []
    for item in nodes {
      guard case let .object(row) = item, let id = row["id"]?.stringValue,
            case let .object(node) = row["node"], node["kind"]?.stringValue == "audioCue",
            let cueId = node["cueId"]?.stringValue,
            let path = assets[cueId]?.stringValue,
            let target = rationalSeconds(node["targetPeak"]) else { continue }
      let url = try validateMacMediaFilePath(path, allowedRoots: allowedRoots)
      let decoded = try audioService.decodePCM(AVURLAsset(url: url), sampleRate: UInt32(sampleRate), channels: UInt16(channels))
      let transient = audioService.transientIndex(samples: decoded.samples) / max(1, channels)
      let targetFrame = Int((target * Double(sampleRate)).rounded())
      let start = targetFrame - transient
      let actualPeak = start + transient
      let offset = Int((Double(targetFrame - actualPeak) * 1000 / Double(sampleRate)).rounded())
      guard abs(offset) <= 50 else { throw MacMediaServiceError.audio("audio cue transient exceeds 50 ms alignment") }
      mixes.append(TimelineCueMix(id: id, samples: decoded.samples, startFrame: start, transientOffsetMs: offset))
    }
    return mixes
  }

  private func graphReceipts(
    _ graph: JSONValue,
    cueMixes: [TimelineCueMix],
    transientMs: Int,
    splitFrame: Int
  ) -> [JSONValue] {
    guard case let .object(values) = graph, case let .array(nodes) = values["nodes"] else { return [] }
    return nodes.compactMap { item in
      guard case let .object(row) = item, let id = row["id"]?.stringValue,
            case let .object(node) = row["node"], let kind = node["kind"]?.stringValue else { return nil }
      var receipt: [String: JSONValue] = [
        "nodeId": .string(id), "kind": .string(kind), "status": .string("rendered")
      ]
      if kind == "audioCue", let cue = cueMixes.first(where: { $0.id == id }) {
        receipt["audioPeakOffsetMs"] = .number(Double(cue.transientOffsetMs))
      }
      if kind == "reverbThrow" { receipt["splitFrame"] = .number(Double(splitFrame)) }
      if kind == "audioCue" && receipt["audioPeakOffsetMs"] == nil {
        receipt["audioPeakOffsetMs"] = .number(Double(transientMs))
      }
      return .object(receipt)
    }
  }

  private func rationalSeconds(_ value: JSONValue?) -> Double? {
    guard case let .object(parts) = value,
          Set(parts.keys) == ["numerator", "denominator"],
          let numerator = parts["numerator"]?.numberValue,
          let denominator = parts["denominator"]?.numberValue,
          numerator.isFinite, denominator.isFinite, denominator > 0 else { return nil }
    return numerator / denominator
  }

  private func validatedGraph(_ graph: JSONValue) throws -> [String: JSONValue] {
    guard case let .object(value) = graph,
          Set(value.keys) == ["schemaVersion", "sourcePath", "duration", "assets", "nodes"],
          value["schemaVersion"]?.numberValue == 1,
          value["sourcePath"]?.stringValue?.isEmpty == false,
          let duration = rationalSeconds(value["duration"]), duration > 0, duration <= maxDuration,
          case let .object(assets) = value["assets"],
          assets.values.allSatisfy({ $0.stringValue?.isEmpty == false }),
          case let .array(nodes) = value["nodes"], nodes.count <= maxNodes else {
      throw MacMediaServiceError.render("invalid finish render graph")
    }
    let required: [String: Set<String>] = [
      "pullback": ["kind", "startScale", "endScale", "centerX", "centerY"],
      "punchWave": ["kind", "peakScale", "peakTime", "width"],
      "biasedPushIn": ["kind", "startScale", "endScale", "biasX", "biasY"],
      "textBloom": ["kind", "text", "startBlur", "risePx"],
      "authorityStack": ["kind", "lines", "stagger"],
      "assetPlacement": ["kind", "assetId", "cell", "parallax"],
      "editorTakeover": ["kind", "assetId", "firstWordId", "lastWordId"],
      "audioCue": ["kind", "cueId", "targetPeak"],
      "reverbThrow": ["kind", "sourceId", "split", "wetTailMs"]
    ]
    var ids = Set<String>()
    var dependencies: [String: [String]] = [:]
    for item in nodes {
      guard case let .object(node) = item,
            Set(node.keys) == ["id", "start", "end", "inputs", "node"],
            let id = node["id"]?.stringValue, !id.isEmpty, ids.insert(id).inserted,
            let start = rationalSeconds(node["start"]), let end = rationalSeconds(node["end"]),
            start >= 0, end > start, end <= duration,
            case let .array(inputs) = node["inputs"], inputs.allSatisfy({ $0.stringValue != nil }),
            case let .object(kind) = node["node"], let name = kind["kind"]?.stringValue,
            let fields = required[name], Set(kind.keys) == fields else {
        throw MacMediaServiceError.render("invalid finish graph node")
      }
      let numbers = kind.values.compactMap(\.numberValue)
      guard numbers.allSatisfy(\.isFinite) else { throw MacMediaServiceError.render("non-finite finish graph value") }
      let inputIds = inputs.compactMap(\.stringValue)
      guard Set(inputIds).count == inputIds.count else {
        throw MacMediaServiceError.render("finish graph has duplicate inputs")
      }
      dependencies[id] = inputIds
      if let assetId = kind["assetId"]?.stringValue, assets[assetId] == nil {
        throw MacMediaServiceError.render("finish graph references unknown asset")
      }
      if let cueId = kind["cueId"]?.stringValue, assets[cueId] == nil {
        throw MacMediaServiceError.render("finish graph references unknown cue")
      }
      switch name {
      case "pullback":
        guard let startScale = kind["startScale"]?.numberValue,
              let endScale = kind["endScale"]?.numberValue,
              let centerX = kind["centerX"]?.numberValue,
              let centerY = kind["centerY"]?.numberValue,
              abs(startScale - 1.3) < 0.000_001,
              abs(endScale - 1.0) < 0.000_001,
              abs(centerX - 0.5) < 0.000_001,
              abs(centerY - 0.5) < 0.000_001 else {
          throw MacMediaServiceError.render("invalid pullback contract")
        }
      case "punchWave":
        guard let peakScale = kind["peakScale"]?.numberValue, peakScale >= 1,
              let peakTime = rationalSeconds(kind["peakTime"]), (0...duration).contains(peakTime),
              let width = rationalSeconds(kind["width"]), width > 0, width <= duration else {
          throw MacMediaServiceError.render("invalid punch-wave contract")
        }
      case "biasedPushIn":
        guard let startScale = kind["startScale"]?.numberValue, startScale >= 1,
              let endScale = kind["endScale"]?.numberValue, endScale >= 1,
              let biasX = kind["biasX"]?.numberValue, (0...1).contains(biasX),
              let biasY = kind["biasY"]?.numberValue, (0...1).contains(biasY) else {
          throw MacMediaServiceError.render("invalid biased-push contract")
        }
      case "textBloom":
        guard kind["text"]?.stringValue?.isEmpty == false,
              let startBlur = kind["startBlur"]?.numberValue, startBlur >= 0 else {
          throw MacMediaServiceError.render("invalid text-bloom contract")
        }
      case "authorityStack":
        guard case let .array(lines) = kind["lines"], !lines.isEmpty,
              lines.allSatisfy({ $0.stringValue?.isEmpty == false }),
              let stagger = rationalSeconds(kind["stagger"]), (0...duration).contains(stagger) else {
          throw MacMediaServiceError.render("invalid authority-stack contract")
        }
      case "assetPlacement":
        guard let cell = kind["cell"]?.stringValue,
              ["topLeft", "topRight", "bottomLeft", "bottomRight"].contains(cell) else {
          throw MacMediaServiceError.render("invalid asset-placement contract")
        }
      case "editorTakeover":
        guard kind["firstWordId"]?.stringValue?.isEmpty == false,
              kind["lastWordId"]?.stringValue?.isEmpty == false else {
          throw MacMediaServiceError.render("invalid editor-takeover contract")
        }
      case "audioCue":
        guard let target = rationalSeconds(kind["targetPeak"]), (0...duration).contains(target) else {
          throw MacMediaServiceError.render("invalid audio-cue contract")
        }
      case "reverbThrow":
        guard kind["sourceId"]?.stringValue?.isEmpty == false,
              let split = rationalSeconds(kind["split"]), (0...duration).contains(split),
              let wetTail = kind["wetTailMs"]?.numberValue, wetTail > 0 else {
          throw MacMediaServiceError.render("invalid reverb-throw contract")
        }
      default:
        throw MacMediaServiceError.render("unknown finish graph node")
      }
    }
    for item in nodes {
      guard case let .object(node) = item, case let .array(inputs) = node["inputs"] else { continue }
      if inputs.contains(where: { $0.stringValue.map { !ids.contains($0) } ?? true }) {
        throw MacMediaServiceError.render("finish graph references unknown input")
      }
    }
    var visitState: [String: Int] = [:]
    func visit(_ id: String) throws {
      if visitState[id] == 1 { throw MacMediaServiceError.render("finish graph contains a cycle") }
      if visitState[id] == 2 { return }
      visitState[id] = 1
      for dependency in dependencies[id] ?? [] { try visit(dependency) }
      visitState[id] = 2
    }
    for id in ids.sorted() { try visit(id) }
    return value
  }

  private func awaitWriter(_ writer: AVAssetWriter) {
    let semaphore = DispatchSemaphore(value: 0)
    writer.finishWriting { semaphore.signal() }
    semaphore.wait()
  }

  private func fixedCreationMetadata() -> [AVMetadataItem] {
    let item = AVMutableMetadataItem()
    item.keySpace = .common
    item.key = AVMetadataKey.commonKeyCreationDate as NSString
    item.value = "1970-01-01T00:00:00Z" as NSString
    return [item]
  }

  private func normalizeContainerTimes(_ url: URL) throws {
    let handle = try FileHandle(forUpdating: url)
    defer { handle.closeFile() }
    let fileSize = handle.seekToEndOfFile()
    let containers: Set<String> = ["moov", "trak", "mdia", "minf", "stbl", "edts", "dinf"]
    func read(_ offset: UInt64, _ count: Int) throws -> Data {
      handle.seek(toFileOffset: offset)
      return handle.readData(ofLength: count)
    }
    func unsigned(_ bytes: Data) -> UInt64 {
      bytes.reduce(0) { ($0 << 8) | UInt64($1) }
    }
    func visit(_ start: UInt64, _ end: UInt64) throws {
      var cursor = start
      while cursor + 8 <= end {
        let header = try read(cursor, 8)
        guard header.count == 8 else { return }
        let size32 = unsigned(header.prefix(4))
        let type = String(data: header.suffix(4), encoding: .ascii) ?? ""
        var headerSize: UInt64 = 8
        var size = size32
        if size32 == 1 {
          let extended = try read(cursor + 8, 8)
          guard extended.count == 8 else { return }
          size = unsigned(extended)
          headerSize = 16
        } else if size32 == 0 {
          size = end - cursor
        }
        guard size >= headerSize, cursor + size <= end else { return }
        if type == "mvhd" || type == "tkhd" || type == "mdhd" {
          let versionOffset = cursor + headerSize
          let version = try read(versionOffset, 1)
          guard let firstByte = version.first else { return }
          let width: UInt64 = firstByte == 1 ? 8 : 4
          let first = versionOffset + 4
          if first + width * 2 <= cursor + size {
            handle.seek(toFileOffset: first)
            handle.write(Data(repeating: 0, count: Int(width * 2)))
          }
        } else if containers.contains(type) {
          try visit(cursor + headerSize, cursor + size)
        }
        cursor += size
      }
    }
    try visit(0, fileSize)
    handle.synchronizeFile()
  }

  private func sha256Hex(_ url: URL) throws -> String {
    let handle = try FileHandle(forReadingFrom: url)
    defer { handle.closeFile() }
    var hash = SHA256()
    while true {
      let chunk = handle.readData(ofLength: 1024 * 1024)
      if chunk.isEmpty { break }
      hash.update(data: chunk)
    }
    return hash.finalize().map { String(format: "%02x", $0) }.joined()
  }

  private func hashPixels(_ buffer: CVPixelBuffer, into hash: inout SHA256) {
    CVPixelBufferLockBaseAddress(buffer, .readOnly)
    defer { CVPixelBufferUnlockBaseAddress(buffer, .readOnly) }
    guard let base = CVPixelBufferGetBaseAddress(buffer) else { return }
    let bytesPerRow = CVPixelBufferGetBytesPerRow(buffer)
    let rowBytes = CVPixelBufferGetWidth(buffer) * 4
    for row in 0..<CVPixelBufferGetHeight(buffer) {
      hash.update(data: Data(bytes: base.advanced(by: row * bytesPerRow), count: rowBytes))
    }
  }

  private func validatedOutput(_ path: String, roots: [String]) throws -> URL {
    guard path.hasPrefix("/") else { throw MacMediaServiceError.invalidPath(path) }
    let output = URL(fileURLWithPath: path).standardizedFileURL
    let parent = output.deletingLastPathComponent().resolvingSymlinksInPath().standardizedFileURL
    guard roots.contains(where: { parent.path == URL(fileURLWithPath: $0).resolvingSymlinksInPath().standardizedFileURL.path || parent.path.hasPrefix(URL(fileURLWithPath: $0).resolvingSymlinksInPath().standardizedFileURL.path + "/") }) else { throw MacMediaServiceError.pathOutsideAllowedRoots(path) }
    return output
  }
}

private extension JSONValue {
  var numberValue: Double? { if case let .number(value) = self { return value }; return nil }
}
#endif

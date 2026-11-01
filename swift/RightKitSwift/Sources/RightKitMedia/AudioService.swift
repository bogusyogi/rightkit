#if os(macOS)
import AVFoundation
import Accelerate
import Foundation

struct MacAudioFeaturesRequest: Codable {
  let sourcePath: String
  let startSeconds: Double?
  let durationSeconds: Double?
  let allowedRoots: [String]
}

struct MacAudioFeatures: Codable {
  let sampleRate: Double
  let channelCount: Int
  let sampleCount: Int
  let rms: Double
  let peak: Double
  let zeroCrossingRate: Double
  let spectralFlux: Double
  let envelope: [Double]
  let classification: String?
  let classificationConfidence: Double?
  let classifierRevision: String?
}

final class AudioService {
  private let maximumWindowSeconds = 60.0
  private let envelopeBuckets = 256

  /// Produces bounded signal evidence only. No classification is edit authority.
  func audioFeatures(_ request: MacAudioFeaturesRequest) throws -> MacAudioFeatures {
    let source = try validateMacMediaFilePath(request.sourcePath, allowedRoots: request.allowedRoots)
    let asset = AVURLAsset(url: source)
    guard let track = asset.tracks(withMediaType: .audio).first else {
      throw MacMediaServiceError.unsupported("source has no audio track")
    }
    let start = max(0, request.startSeconds ?? 0)
    let duration = min(max(0, request.durationSeconds ?? maximumWindowSeconds), maximumWindowSeconds)
    guard duration > 0 else { throw MacMediaServiceError.audio("audio feature duration must be positive") }
    let reader = try AVAssetReader(asset: asset)
    reader.timeRange = CMTimeRange(start: CMTime(seconds: start, preferredTimescale: 48_000), duration: CMTime(seconds: duration, preferredTimescale: 48_000))
    let output = AVAssetReaderTrackOutput(track: track, outputSettings: [
      AVFormatIDKey: kAudioFormatLinearPCM, AVLinearPCMIsFloatKey: true,
      AVLinearPCMBitDepthKey: 32, AVLinearPCMIsNonInterleaved: false
    ])
    guard reader.canAdd(output) else { throw MacMediaServiceError.audio("cannot decode audio track") }
    reader.add(output)
    guard reader.startReading() else { throw MacMediaServiceError.audio(reader.error?.localizedDescription ?? "audio reader did not start") }
    var samples = [Float]()
    var sampleRate = 0.0
    var channels = 0
    while reader.status == .reading, let buffer = output.copyNextSampleBuffer() {
      defer { CMSampleBufferInvalidate(buffer) }
      guard let block = CMSampleBufferGetDataBuffer(buffer) else { continue }
      var length = 0
      var data: UnsafeMutablePointer<Int8>?
      guard CMBlockBufferGetDataPointer(block, atOffset: 0, lengthAtOffsetOut: nil, totalLengthOut: &length, dataPointerOut: &data) == noErr,
            let data else { continue }
      if let description = CMSampleBufferGetFormatDescription(buffer), let asbd = CMAudioFormatDescriptionGetStreamBasicDescription(description)?.pointee {
        sampleRate = asbd.mSampleRate
        channels = Int(asbd.mChannelsPerFrame)
      }
      let count = length / MemoryLayout<Float>.size
      let values = data.withMemoryRebound(to: Float.self, capacity: count) { Array(UnsafeBufferPointer(start: $0, count: count)) }
      if channels > 1 {
        for frame in stride(from: 0, to: values.count - (values.count % channels), by: channels) {
          samples.append(values[frame..<(frame + channels)].reduce(0, +) / Float(channels))
        }
      } else { samples.append(contentsOf: values) }
    }
    guard reader.status == .completed else { throw MacMediaServiceError.audio(reader.error?.localizedDescription ?? "audio reader failed") }
    guard !samples.isEmpty, samples.allSatisfy({ $0.isFinite }) else { throw MacMediaServiceError.audio("decoded audio contains no finite samples") }
    var squares = [Float](repeating: 0, count: samples.count)
    vDSP_vsq(samples, 1, &squares, 1, vDSP_Length(samples.count))
    var meanSquare: Float = 0
    vDSP_meamgv(squares, 1, &meanSquare, vDSP_Length(squares.count))
    var peak: Float = 0
    vDSP_maxmgv(samples, 1, &peak, vDSP_Length(samples.count))
    let crossings = zip(samples, samples.dropFirst()).reduce(0) { $0 + (($1.0 < 0) != ($1.1 < 0) ? 1 : 0) }
    let envelope = makeEnvelope(samples)
    let flux = spectralFlux(samples)
    return MacAudioFeatures(sampleRate: sampleRate, channelCount: max(channels, 1), sampleCount: samples.count,
                            rms: Double(meanSquare.squareRoot()), peak: Double(peak),
                            zeroCrossingRate: Double(crossings) / Double(max(samples.count - 1, 1)),
                            spectralFlux: flux, envelope: envelope,
                            classification: nil, classificationConfidence: nil, classifierRevision: nil)
  }

  private func makeEnvelope(_ samples: [Float]) -> [Double] {
    let buckets = min(envelopeBuckets, samples.count)
    return (0..<buckets).map { index in
      let start = index * samples.count / buckets
      let end = max(start + 1, (index + 1) * samples.count / buckets)
      return Double(samples[start..<end].map { abs($0) }.max() ?? 0)
    }
  }

  private func spectralFlux(_ samples: [Float]) -> Double {
    let window = 1024
    guard samples.count >= window * 2 else { return 0 }
    var previous = [Float](repeating: 0, count: window / 2)
    var sum = 0.0
    var frames = 0
    for offset in stride(from: 0, through: samples.count - window, by: window) {
      var real = Array(samples[offset..<(offset + window)])
      var imag = [Float](repeating: 0, count: window)
      guard let setup = vDSP_create_fftsetup(vDSP_Length(log2(Float(window))), FFTRadix(kFFTRadix2)) else { return 0 }
      defer { vDSP_destroy_fftsetup(setup) }
      var magnitude = [Float](repeating: 0, count: window / 2)
      real.withUnsafeMutableBufferPointer { realBuffer in
        imag.withUnsafeMutableBufferPointer { imagBuffer in
          guard let realBase = realBuffer.baseAddress, let imagBase = imagBuffer.baseAddress else { return }
          var split = DSPSplitComplex(realp: realBase, imagp: imagBase)
          vDSP_fft_zip(setup, &split, 1, vDSP_Length(log2(Float(window))), FFTDirection(FFT_FORWARD))
          magnitude.withUnsafeMutableBufferPointer { magnitudeBuffer in
            guard let magnitudeBase = magnitudeBuffer.baseAddress else { return }
            vDSP_zvabs(&split, 1, magnitudeBase, 1, vDSP_Length(window / 2))
          }
        }
      }
      sum += zip(magnitude, previous).reduce(0.0) { $0 + Double(max(0, $1.0 - $1.1)) }
      previous = magnitude; frames += 1
    }
    return sum / Double(max(frames, 1))
  }
}
#endif

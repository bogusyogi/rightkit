#if os(macOS)
import Accelerate
import AVFoundation
import Foundation

struct AudioCueSample: Codable { let transientOffsetMs: Int; let dry: Double; let wet: Double }

/// Small, deterministic PCM engine used by the native timeline writer.  It
/// deliberately keeps the dry body untouched and applies a bounded feedback
/// tail only after the stored split cue.
final class AudioFinishService {
  func transientIndex(samples: [Float]) -> Int {
    guard samples.count > 2 else { return 0 }
    var differences = Array(repeating: Float.zero, count: samples.count - 1)
    samples.withUnsafeBufferPointer { source in
      vDSP_vsub(
        source.baseAddress!, 1,
        source.baseAddress!.advanced(by: 1), 1,
        &differences, 1,
        vDSP_Length(differences.count)
      )
    }
    vDSP_vabs(differences, 1, &differences, 1, vDSP_Length(differences.count))
    var peak = Float.zero
    var index = vDSP_Length.zero
    vDSP_maxvi(differences, 1, &peak, &index, vDSP_Length(differences.count))
    return Int(index) + 1
  }

  func split(timeMs: Int, cueMs: Int, transientMs: Int) -> AudioCueSample {
    let delta = abs(cueMs - transientMs) <= 50 ? cueMs - transientMs : 0
    return AudioCueSample(transientOffsetMs: delta, dry: timeMs < cueMs ? 1 : 0, wet: timeMs < cueMs ? 0 : 1)
  }

  func decodePCM(_ asset: AVAsset, sampleRate: UInt32, channels: UInt16) throws -> (samples: [Float], sampleCount: Int) {
    guard let track = asset.tracks(withMediaType: .audio).first else { return ([], 0) }
    let reader = try AVAssetReader(asset: asset)
    let output = AVAssetReaderTrackOutput(track: track, outputSettings: [
      AVFormatIDKey: kAudioFormatLinearPCM, AVSampleRateKey: Int(sampleRate),
      AVNumberOfChannelsKey: Int(channels), AVLinearPCMBitDepthKey: 32,
      AVLinearPCMIsFloatKey: true, AVLinearPCMIsBigEndianKey: false,
      AVLinearPCMIsNonInterleaved: false
    ])
    guard reader.canAdd(output) else { throw MacMediaServiceError.audio("cannot configure linear PCM reader") }
    reader.add(output)
    guard reader.startReading() else { throw MacMediaServiceError.audio(reader.error?.localizedDescription ?? "audio reader did not start") }
    var values: [Float] = []
    while let sample = output.copyNextSampleBuffer(), let block = CMSampleBufferGetDataBuffer(sample) {
      var length = 0; var pointer: UnsafeMutablePointer<Int8>?
      guard CMBlockBufferGetDataPointer(block, atOffset: 0, lengthAtOffsetOut: nil, totalLengthOut: &length, dataPointerOut: &pointer) == kCMBlockBufferNoErr,
            let pointer else { continue }
      let count = length / MemoryLayout<Float>.size
      values.append(contentsOf: UnsafeBufferPointer(start: UnsafeRawPointer(pointer).assumingMemoryBound(to: Float.self), count: count))
    }
    guard reader.status == .completed else { throw MacMediaServiceError.audio(reader.error?.localizedDescription ?? "audio reader failed") }
    return (values, values.count / max(1, Int(channels)))
  }

  func renderWetTail(_ input: [Float], splitFrame: Int, channels: Int, sampleRate: Int) -> [Float] {
    guard !input.isEmpty, channels > 0 else { return input }
    var output = input
    let delay = max(channels, Int(Double(sampleRate) * 0.055) * channels)
    guard splitFrame * channels < input.count else { return output }
    for i in (splitFrame * channels)..<input.count {
      let delayed = i >= delay ? output[i - delay] : 0
      output[i] = max(-1, min(1, input[i] + delayed * 0.28))
    }
    return output
  }
}
#endif

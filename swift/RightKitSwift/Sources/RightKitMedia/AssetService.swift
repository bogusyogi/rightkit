#if os(macOS)
import AVFoundation
import CoreMedia
import Foundation
import ImageIO

/// Framework-facing asset service. `main.swift` translates these values to the
/// versioned wire protocol; this file intentionally has no envelope types.
struct MacMediaRationalTime: Codable, Equatable {
  let numerator: Int64
  let denominator: Int32

  init(_ time: CMTime) throws {
    guard time.isValid, time.timescale > 0 else { throw MacMediaServiceError.invalidTimestamp }
    let divisor = Self.gcd(abs(time.value), Int64(time.timescale))
    numerator = time.value / divisor
    denominator = Int32(Int64(time.timescale) / divisor)
  }

  private static func gcd(_ lhs: Int64, _ rhs: Int64) -> Int64 {
    var a = lhs
    var b = rhs
    while b != 0 { (a, b) = (b, a % b) }
    return max(a, 1)
  }
}

enum MacMediaServiceError: Error {
  case invalidPath(String)
  case pathOutsideAllowedRoots(String)
  case notRegularFile(String)
  case invalidTimestamp
  case assetLoad(String)
  case frameExtraction(String)
  case render(String)
  case audio(String)
  case unsupported(String)
}

extension MacMediaServiceError {
  var code: String {
    switch self {
    case .invalidPath: return "invalid_path"
    case .pathOutsideAllowedRoots: return "path_outside_allowed_roots"
    case .notRegularFile: return "not_regular_file"
    case .invalidTimestamp: return "invalid_timestamp"
    case .assetLoad: return "asset_load_failed"
    case .frameExtraction: return "frame_extraction_failed"
    case .render: return "render_failed"
    case .audio: return "audio_failed"
    case .unsupported: return "unsupported"
    }
  }
}

struct MacMediaVideoTrack: Codable {
  let trackId: Int32
  let naturalWidth: Double
  let naturalHeight: Double
  let preferredTransform: [Double]
  let nominalFrameRate: Double
  let minimumFrameDuration: MacMediaRationalTime?
  let timeRangeStart: MacMediaRationalTime?
  let timeRangeDuration: MacMediaRationalTime?
  let colorProperties: [String: String]
  let hdr: Bool
}

struct MacMediaAudioTrack: Codable {
  let trackId: Int32
  let timeRangeStart: MacMediaRationalTime?
  let timeRangeDuration: MacMediaRationalTime?
  let languageCode: String?
  let formatDescriptions: [String]
}

struct MacMediaAssetInspection: Codable {
  let duration: MacMediaRationalTime?
  let videoTracks: [MacMediaVideoTrack]
  let audioTracks: [MacMediaAudioTrack]
}

struct MacMediaFrameExtraction: Codable {
  let actualTime: MacMediaRationalTime
  let width: Int
  let height: Int
  let outputPath: String
}

/// Validates worker file arguments before AVFoundation sees them.  Resolving
/// symlinks first prevents a permitted-looking path from escaping its roots.
func validateMacMediaFilePath(_ path: String, allowedRoots: [String]) throws -> URL {
  guard path.hasPrefix("/") else { throw MacMediaServiceError.invalidPath(path) }
  let fileURL = URL(fileURLWithPath: path).resolvingSymlinksInPath().standardizedFileURL
  let values = try fileURL.resourceValues(forKeys: [.isRegularFileKey])
  guard values.isRegularFile == true else { throw MacMediaServiceError.notRegularFile(path) }
  let permitted = allowedRoots.contains { root in
    let canonicalRoot = URL(fileURLWithPath: root).resolvingSymlinksInPath().standardizedFileURL
    let prefix = canonicalRoot.path.hasSuffix("/") ? canonicalRoot.path : canonicalRoot.path + "/"
    return fileURL.path == canonicalRoot.path || fileURL.path.hasPrefix(prefix)
  }
  guard permitted else { throw MacMediaServiceError.pathOutsideAllowedRoots(path) }
  return fileURL
}

final class AssetService {
  func inspectAsset(sourcePath: String, allowedRoots: [String]) throws -> MacMediaAssetInspection {
    let url = try validateMacMediaFilePath(sourcePath, allowedRoots: allowedRoots)
    let asset = AVURLAsset(url: url)
    let loaded = DispatchSemaphore(value: 0)
    asset.loadValuesAsynchronously(forKeys: ["tracks", "duration"]) { loaded.signal() }
    loaded.wait()
    var error: NSError?
    guard asset.statusOfValue(forKey: "tracks", error: &error) == .loaded else {
      throw MacMediaServiceError.assetLoad(error?.localizedDescription ?? "tracks unavailable")
    }
    let duration = asset.duration.isNumeric ? try? MacMediaRationalTime(asset.duration) : nil
    let videoTracks = asset.tracks(withMediaType: .video).map(Self.videoTrack)
    let audioTracks = asset.tracks(withMediaType: .audio).map(Self.audioTrack)
    return MacMediaAssetInspection(duration: duration, videoTracks: videoTracks, audioTracks: audioTracks)
  }

  func extractFrame(
    sourcePath: String,
    time: MacMediaRationalTime,
    outputPath: String,
    allowedRoots: [String]
  ) throws -> MacMediaFrameExtraction {
    let source = try validateMacMediaFilePath(sourcePath, allowedRoots: allowedRoots)
    let output = try validatedOutputPath(outputPath, allowedRoots: allowedRoots)
    guard time.denominator > 0 else { throw MacMediaServiceError.invalidTimestamp }
    let generator = AVAssetImageGenerator(asset: AVURLAsset(url: source))
    generator.appliesPreferredTrackTransform = true
    generator.requestedTimeToleranceBefore = .zero
    generator.requestedTimeToleranceAfter = .zero
    var actual = CMTime.zero
    do {
      let image = try generator.copyCGImage(at: CMTime(value: time.numerator, timescale: time.denominator), actualTime: &actual)
      guard let destination = CGImageDestinationCreateWithURL(output as CFURL, "public.jpeg" as CFString, 1, nil) else {
        throw MacMediaServiceError.frameExtraction("cannot create JPEG destination")
      }
      CGImageDestinationAddImage(destination, image, nil)
      guard CGImageDestinationFinalize(destination) else { throw MacMediaServiceError.frameExtraction("cannot write JPEG") }
      return MacMediaFrameExtraction(actualTime: try MacMediaRationalTime(actual), width: image.width, height: image.height, outputPath: output.path)
    } catch let error as MacMediaServiceError {
      throw error
    } catch {
      throw MacMediaServiceError.frameExtraction(error.localizedDescription)
    }
  }

  private func validatedOutputPath(_ path: String, allowedRoots: [String]) throws -> URL {
    guard path.hasPrefix("/") else { throw MacMediaServiceError.invalidPath(path) }
    let output = URL(fileURLWithPath: path).resolvingSymlinksInPath().standardizedFileURL
    guard let parent = output.deletingLastPathComponent().path.removingPercentEncoding else { throw MacMediaServiceError.invalidPath(path) }
    _ = try validateMacMediaDirectory(parent, allowedRoots: allowedRoots)
    return output
  }

  private func validateMacMediaDirectory(_ path: String, allowedRoots: [String]) throws -> URL {
    let directory = URL(fileURLWithPath: path).resolvingSymlinksInPath().standardizedFileURL
    let values = try directory.resourceValues(forKeys: [.isDirectoryKey])
    guard values.isDirectory == true else { throw MacMediaServiceError.invalidPath(path) }
    let permitted = allowedRoots.contains { root in
      let canonical = URL(fileURLWithPath: root).resolvingSymlinksInPath().standardizedFileURL.path
      return directory.path == canonical || directory.path.hasPrefix(canonical + "/")
    }
    guard permitted else { throw MacMediaServiceError.pathOutsideAllowedRoots(path) }
    return directory
  }

  private static func videoTrack(_ track: AVAssetTrack) -> MacMediaVideoTrack {
    let transform = track.preferredTransform
    let color = track.formatDescriptions.compactMap { description -> [String: String]? in
      let format = description as! CMFormatDescription
      guard let extensions = CMFormatDescriptionGetExtensions(format) as? [String: Any] else { return nil }
      return extensions.compactMapValues { value in String(describing: value) }
    }.first ?? [:]
    let hdr = color.keys.contains { $0.localizedCaseInsensitiveContains("HDR") || $0.localizedCaseInsensitiveContains("TransferFunction") }
    return MacMediaVideoTrack(
      trackId: track.trackID,
      naturalWidth: Double(track.naturalSize.width),
      naturalHeight: Double(track.naturalSize.height),
      preferredTransform: [Double(transform.a), Double(transform.b), Double(transform.c), Double(transform.d), Double(transform.tx), Double(transform.ty)],
      nominalFrameRate: Double(track.nominalFrameRate),
      minimumFrameDuration: try? MacMediaRationalTime(track.minFrameDuration),
      timeRangeStart: try? MacMediaRationalTime(track.timeRange.start),
      timeRangeDuration: try? MacMediaRationalTime(track.timeRange.duration),
      colorProperties: color,
      hdr: hdr
    )
  }

  private static func audioTrack(_ track: AVAssetTrack) -> MacMediaAudioTrack {
    MacMediaAudioTrack(
      trackId: track.trackID,
      timeRangeStart: try? MacMediaRationalTime(track.timeRange.start),
      timeRangeDuration: try? MacMediaRationalTime(track.timeRange.duration),
      languageCode: track.languageCode,
      formatDescriptions: track.formatDescriptions.map { String(describing: $0) }
    )
  }
}
#endif

// Does AVFoundation cover the media formats HeardRight accepts today?
//
// The ffmpeg -> AVFoundation swap (see docs/APPLE_NATIVE_API_AUDIT_2026-07-18.md)
// is a COVERAGE question, not an accuracy one: decoding to PCM is exact either
// way, so the only thing that can regress is which containers open at all. The
// file picker applies no extension filter, so users can hand the app anything.
//
// Run:  swiftc -O av_format_coverage.swift -o /tmp/avcov && /tmp/avcov
import AVFoundation
import UniformTypeIdentifiers
let types = AVURLAsset.audiovisualTypes().map { $0.rawValue }
// Formats HeardRight's picker can receive today (no extension filter; ffmpeg decodes all).
let probe = ["mp3","m4a","wav","flac","mp4","mov","aiff","caf","aac",
             "opus","ogg","webm","mkv","wma","amr","wv","ape"]
print("AVFoundation audiovisualTypes: \(types.count)")
for ext in probe {
    guard let ut = UTType(filenameExtension: ext) else { print("  \(ext)\tNO UTType"); continue }
    let ok = types.contains(ut.identifier)
    print("  \(ext)\t\(ok ? "OK" : "UNSUPPORTED")\t\(ut.identifier)")
}

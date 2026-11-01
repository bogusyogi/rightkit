// AVFoundation decode benchmark — the ffmpeg replacement for file transcription.
//
// the app currently shells out to ffmpeg to turn a chosen media file into
// 16 kHz mono PCM s16le (`file_transcribe.rs`). This does the same work through
// AVAssetReader + AVAudioConverter and reports wall time and peak RSS so the
// swap can be judged on latency and footprint, not bundle size.
//
// Correctness note: this writes the SAME format ffmpeg is asked for
// (-ac 1 -ar 16000 -c:a pcm_s16le), so the output is byte-comparable in
// structure and the resulting sample count can be checked against ffmpeg's.
//
// Build: swiftc -O av_decode_bench.swift -o av_decode_bench
// Run:   ./av_decode_bench --input <file> [--out out.pcm]

import AVFoundation
import Foundation

func fail(_ m: String) -> Never {
    FileHandle.standardError.write(Data("av_decode_bench: \(m)\n".utf8))
    exit(1)
}

func peakResidentMB() -> Double {
    var info = rusage()
    getrusage(RUSAGE_SELF, &info)
    return Double(info.ru_maxrss) / 1_048_576.0
}

var input: String?
var outPath: String?
var i = 1
let a = CommandLine.arguments
while i < a.count {
    switch a[i] {
    case "--input": i += 1; input = i < a.count ? a[i] : nil
    case "--out": i += 1; outPath = i < a.count ? a[i] : nil
    default: fail("unknown argument \(a[i])")
    }
    i += 1
}
guard let input else { fail("usage: av_decode_bench --input <file> [--out out.pcm]") }

let url = URL(fileURLWithPath: input)
let asset = AVURLAsset(url: url)

let t0 = Date()

// loadTracks is async; block on it so this stays a straight-line CLI.
let trackSem = DispatchSemaphore(value: 0)
var loadedTracks: [AVAssetTrack] = []
var loadError: Error?
asset.loadTracks(withMediaType: .audio) { tracks, error in
    loadedTracks = tracks ?? []
    loadError = error
    trackSem.signal()
}
_ = trackSem.wait(timeout: .now() + 60)
if let loadError { fail("cannot load tracks: \(loadError.localizedDescription)") }
guard let track = loadedTracks.first else {
    fail("no audio track in \(input) — AVFoundation cannot read this container")
}

guard let reader = try? AVAssetReader(asset: asset) else { fail("cannot create reader") }

// Ask AVFoundation for exactly what ffmpeg is asked for: 16 kHz mono s16le.
let settings: [String: Any] = [
    AVFormatIDKey: kAudioFormatLinearPCM,
    AVSampleRateKey: 16000.0,
    AVNumberOfChannelsKey: 1,
    AVLinearPCMBitDepthKey: 16,
    AVLinearPCMIsFloatKey: false,
    AVLinearPCMIsBigEndianKey: false,
    AVLinearPCMIsNonInterleaved: false,
]
let output = AVAssetReaderTrackOutput(track: track, outputSettings: settings)
guard reader.canAdd(output) else { fail("cannot add output") }
reader.add(output)
guard reader.startReading() else { fail("startReading failed: \(reader.error?.localizedDescription ?? "?")") }

var pcm = Data()
while let buffer = output.copyNextSampleBuffer() {
    guard let block = CMSampleBufferGetDataBuffer(buffer) else { continue }
    var length = 0
    var pointer: UnsafeMutablePointer<Int8>?
    if CMBlockBufferGetDataPointer(block, atOffset: 0, lengthAtOffsetOut: nil, totalLengthOut: &length, dataPointerOut: &pointer) == noErr,
       let pointer {
        pcm.append(UnsafeBufferPointer(start: UnsafeRawPointer(pointer).assumingMemoryBound(to: UInt8.self), count: length))
    }
    CMSampleBufferInvalidate(buffer)
}

if reader.status == .failed {
    fail("decode failed: \(reader.error?.localizedDescription ?? "unknown")")
}

let ms = Date().timeIntervalSince(t0) * 1000
let samples = pcm.count / 2  // 16-bit mono
let seconds = Double(samples) / 16000.0

if let outPath {
    try pcm.write(to: URL(fileURLWithPath: outPath))
}

print("""
{"ms": \(String(format: "%.1f", ms)), "bytes": \(pcm.count), "samples": \(samples), \
"audio_seconds": \(String(format: "%.3f", seconds)), "peak_rss_mb": \(String(format: "%.1f", peakResidentMB()))}
""")

import Foundation

/// Writes mono-16k float samples to a complete, self-contained 16-bit PCM WAV
/// synchronously. Unlike `AVAudioRecorder`, `Data.write(to:)` returns only once
/// complete file bytes are written, so later reads cannot race an async finalize. Used
/// only to persist a capture for the retry / insert-previous path.
public enum PCMWavWriter {
    public enum WriteError: Error, Sendable {
        case invalidSampleRate
        case recordingTooLarge
    }
    public static func write(samples: [Float], sampleRate: Int, to url: URL) throws {
        guard sampleRate > 0, sampleRate <= Int(UInt32.max) / 2 else {
            throw WriteError.invalidSampleRate
        }
        // RIFF's 32-bit size includes 36 bytes preceding PCM; validate before arithmetic/casts.
        guard samples.count <= (Int(UInt32.max) - 36) / 2 else {
            throw WriteError.recordingTooLarge
        }
        let channels = 1
        let bitsPerSample = 16
        let byteRate = sampleRate * channels * bitsPerSample / 8
        let blockAlign = channels * bitsPerSample / 8
        let dataBytes = samples.count * 2

        var data = Data(capacity: 44 + dataBytes)
        func appendUInt32(_ value: UInt32) { var v = value.littleEndian; withUnsafeBytes(of: &v) { data.append(contentsOf: $0) } }
        func appendUInt16(_ value: UInt16) { var v = value.littleEndian; withUnsafeBytes(of: &v) { data.append(contentsOf: $0) } }

        data.append(contentsOf: Array("RIFF".utf8))
        appendUInt32(UInt32(36 + dataBytes))
        data.append(contentsOf: Array("WAVE".utf8))
        data.append(contentsOf: Array("fmt ".utf8))
        appendUInt32(16)                       // PCM fmt chunk size
        appendUInt16(1)                        // PCM
        appendUInt16(UInt16(channels))
        appendUInt32(UInt32(sampleRate))
        appendUInt32(UInt32(byteRate))
        appendUInt16(UInt16(blockAlign))
        appendUInt16(UInt16(bitsPerSample))
        data.append(contentsOf: Array("data".utf8))
        appendUInt32(UInt32(dataBytes))
        // Bulk conversion — a per-sample `data.append` costs ~9.6M calls on a
        // 5-minute capture; one map + one byte copy is a single pass.
        let pcm = samples.map { sample in
            Int16((sample.isFinite ? max(-1, min(1, sample)) : 0) * 32767).littleEndian
        }
        pcm.withUnsafeBytes { data.append(contentsOf: $0) }
        try data.write(to: url, options: .atomic)
    }
}

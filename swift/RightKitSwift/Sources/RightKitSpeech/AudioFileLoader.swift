import AVFoundation
import CoreML
import Foundation

extension ParakeetCoreMLRuntime {
    static func loadMono16kSamples(from url: URL) throws -> [Float] {
        let file = try AVAudioFile(forReading: url)
        let inputFormat = file.processingFormat
        guard let inputBuffer = AVAudioPCMBuffer(
            pcmFormat: inputFormat,
            frameCapacity: AVAudioFrameCount(file.length)
        ) else {
            throw SpeechError.unsupportedAudioFormat
        }
        try file.read(into: inputBuffer)

        guard let outputFormat = AVAudioFormat(
            commonFormat: .pcmFormatFloat32,
            sampleRate: 16_000,
            channels: 1,
            interleaved: false
        ), let converter = AVAudioConverter(from: inputFormat, to: outputFormat) else {
            throw SpeechError.unsupportedAudioFormat
        }

        var consumedInput = false
        var conversionError: NSError?
        var samples: [Float] = []
        while true {
            guard let outputBuffer = AVAudioPCMBuffer(pcmFormat: outputFormat, frameCapacity: 4096) else {
                throw SpeechError.unsupportedAudioFormat
            }
            let status = converter.convert(to: outputBuffer, error: &conversionError) { _, outStatus in
                if consumedInput {
                    outStatus.pointee = .endOfStream
                    return nil
                }
                consumedInput = true
                outStatus.pointee = .haveData
                return inputBuffer
            }
            if status == .error {
                throw conversionError ?? SpeechError.unsupportedAudioFormat
            }
            if let channel = outputBuffer.floatChannelData?.pointee, outputBuffer.frameLength > 0 {
                samples.append(contentsOf: UnsafeBufferPointer(start: channel, count: Int(outputBuffer.frameLength)))
            }
            if status == .endOfStream {
                break
            }
        }
        return samples
    }
}

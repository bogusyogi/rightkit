// CodeRight-owned: RightKitSwift 0.8.1 has no TTS target; nothing to adopt (not serious, report I-4)
import AVFoundation
import Foundation

// ENG-114: native on-device synthesis only. External-artifact gap: none; this
// implementation uses AVSpeechSynthesizer and system frameworks without network
// or third-party dependencies.

/// The explicit PCM contract emitted by ``SpeechSynthesisPipeline``.
public struct SpeechPCMFormat: Sendable, Equatable {
    /// Samples per second in the emitted PCM stream.
    public let sampleRate: Double
    /// Channel count in the emitted PCM stream.
    public let channels: Int
    /// Bits per sample; this pipeline emits Float32.
    public let bitsPerSample: Int
    /// True because the emitted samples are IEEE Float32 values.
    public let isFloat: Bool

    /// Creates the little-endian Float32 PCM format used by the pipeline.
    public init(sampleRate: Double, channels: Int = 1) {
        precondition(sampleRate > 0, "sampleRate must be positive")
        precondition(channels == 1, "the speech pipeline currently emits mono PCM")
        self.sampleRate = sampleRate
        self.channels = channels
        bitsPerSample = 32
        isFloat = true
    }
}

/// A text segment and its UTF-8 source span before speech synthesis.
public struct SpeechTextSegment: Sendable, Equatable {
    /// Text passed to one per-segment synthesis operation.
    public let text: String
    /// UTF-8 byte range in the original input, excluding outer whitespace.
    public let sourceRange: Range<Int>

    /// Creates a source-traceable text segment.
    public init(text: String, sourceRange: Range<Int>) {
        self.text = text
        self.sourceRange = sourceRange
    }
}

/// The position of one source segment in the stitched PCM timeline.
public struct SpeechSegmentTiming: Sendable, Equatable {
    /// Zero-based source segment index.
    public let index: Int
    /// Source text represented by this timeline span.
    public let text: String
    /// UTF-8 byte range in the original input.
    public let sourceRange: Range<Int>
    /// First output frame at which this segment contributes audio.
    public let startFrame: Int
    /// Number of source frames represented, including any overlapped crossfade.
    public let durationFrames: Int
    /// Start offset in seconds in the stitched output.
    public let startTime: TimeInterval
    /// Segment duration in seconds, including any overlapped crossfade.
    public let duration: TimeInterval

    /// Creates a timing entry for a synthesized segment.
    public init(
        index: Int,
        text: String,
        sourceRange: Range<Int>,
        startFrame: Int,
        durationFrames: Int,
        sampleRate: Double
    ) {
        self.index = index
        self.text = text
        self.sourceRange = sourceRange
        self.startFrame = startFrame
        self.durationFrames = durationFrames
        startTime = Double(startFrame) / sampleRate
        duration = Double(durationFrames) / sampleRate
    }
}

/// Stitched Float32 PCM and the source-to-timeline manifest for one synthesis request.
public struct SpeechSynthesisResult: Sendable, Equatable {
    /// Little-endian Float32 PCM bytes.
    public let audio: Data
    /// Explicit format of ``audio``.
    public let format: SpeechPCMFormat
    /// Number of mono frames in ``audio``.
    public let frameCount: Int
    /// Source segment offsets and durations in the stitched timeline.
    public let manifest: [SpeechSegmentTiming]

    /// Creates a completed speech result.
    public init(
        audio: Data,
        format: SpeechPCMFormat,
        frameCount: Int,
        manifest: [SpeechSegmentTiming]
    ) {
        self.audio = audio
        self.format = format
        self.frameCount = frameCount
        self.manifest = manifest
    }
}

/// Typed failures from the bounded native speech pipeline.
public enum SpeechSynthesisError: Error, Sendable, Equatable, LocalizedError {
    case emptyInput
    case inputTooLong(actual: Int, limit: Int)
    case noVoiceAvailable(language: String)
    case synthesisFailed(segment: Int, reason: String)

    public var errorDescription: String? {
        switch self {
        case .emptyInput:
            "Speech input is empty."
        case let .inputTooLong(actual, limit):
            "Speech input is too long (\(actual) characters; limit is \(limit))."
        case let .noVoiceAvailable(language):
            "No speech voice is available for \(language)."
        case let .synthesisFailed(segment, reason):
            "Speech synthesis failed for segment \(segment + 1): \(reason)"
        }
    }
}

/// Native iOS TTS that chunks text, levels each render, and stitches it with a seam-derived crossfade.
@MainActor
public final class SpeechSynthesisPipeline {
    /// Immutable bounds and audio choices for a pipeline instance.
    public struct Configuration: Sendable, Equatable {
        /// BCP-47 language tag passed to AVSpeechSynthesisVoice.
        public let language: String
        /// Hard cap that prevents an unbounded synthesis request.
        public let maxInputCharacters: Int
        /// Maximum sentence chunk size before word/character fallback.
        public let maxSegmentCharacters: Int
        /// Sample rate of the explicit output format.
        public let sampleRate: Double
        /// Common RMS amplitude toward which rendered chunks are normalized.
        public let targetRMS: Float
        /// Maximum positive correction applied to a quiet chunk.
        public let maximumGainDB: Double

        /// Configures a bounded mono Float32 synthesis stream.
        public init(
            language: String = "en-US",
            maxInputCharacters: Int = 20_000,
            maxSegmentCharacters: Int = 240,
            sampleRate: Double = 22_050,
            targetRMS: Float = 0.12,
            maximumGainDB: Double = 18
        ) {
            precondition(!language.isEmpty, "language must not be empty")
            precondition(maxInputCharacters >= 1, "maxInputCharacters must be positive")
            precondition(maxSegmentCharacters >= 1, "maxSegmentCharacters must be positive")
            precondition(sampleRate > 0, "sampleRate must be positive")
            precondition(targetRMS > 0 && targetRMS <= 1, "targetRMS must be in (0, 1]")
            precondition(maximumGainDB >= 0, "maximumGainDB must not be negative")
            self.language = language
            self.maxInputCharacters = maxInputCharacters
            self.maxSegmentCharacters = maxSegmentCharacters
            self.sampleRate = sampleRate
            self.targetRMS = targetRMS
            self.maximumGainDB = maximumGainDB
        }
    }

    /// Bounds and output format used by this pipeline instance.
    public let configuration: Configuration

    /// Creates a pipeline with a 20,000-character input cap and 240-character segment fallback.
    public init(configuration: Configuration = Configuration()) {
        self.configuration = configuration
    }

    /// Splits sentences first, then falls back to word boundaries and finally character boundaries.
    ///
    /// The returned ranges are UTF-8 offsets into the original string after only its outer
    /// whitespace has been removed. Abbreviations and decimal points do not end a sentence.
    public static func chunkText(
        _ text: String,
        maxCharacters: Int = 240
    ) -> [SpeechTextSegment] {
        precondition(maxCharacters >= 1, "maxCharacters must be positive")
        guard let first = text.firstIndex(where: { !$0.isWhitespace }),
              let last = text.lastIndex(where: { !$0.isWhitespace })
        else { return [] }
        let trimmedRange = first ..< text.index(after: last)
        let trimmed = text[trimmedRange]

        let sentences = sentenceRanges(in: trimmed)
        var ranges: [Range<Substring.Index>] = []
        var accumulated: Range<Substring.Index>?

        for sentence in sentences {
            let sentenceLength = trimmed.distance(from: sentence.lowerBound, to: sentence.upperBound)
            if sentenceLength > maxCharacters {
                if let accumulated {
                    ranges.append(accumulated)
                }
                accumulated = nil
                ranges.append(contentsOf: splitOversized(sentence, in: trimmed, maxCharacters: maxCharacters))
                continue
            }

            guard let current = accumulated else {
                accumulated = sentence
                continue
            }
            let combinedLength = trimmed.distance(from: current.lowerBound, to: sentence.upperBound)
            if combinedLength <= maxCharacters {
                accumulated = current.lowerBound ..< sentence.upperBound
            } else {
                ranges.append(current)
                accumulated = sentence
            }
        }
        if let accumulated {
            ranges.append(accumulated)
        }

        return ranges.map { range in
            let lower = text.utf8.distance(from: text.startIndex, to: trimmedRange.lowerBound)
                + trimmed.utf8.distance(from: trimmed.startIndex, to: range.lowerBound)
            let upper = text.utf8.distance(from: text.startIndex, to: trimmedRange.lowerBound)
                + trimmed.utf8.distance(from: trimmed.startIndex, to: range.upperBound)
            return SpeechTextSegment(text: String(trimmed[range]), sourceRange: lower ..< upper)
        }
    }

    /// Synthesizes all chunks serially and returns one explicitly formatted stitched PCM stream.
    public func synthesize(_ text: String) async throws -> SpeechSynthesisResult {
        guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            throw SpeechSynthesisError.emptyInput
        }
        guard text.count <= configuration.maxInputCharacters else {
            throw SpeechSynthesisError.inputTooLong(
                actual: text.count,
                limit: configuration.maxInputCharacters
            )
        }
        guard AVSpeechSynthesisVoice(language: configuration.language) != nil else {
            throw SpeechSynthesisError.noVoiceAvailable(language: configuration.language)
        }

        let chunks = Self.chunkText(text, maxCharacters: configuration.maxSegmentCharacters)
        guard !chunks.isEmpty else { throw SpeechSynthesisError.emptyInput }

        let format = SpeechPCMFormat(sampleRate: configuration.sampleRate)
        guard let targetFormat = AVAudioFormat(
            standardFormatWithSampleRate: configuration.sampleRate,
            channels: 1
        ) else {
            throw SpeechSynthesisError.synthesisFailed(segment: 0, reason: "the requested PCM format is unavailable")
        }
        var normalized: [[Float]] = []
        normalized.reserveCapacity(chunks.count)

        for (index, chunk) in chunks.enumerated() {
            let samples = try await synthesizeSegment(
                chunk.text,
                index: index,
                targetFormat: targetFormat
            )
            guard !samples.isEmpty, Self.rms(of: samples) > 0 else {
                throw SpeechSynthesisError.synthesisFailed(
                    segment: index,
                    reason: "the voice returned no audible PCM"
                )
            }
            normalized.append(Self.normalized(samples, targetRMS: configuration.targetRMS, maximumGainDB: configuration.maximumGainDB))
        }

        let stitched = Self.stitch(normalized)
        guard !stitched.samples.isEmpty, Self.rms(of: stitched.samples) > 0 else {
            throw SpeechSynthesisError.synthesisFailed(segment: 0, reason: "the stitched result is silent")
        }

        let manifest = zip(chunks.indices, chunks).map { index, chunk in
            let timing = stitched.timings[index]
            return SpeechSegmentTiming(
                index: index,
                text: chunk.text,
                sourceRange: chunk.sourceRange,
                startFrame: timing.startFrame,
                durationFrames: timing.durationFrames,
                sampleRate: format.sampleRate
            )
        }
        return SpeechSynthesisResult(
            audio: Self.encodeFloat32(stitched.samples),
            format: format,
            frameCount: stitched.samples.count,
            manifest: manifest
        )
    }

    /// Computes RMS amplitude for synthetic-buffer tests and loudness matching.
    public static func rms(of samples: [Float]) -> Double {
        guard !samples.isEmpty else { return 0 }
        let meanSquare = samples.reduce(0.0) { partial, sample in
            partial + Double(sample) * Double(sample)
        } / Double(samples.count)
        return sqrt(meanSquare)
    }

    private func synthesizeSegment(
        _ text: String,
        index: Int,
        targetFormat: AVAudioFormat
    ) async throws -> [Float] {
        guard let voice = AVSpeechSynthesisVoice(language: configuration.language) else {
            throw SpeechSynthesisError.noVoiceAvailable(language: configuration.language)
        }
        let utterance = AVSpeechUtterance(string: text)
        utterance.voice = voice
        let synthesizer = AVSpeechSynthesizer()
        let cancellation = SynthesizerCancellationBox(synthesizer)
        let collector = PCMBufferCollector(targetFormat: targetFormat)
        defer { cancellation.stop() }

        let outcome: CollectionOutcome = await withTaskCancellationHandler(operation: {
            await withCheckedContinuation { (continuation: CheckedContinuation<CollectionOutcome, Never>) in
                synthesizer.write(utterance) { @Sendable buffer in
                    if let pcm = buffer as? AVAudioPCMBuffer, pcm.frameLength == 0 {
                        if let outcome = collector.finish() {
                            continuation.resume(returning: outcome)
                        }
                        return
                    }
                    collector.append(buffer)
                }
            }
        }, onCancel: {
            cancellation.stop()
        })
        switch outcome {
        case let .samples(samples):
            return samples
        case let .failure(reason):
            throw SpeechSynthesisError.synthesisFailed(segment: index, reason: reason)
        }
    }

    static func normalized(
        _ samples: [Float],
        targetRMS: Float,
        maximumGainDB: Double
    ) -> [Float] {
        let measured = rms(of: samples)
        guard measured > 0 else { return samples }
        let requestedGain = Double(targetRMS) / measured
        let maximumGain = pow(10, maximumGainDB / 20)
        let gain = min(requestedGain, maximumGain)
        return samples.map { sample in
            max(-1, min(1, Float(Double(sample) * gain)))
        }
    }

    struct StitchTiming {
        let startFrame: Int
        let durationFrames: Int
    }

    struct StitchedAudio {
        var samples: [Float]
        var timings: [StitchTiming]
    }

    static func stitch(_ segments: [[Float]]) -> StitchedAudio {
        guard let first = segments.first else { return StitchedAudio(samples: [], timings: []) }
        var output = first
        var timings = [StitchTiming(startFrame: 0, durationFrames: first.count)]
        timings.reserveCapacity(segments.count)

        for segment in segments.dropFirst() {
            let fade = seamFadeLength(left: output, right: segment)
            let overlap = min(fade, output.count / 2, segment.count / 2)
            let start = output.count - overlap
            if overlap > 0 {
                for offset in 0 ..< overlap {
                    let progress = Float(offset + 1) / Float(overlap + 1)
                    output[start + offset] = output[start + offset] * (1 - progress) + segment[offset] * progress
                }
                output.append(contentsOf: segment.dropFirst(overlap))
            } else {
                output.append(contentsOf: segment)
            }
            timings.append(StitchTiming(startFrame: start, durationFrames: segment.count))
        }
        return StitchedAudio(samples: output, timings: timings)
    }

    private static func seamFadeLength(left: [Float], right: [Float]) -> Int {
        guard !left.isEmpty, !right.isEmpty else { return 0 }
        let leftSilence = edgeSilenceLength(left, fromEnd: true)
        let rightSilence = edgeSilenceLength(right, fromEnd: false)
        let leftCrossing = zeroCrossingDistance(left, fromEnd: true)
        let rightCrossing = zeroCrossingDistance(right, fromEnd: false)
        let seamDerived = max(leftSilence, rightSilence, leftCrossing + rightCrossing + 1)
        return min(seamDerived, min(left.count, right.count) / 2)
    }

    private static func edgeSilenceLength(_ samples: [Float], fromEnd: Bool) -> Int {
        let threshold: Float = 0.01
        var count = 0
        if fromEnd {
            for sample in samples.reversed() {
                guard abs(sample) <= threshold else { break }
                count += 1
            }
        } else {
            for sample in samples {
                guard abs(sample) <= threshold else { break }
                count += 1
            }
        }
        return count
    }

    private static func zeroCrossingDistance(_ samples: [Float], fromEnd: Bool) -> Int {
        guard samples.count > 1 else { return 0 }
        if fromEnd {
            for index in stride(from: samples.count - 1, through: 1, by: -1) {
                if samples[index] == 0 || samples[index] * samples[index - 1] <= 0 {
                    return samples.count - 1 - index
                }
            }
            return samples.count - 1
        }
        for index in 0 ..< samples.count - 1 {
            if samples[index] == 0 || samples[index] * samples[index + 1] <= 0 {
                return index
            }
        }
        return samples.count - 1
    }

    private static func encodeFloat32(_ samples: [Float]) -> Data {
        var data = Data(capacity: samples.count * MemoryLayout<Float>.size)
        for sample in samples {
            var bits = sample.bitPattern.littleEndian
            withUnsafeBytes(of: &bits) { data.append(contentsOf: $0) }
        }
        return data
    }

    private static func sentenceRanges(in text: Substring) -> [Range<Substring.Index>] {
        var ranges: [Range<Substring.Index>] = []
        var sentenceStart = text.startIndex
        var index = text.startIndex
        while index < text.endIndex {
            let character = text[index]
            let isTerminator = character == "!" || character == "?" || character == "。" || character == "！" || character == "？"
            let isPeriod = character == "."
            if (isTerminator || isPeriod) && isSentenceEnd(text, at: index, isPeriod: isPeriod) {
                var end = text.index(after: index)
                while end < text.endIndex, text[end].isWhitespace {
                    end = text.index(after: end)
                }
                ranges.append(sentenceStart ..< end)
                sentenceStart = end
                index = end
            } else {
                index = text.index(after: index)
            }
        }
        if sentenceStart < text.endIndex {
            ranges.append(sentenceStart ..< text.endIndex)
        }
        return ranges
    }

    private static func isSentenceEnd(_ text: Substring, at index: Substring.Index, isPeriod: Bool) -> Bool {
        let next = text.index(after: index)
        if next < text.endIndex, text[next].isNumber || text[next] == "." { return false }
        if isPeriod, index > text.startIndex {
            let previous = text[text.index(before: index)]
            if previous.isNumber, next < text.endIndex, text[next].isNumber { return false }
            let wordStart = text[..<index].lastIndex(where: { $0.isWhitespace }).map { text.index(after: $0) } ?? text.startIndex
            let word = text[wordStart ..< index].lowercased()
            let abbreviations: Set<String> = ["mr", "mrs", "ms", "dr", "prof", "sr", "jr", "st", "vs", "etc", "e.g", "i.e"]
            if abbreviations.contains(word) || (word.count == 1 && word.first?.isLetter == true) {
                return false
            }
        }
        return next == text.endIndex || text[next].isWhitespace || !text[next].isLetter
    }

    private static func splitOversized(
        _ range: Range<Substring.Index>,
        in text: Substring,
        maxCharacters: Int
    ) -> [Range<Substring.Index>] {
        let words = text[range].split(whereSeparator: { $0.isWhitespace })
        guard !words.isEmpty else { return [] }
        var result: [Range<Substring.Index>] = []
        var current: Range<Substring.Index>?
        for word in words {
            if word.count > maxCharacters {
                if let current { result.append(current) }
                current = nil
                var start = word.startIndex
                while start < word.endIndex {
                    let end = word.index(start, offsetBy: maxCharacters, limitedBy: word.endIndex) ?? word.endIndex
                    result.append(start ..< end)
                    start = end
                }
                continue
            }
            guard let currentRange = current else {
                current = word.startIndex ..< word.endIndex
                continue
            }
            let combined = text.distance(from: currentRange.lowerBound, to: word.endIndex)
            if combined <= maxCharacters {
                current = currentRange.lowerBound ..< word.endIndex
            } else {
                result.append(currentRange)
                current = word.startIndex ..< word.endIndex
            }
        }
        if let current { result.append(current) }
        return result
    }
}

private enum CollectionOutcome: Sendable {
    case samples([Float])
    case failure(String)
}

/// Synchronizes callback-owned PCM conversion without moving AVAudioBuffer across an actor boundary.
private final class PCMBufferCollector: @unchecked Sendable {
    private let targetFormat: AVAudioFormat
    private let lock = NSLock()
    private var samples: [Float] = []
    private var sourceSampleRate: Double?
    private var failure: String?
    private var finished = false

    init(targetFormat: AVAudioFormat) {
        self.targetFormat = targetFormat
    }

    func append(_ buffer: AVAudioBuffer) {
        lock.lock()
        defer { lock.unlock() }
        guard !finished, failure == nil else { return }
        guard let pcm = buffer as? AVAudioPCMBuffer, pcm.frameLength > 0 else {
            failure = "the callback returned a non-PCM buffer"
            return
        }
        guard pcm.format.sampleRate > 0 else {
            failure = "the callback returned PCM without a sample rate"
            return
        }
        if let sourceSampleRate, sourceSampleRate != pcm.format.sampleRate {
            failure = "the callback changed sample rate mid-segment"
            return
        }
        sourceSampleRate = pcm.format.sampleRate
        do {
            samples.append(contentsOf: try decodedMonoSamples(from: pcm))
        } catch {
            failure = String(describing: error)
        }
    }

    func finish() -> CollectionOutcome? {
        lock.lock()
        defer { lock.unlock() }
        guard !finished else { return nil }
        finished = true
        if let failure { return .failure(failure) }
        guard !samples.isEmpty, let sourceSampleRate else {
            return .failure("the voice returned no PCM frames")
        }
        return .samples(resample(samples, from: sourceSampleRate, to: targetFormat.sampleRate))
    }

    private func decodedMonoSamples(from input: AVAudioPCMBuffer) throws -> [Float] {
        let frameCount = Int(input.frameLength)
        let channelCount = Int(input.format.channelCount)
        guard channelCount > 0 else { throw DecodeFailure.noChannels }
        let stride = Int(input.stride)
        guard stride > 0 else { throw DecodeFailure.invalidStride }
        var mono = [Float](repeating: 0, count: frameCount)

        if let channels = input.floatChannelData {
            for frame in 0 ..< frameCount {
                for channel in 0 ..< channelCount {
                    mono[frame] += channels[channel][frame * stride]
                }
            }
        } else if let channels = input.int16ChannelData {
            for frame in 0 ..< frameCount {
                for channel in 0 ..< channelCount {
                    mono[frame] += Float(channels[channel][frame * stride]) / 32_768
                }
            }
        } else if let channels = input.int32ChannelData {
            for frame in 0 ..< frameCount {
                for channel in 0 ..< channelCount {
                    mono[frame] += Float(channels[channel][frame * stride]) / 2_147_483_648
                }
            }
        } else {
            throw DecodeFailure.unsupportedSampleType
        }
        let divisor = Float(channelCount)
        return mono.map { $0 / divisor }
    }

    private func resample(_ input: [Float], from sourceRate: Double, to targetRate: Double) -> [Float] {
        guard !input.isEmpty, sourceRate != targetRate else { return input }
        let outputCount = max(1, Int((Double(input.count) * targetRate / sourceRate).rounded()))
        var output = [Float](repeating: 0, count: outputCount)
        for index in output.indices {
            let sourcePosition = Double(index) * sourceRate / targetRate
            let lower = min(Int(sourcePosition), input.count - 1)
            let upper = min(lower + 1, input.count - 1)
            let fraction = Float(sourcePosition - Double(lower))
            output[index] = input[lower] * (1 - fraction) + input[upper] * fraction
        }
        return output
    }

    private enum DecodeFailure: Error {
        case noChannels
        case invalidStride
        case unsupportedSampleType
    }
}

/// Gives cancellation a thread-safe stop operation while the speech callback is still draining.
private final class SynthesizerCancellationBox: @unchecked Sendable {
    private let synthesizer: AVSpeechSynthesizer

    init(_ synthesizer: AVSpeechSynthesizer) {
        self.synthesizer = synthesizer
    }

    func stop() {
        synthesizer.stopSpeaking(at: .immediate)
    }
}

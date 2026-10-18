import Foundation
@preconcurrency import AVFoundation

/// The explicit PCM contract of the pipeline's output: mono IEEE Float32.
public struct PCMFormat: Sendable, Equatable {
    public let sampleRate: Double
    public let channels: Int
    public let bitsPerSample: Int
    public let isFloat: Bool

    /// Mono Float32 at `sampleRate`.
    public init(sampleRate: Double) {
        self.sampleRate = max(1, sampleRate)
        channels = 1
        bitsPerSample = 32
        isFloat = true
    }
}

/// Which system voice speaks.
public enum VoiceSelection: Sendable, Equatable {
    /// The default voice for a BCP-47 language tag (for example `"en-US"`).
    case language(String)
    /// One specific installed voice, by `AVSpeechSynthesisVoice.identifier`.
    case identifier(String)

    var description: String {
        switch self {
        case let .language(tag): return tag
        case let .identifier(id): return id
        }
    }
}

/// One installed system voice.
public struct VoiceInfo: Sendable, Equatable, Identifiable {
    public enum Quality: Int, Sendable, Equatable, Comparable {
        case standard = 0
        case enhanced = 1
        case premium = 2
        public static func < (lhs: Quality, rhs: Quality) -> Bool { lhs.rawValue < rhs.rawValue }
    }

    public let identifier: String
    public let name: String
    public let language: String
    public let quality: Quality
    public var id: String { identifier }

    public init(identifier: String, name: String, language: String, quality: Quality) {
        self.identifier = identifier
        self.name = name
        self.language = language
        self.quality = quality
    }

    /// Installed voices, best quality first then by name. `language` keeps voices whose tag equals it or
    /// shares its language code (`"en"` matches `"en-GB"`).
    public static func installed(language: String? = nil) -> [VoiceInfo] {
        let voices = AVSpeechSynthesisVoice.speechVoices().map { voice in
            VoiceInfo(
                identifier: voice.identifier, name: voice.name, language: voice.language,
                quality: voice.quality == .premium ? .premium : voice.quality == .enhanced ? .enhanced : .standard)
        }
        let filtered = voices.filter { voice in
            guard let language else { return true }
            let have = voice.language.lowercased()
            let wanted = language.lowercased()
            if have == wanted { return true }
            // A bare language code ("en") matches every region ("en-GB").
            return !wanted.contains("-") && have.hasPrefix(wanted + "-")
        }
        return filtered.sorted { lhs, rhs in
            lhs.quality != rhs.quality ? lhs.quality > rhs.quality : lhs.name < rhs.name
        }
    }
}

/// The position of one source segment in the stitched PCM timeline.
public struct SegmentTiming: Sendable, Equatable {
    public let index: Int
    public let text: String
    public let sourceRange: Range<Int>
    public let startFrame: Int
    public let durationFrames: Int
    public let startTime: TimeInterval
    public let duration: TimeInterval

    public init(index: Int, text: String, sourceRange: Range<Int>, startFrame: Int, durationFrames: Int, sampleRate: Double) {
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
public struct SynthesisResult: Sendable, Equatable {
    /// Little-endian Float32 PCM bytes.
    public let audio: Data
    public let format: PCMFormat
    public let frameCount: Int
    public let manifest: [SegmentTiming]

    public init(audio: Data, format: PCMFormat, frameCount: Int, manifest: [SegmentTiming]) {
        self.audio = audio
        self.format = format
        self.frameCount = frameCount
        self.manifest = manifest
    }
}

/// One synthesized, loudness-matched chunk of speech.
public struct AudioChunk: Sendable, Equatable {
    /// Zero-based position in the request.
    public let index: Int
    /// Number of chunks in the request.
    public let count: Int
    public let text: String
    /// UTF-8 byte range of ``text`` in the original input.
    public let sourceRange: Range<Int>
    /// Mono Float32 samples at ``format``'s rate.
    public let samples: [Float]
    public let format: PCMFormat

    public init(index: Int, count: Int, text: String, sourceRange: Range<Int>, samples: [Float], format: PCMFormat) {
        self.index = index
        self.count = count
        self.text = text
        self.sourceRange = sourceRange
        self.samples = samples
        self.format = format
    }

    public var duration: TimeInterval { Double(samples.count) / format.sampleRate }

    /// A playable buffer, or nil if the system cannot allocate one.
    public func makeBuffer() -> AVAudioPCMBuffer? {
        guard !samples.isEmpty,
              let audioFormat = AVAudioFormat(standardFormatWithSampleRate: format.sampleRate, channels: 1),
              let buffer = AVAudioPCMBuffer(pcmFormat: audioFormat, frameCapacity: AVAudioFrameCount(samples.count)),
              let channel = buffer.floatChannelData?[0]
        else { return nil }
        samples.withUnsafeBufferPointer { channel.update(from: $0.baseAddress!, count: samples.count) }
        buffer.frameLength = AVAudioFrameCount(samples.count)
        return buffer
    }
}

/// Typed failures from the bounded native speech pipeline.
public enum SynthesisError: Error, Sendable, Equatable, LocalizedError {
    case emptyInput
    case inputTooLong(actual: Int, limit: Int)
    case noVoiceAvailable(voice: String)
    case synthesisFailed(segment: Int, reason: String)

    public var errorDescription: String? {
        switch self {
        case .emptyInput: return "Speech input is empty."
        case let .inputTooLong(actual, limit): return "Speech input is too long (\(actual) characters; limit is \(limit))."
        case let .noVoiceAvailable(voice): return "No speech voice is available for \(voice)."
        case let .synthesisFailed(segment, reason): return "Speech synthesis failed for segment \(segment + 1): \(reason)"
        }
    }
}

/// Renders one segment of text to mono Float32 samples at a requested rate. The pipeline's only platform
/// dependency, so it can be replaced in tests.
protocol SegmentSynthesizer: Sendable {
    func isVoiceAvailable(_ voice: VoiceSelection) -> Bool
    /// Returns audible samples at `targetFormat`'s rate, or throws `SynthesisError.synthesisFailed`/`CancellationError`.
    func render(text: String, index: Int, configuration: SynthesisPipeline.Configuration, targetFormat: AVAudioFormat) async throws -> [Float]
}

/// Native on-device TTS: chunks text, renders each chunk with `AVSpeechSynthesizer.write`, levels each
/// render to a common loudness, and either streams the chunks as they finish (``chunks(for:)``) or stitches
/// them with a seam-derived crossfade (``synthesize(_:)``).
///
/// Cancelling the consuming task (or breaking out of the stream) cancels the in-flight render. No network,
/// provider or third-party dependency is involved.
public struct SynthesisPipeline: Sendable {
    public struct Configuration: Sendable, Equatable {
        public var voice: VoiceSelection
        /// Hard cap that prevents an unbounded synthesis request.
        public var maxInputCharacters: Int
        /// Maximum sentence chunk size before word and character fallback.
        public var maxSegmentCharacters: Int
        /// Sample rate of the output.
        public var sampleRate: Double
        /// Common RMS amplitude toward which rendered chunks are normalized.
        public var targetRMS: Float
        /// Maximum positive correction applied to a quiet chunk.
        public var maximumGainDB: Double
        /// `AVSpeechUtterance.rate` (0...1), nil for the system default. Playback speed is separate: see
        /// ``PlaybackController/rate``.
        public var speechRate: Float?

        public init(
            voice: VoiceSelection = .language("en-US"),
            maxInputCharacters: Int = 20_000,
            maxSegmentCharacters: Int = 240,
            sampleRate: Double = 22_050,
            targetRMS: Float = 0.12,
            maximumGainDB: Double = 18,
            speechRate: Float? = nil
        ) {
            self.voice = voice
            self.maxInputCharacters = max(1, maxInputCharacters)
            self.maxSegmentCharacters = max(1, maxSegmentCharacters)
            self.sampleRate = max(1, sampleRate)
            self.targetRMS = min(max(targetRMS, 0.001), 1)
            self.maximumGainDB = max(0, maximumGainDB)
            self.speechRate = speechRate.map { min(max($0, AVSpeechUtteranceMinimumSpeechRate), AVSpeechUtteranceMaximumSpeechRate) }
        }
    }

    public let configuration: Configuration
    private let synthesizer: any SegmentSynthesizer

    public init(configuration: Configuration = Configuration()) {
        self.init(configuration: configuration, synthesizer: AVSpeechSegmentSynthesizer())
    }

    init(configuration: Configuration, synthesizer: any SegmentSynthesizer) {
        self.configuration = configuration
        self.synthesizer = synthesizer
    }

    public static func chunkText(_ text: String, maxCharacters: Int = 240) -> [TextSegment] {
        TextChunker.chunk(text, maxCharacters: maxCharacters)
    }

    /// Streams the chunks of `text` in order, each as soon as it has been rendered, so playback can start
    /// after the first one. The stream fails with a ``SynthesisError`` (nothing is yielded for invalid
    /// input) or `CancellationError`; ending the iteration early cancels the remaining work.
    public func chunks(for text: String) -> AsyncThrowingStream<AudioChunk, Error> {
        let configuration = self.configuration
        let synthesizer = self.synthesizer
        return AsyncThrowingStream { continuation in
            let task = Task {
                do {
                    guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { throw SynthesisError.emptyInput }
                    guard text.count <= configuration.maxInputCharacters else {
                        throw SynthesisError.inputTooLong(actual: text.count, limit: configuration.maxInputCharacters)
                    }
                    guard synthesizer.isVoiceAvailable(configuration.voice) else {
                        throw SynthesisError.noVoiceAvailable(voice: configuration.voice.description)
                    }
                    let segments = TextChunker.chunk(text, maxCharacters: configuration.maxSegmentCharacters)
                    guard !segments.isEmpty else { throw SynthesisError.emptyInput }
                    guard let targetFormat = AVAudioFormat(standardFormatWithSampleRate: configuration.sampleRate, channels: 1) else {
                        throw SynthesisError.synthesisFailed(segment: 0, reason: "the requested PCM format is unavailable")
                    }
                    let format = PCMFormat(sampleRate: configuration.sampleRate)
                    for (index, segment) in segments.enumerated() {
                        try Task.checkCancellation()
                        let samples = try await synthesizer.render(
                            text: segment.text, index: index, configuration: configuration, targetFormat: targetFormat)
                        guard !samples.isEmpty, Self.rms(of: samples) > 0 else {
                            throw SynthesisError.synthesisFailed(segment: index, reason: "the voice returned no audible PCM")
                        }
                        continuation.yield(AudioChunk(
                            index: index, count: segments.count, text: segment.text, sourceRange: segment.sourceRange,
                            samples: Self.normalized(samples, targetRMS: configuration.targetRMS, maximumGainDB: configuration.maximumGainDB),
                            format: format))
                    }
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    /// Synthesizes every chunk and returns one stitched stream with a source-to-timeline manifest.
    public func synthesize(_ text: String) async throws -> SynthesisResult {
        var chunks: [AudioChunk] = []
        for try await chunk in self.chunks(for: text) { chunks.append(chunk) }
        guard !chunks.isEmpty else { throw SynthesisError.emptyInput }
        let stitched = Self.stitch(chunks.map(\.samples))
        guard !stitched.samples.isEmpty, Self.rms(of: stitched.samples) > 0 else {
            throw SynthesisError.synthesisFailed(segment: 0, reason: "the stitched result is silent")
        }
        let format = PCMFormat(sampleRate: configuration.sampleRate)
        let manifest = zip(chunks, stitched.timings).map { chunk, timing in
            SegmentTiming(
                index: chunk.index, text: chunk.text, sourceRange: chunk.sourceRange,
                startFrame: timing.startFrame, durationFrames: timing.durationFrames, sampleRate: format.sampleRate)
        }
        return SynthesisResult(audio: Self.encodeFloat32(stitched.samples), format: format, frameCount: stitched.samples.count, manifest: manifest)
    }

    // MARK: Signal helpers

    /// RMS amplitude, for loudness matching and silence detection.
    public static func rms(of samples: [Float]) -> Double {
        guard !samples.isEmpty else { return 0 }
        let meanSquare = samples.reduce(0.0) { $0 + Double($1) * Double($1) } / Double(samples.count)
        return meanSquare.squareRoot()
    }

    static func normalized(_ samples: [Float], targetRMS: Float, maximumGainDB: Double) -> [Float] {
        let measured = rms(of: samples)
        guard measured > 0 else { return samples }
        let gain = min(Double(targetRMS) / measured, pow(10, maximumGainDB / 20))
        return samples.map { max(-1, min(1, Float(Double($0) * gain))) }
    }

    struct StitchTiming: Equatable {
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
        for segment in segments.dropFirst() {
            let overlap = min(seamFadeLength(left: output, right: segment), output.count / 2, segment.count / 2)
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
            for index in stride(from: samples.count - 1, through: 1, by: -1) where samples[index] == 0 || samples[index] * samples[index - 1] <= 0 {
                return samples.count - 1 - index
            }
            return samples.count - 1
        }
        for index in 0 ..< samples.count - 1 where samples[index] == 0 || samples[index] * samples[index + 1] <= 0 {
            return index
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
}

// MARK: - AVSpeechSynthesizer renderer

private enum RenderOutcome: Sendable {
    case samples([Float])
    case failure(String)
    case cancelled
}

/// Resumes its continuation exactly once, whether the result or a cancellation comes first.
private final class RenderOutcomeBox: @unchecked Sendable {
    private let lock = NSLock()
    private var continuation: CheckedContinuation<RenderOutcome, Never>?
    private var pending: RenderOutcome?
    private var resumed = false

    func install(_ continuation: CheckedContinuation<RenderOutcome, Never>) {
        let early = lock.withLock { () -> RenderOutcome? in
            if let pending { return pending }
            self.continuation = continuation
            return nil
        }
        if let early { continuation.resume(returning: early) }
    }

    func resume(_ outcome: RenderOutcome) {
        let waiting = lock.withLock { () -> CheckedContinuation<RenderOutcome, Never>? in
            guard !resumed else { return nil }
            resumed = true
            if let continuation { self.continuation = nil; return continuation }
            pending = outcome
            return nil
        }
        waiting?.resume(returning: outcome)
    }
}

struct AVSpeechSegmentSynthesizer: SegmentSynthesizer {
    func isVoiceAvailable(_ voice: VoiceSelection) -> Bool {
        Self.voice(for: voice) != nil
    }

    static func voice(for selection: VoiceSelection) -> AVSpeechSynthesisVoice? {
        switch selection {
        case let .language(tag): return AVSpeechSynthesisVoice(language: tag)
        case let .identifier(id): return AVSpeechSynthesisVoice(identifier: id)
        }
    }

    func render(text: String, index: Int, configuration: SynthesisPipeline.Configuration, targetFormat: AVAudioFormat) async throws -> [Float] {
        guard let voice = Self.voice(for: configuration.voice) else {
            throw SynthesisError.noVoiceAvailable(voice: configuration.voice.description)
        }
        switch await renderOnMain(text: text, voice: voice, rate: configuration.speechRate, targetFormat: targetFormat) {
        case let .samples(samples): return samples
        case let .failure(reason): throw SynthesisError.synthesisFailed(segment: index, reason: reason)
        case .cancelled: throw CancellationError()
        }
    }

    @MainActor
    private func renderOnMain(text: String, voice: AVSpeechSynthesisVoice, rate: Float?, targetFormat: AVAudioFormat) async -> RenderOutcome {
        let utterance = AVSpeechUtterance(string: text)
        utterance.voice = voice
        if let rate { utterance.rate = rate }
        let synthesizer = AVSpeechSynthesizer()
        let stopper = SynthesizerStopper(synthesizer)
        let collector = PCMBufferCollector(targetFormat: targetFormat)
        let box = RenderOutcomeBox()
        defer { stopper.stop() }
        return await withTaskCancellationHandler(operation: {
            await withCheckedContinuation { (continuation: CheckedContinuation<RenderOutcome, Never>) in
                box.install(continuation)
                synthesizer.write(utterance) { @Sendable buffer in
                    if let pcm = buffer as? AVAudioPCMBuffer, pcm.frameLength == 0 {
                        // A zero-length buffer is the end of the utterance.
                        if let outcome = collector.finish() { box.resume(outcome) }
                        return
                    }
                    collector.append(buffer)
                }
            }
        }, onCancel: {
            stopper.stop()
            box.resume(.cancelled)
        })
    }
}

/// Synchronizes callback-owned PCM conversion without moving `AVAudioBuffer` across an actor boundary.
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

    func finish() -> RenderOutcome? {
        lock.lock()
        defer { lock.unlock() }
        guard !finished else { return nil }
        finished = true
        if let failure { return .failure(failure) }
        guard !samples.isEmpty, let sourceSampleRate else { return .failure("the voice returned no PCM frames") }
        return .samples(PCMResampler.linear(samples, from: sourceSampleRate, to: targetFormat.sampleRate))
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
                for channel in 0 ..< channelCount { mono[frame] += channels[channel][frame * stride] }
            }
        } else if let channels = input.int16ChannelData {
            for frame in 0 ..< frameCount {
                for channel in 0 ..< channelCount { mono[frame] += Float(channels[channel][frame * stride]) / 32_768 }
            }
        } else if let channels = input.int32ChannelData {
            for frame in 0 ..< frameCount {
                for channel in 0 ..< channelCount { mono[frame] += Float(channels[channel][frame * stride]) / 2_147_483_648 }
            }
        } else {
            throw DecodeFailure.unsupportedSampleType
        }
        let divisor = Float(channelCount)
        return mono.map { $0 / divisor }
    }

    private enum DecodeFailure: Error {
        case noChannels
        case invalidStride
        case unsupportedSampleType
    }
}

/// Linear-interpolation resampler for the mono renders `AVSpeechSynthesizer` produces at its own rate.
enum PCMResampler {
    static func linear(_ input: [Float], from sourceRate: Double, to targetRate: Double) -> [Float] {
        guard !input.isEmpty, sourceRate != targetRate else { return input }
        let outputCount = max(1, Int((Double(input.count) * targetRate / sourceRate).rounded()))
        var output = [Float](repeating: 0, count: outputCount)
        for index in output.indices {
            let position = Double(index) * sourceRate / targetRate
            let lower = min(Int(position), input.count - 1)
            let upper = min(lower + 1, input.count - 1)
            let fraction = Float(position - Double(lower))
            output[index] = input[lower] * (1 - fraction) + input[upper] * fraction
        }
        return output
    }
}

/// Gives cancellation a thread-safe stop operation while the speech callback is still draining.
private final class SynthesizerStopper: @unchecked Sendable {
    private let synthesizer: AVSpeechSynthesizer

    init(_ synthesizer: AVSpeechSynthesizer) {
        self.synthesizer = synthesizer
    }

    func stop() {
        synthesizer.stopSpeaking(at: .immediate)
    }
}

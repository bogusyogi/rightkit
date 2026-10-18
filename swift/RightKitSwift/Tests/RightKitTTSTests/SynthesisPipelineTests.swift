import AVFoundation
import Foundation
import Testing

@testable import RightKitTTS

private final class Counter: @unchecked Sendable {
    private let lock = NSLock()
    private var _value = 0
    private var _flag = false
    var value: Int { lock.withLock { _value } }
    var flag: Bool {
        get { lock.withLock { _flag } }
        set { lock.withLock { _flag = newValue } }
    }
    func bump() { lock.withLock { _value += 1 } }
}

/// Renders a quiet tone per segment instead of speaking.
private struct FakeSynthesizer: SegmentSynthesizer {
    var voiceAvailable = true
    var renderBody: @Sendable (_ text: String, _ index: Int) async throws -> [Float] = { _, _ in tone(amplitude: 0.05, frames: 2_205) }

    func isVoiceAvailable(_ voice: VoiceSelection) -> Bool { voiceAvailable }

    func render(text: String, index: Int, configuration: SynthesisPipeline.Configuration, targetFormat: AVAudioFormat) async throws -> [Float] {
        try await renderBody(text, index)
    }
}

private func tone(amplitude: Float, frames: Int) -> [Float] {
    (0..<frames).map { amplitude * sin(Float($0) * 2 * .pi * 220 / 22_050) }
}

private func pipeline(maxSegment: Int = 6, _ synthesizer: FakeSynthesizer = FakeSynthesizer()) -> SynthesisPipeline {
    SynthesisPipeline(configuration: .init(maxSegmentCharacters: maxSegment), synthesizer: synthesizer)
}

private func collect(_ stream: AsyncThrowingStream<AudioChunk, Error>) async throws -> [AudioChunk] {
    var chunks: [AudioChunk] = []
    for try await chunk in stream { chunks.append(chunk) }
    return chunks
}

struct TextChunkerTests {
    @Test func sentencesPackUpToTheLimitAndSplitBeyondIt() {
        let text = "One. Two. Three."
        #expect(TextChunker.chunk(text, maxCharacters: 6).map(\.text) == ["One.", "Two.", "Three."])
        #expect(TextChunker.chunk(text, maxCharacters: 240).map(\.text) == ["One. Two. Three."])
        #expect(TextChunker.chunk(text, maxCharacters: 10).map(\.text) == ["One. Two.", "Three."])
    }

    @Test func abbreviationsInitialsAndDecimalsDoNotEndASentence() {
        let text = "Dr. Smith paid 3.50 dollars, e.g. for tea. Then he left."
        #expect(TextChunker.chunk(text, maxCharacters: 50).map(\.text) == ["Dr. Smith paid 3.50 dollars, e.g. for tea.", "Then he left."])
        #expect(TextChunker.chunk("Mr. J. R. Smith waved.", maxCharacters: 240).count == 1)
    }

    @Test func anOversizedSentenceFallsBackToWordsThenCharacters() {
        let words = TextChunker.chunk("alpha beta gamma delta", maxCharacters: 11)
        #expect(words.map(\.text) == ["alpha beta", "gamma delta"])
        #expect(words.allSatisfy { $0.text.count <= 11 })
        let characters = TextChunker.chunk("abcdefghij", maxCharacters: 4)
        #expect(characters.map(\.text) == ["abcd", "efgh", "ij"])
    }

    @Test func rangesAreUTF8OffsetsIntoTheOriginalWithOuterWhitespaceRemoved() throws {
        let text = "  Héllo wörld.  Bye!  "
        let chunks = TextChunker.chunk(text, maxCharacters: 14)
        #expect(chunks.map(\.text) == ["Héllo wörld.", "Bye!"])
        let bytes = Array(text.utf8)
        for chunk in chunks {
            #expect(String(decoding: bytes[chunk.sourceRange], as: UTF8.self) == chunk.text)
        }
    }

    @Test func emptyAndBlankTextProduceNothing() {
        #expect(TextChunker.chunk("").isEmpty)
        #expect(TextChunker.chunk(" \n\t ").isEmpty)
        #expect(TextChunker.chunk("x", maxCharacters: 0).map(\.text) == ["x"], "a non-positive limit is treated as one character")
    }
}

struct SynthesisPipelineTests {
    @Test func chunksStreamInOrderWithIndexCountAndSourceRanges() async throws {
        let chunks = try await collect(pipeline().chunks(for: "One. Two. Three."))
        #expect(chunks.map(\.index) == [0, 1, 2])
        #expect(chunks.allSatisfy { $0.count == 3 })
        #expect(chunks.map(\.text) == ["One.", "Two.", "Three."])
        #expect(chunks.map(\.sourceRange) == [0 ..< 4, 5 ..< 9, 10 ..< 16])
        #expect(chunks.allSatisfy { $0.format == PCMFormat(sampleRate: 22_050) })
    }

    @Test func everyChunkIsLeveledToTheTargetLoudness() async throws {
        let quiet = FakeSynthesizer(renderBody: { _, index in tone(amplitude: index == 0 ? 0.03 : 0.2, frames: 2_205) })
        let chunks = try await collect(pipeline(quiet).chunks(for: "One. Two."))
        for chunk in chunks {
            #expect(abs(SynthesisPipeline.rms(of: chunk.samples) - 0.12) < 0.005)
        }
    }

    @Test func gainIsCappedSoNearSilenceIsNotBlownUpIntoNoise() {
        let samples = tone(amplitude: 0.0001, frames: 1_000)
        let leveled = SynthesisPipeline.normalized(samples, targetRMS: 0.12, maximumGainDB: 18)
        let gain = SynthesisPipeline.rms(of: leveled) / SynthesisPipeline.rms(of: samples)
        #expect(abs(gain - pow(10, 18.0 / 20)) < 0.01)
        #expect(SynthesisPipeline.normalized([], targetRMS: 0.12, maximumGainDB: 18).isEmpty)
        #expect(SynthesisPipeline.normalized([0, 0], targetRMS: 0.12, maximumGainDB: 18) == [0, 0])
    }

    @Test func invalidRequestsFailBeforeAnyRendering() async {
        let rendered = Counter()
        let counting = FakeSynthesizer(renderBody: { _, _ in rendered.bump(); return tone(amplitude: 0.1, frames: 100) })
        await #expect(throws: SynthesisError.emptyInput) { try await collect(pipeline(counting).chunks(for: "  \n ")) }
        let short = SynthesisPipeline(configuration: .init(maxInputCharacters: 5), synthesizer: counting)
        await #expect(throws: SynthesisError.inputTooLong(actual: 11, limit: 5)) { try await collect(short.chunks(for: "hello world")) }
        var noVoice = counting
        noVoice.voiceAvailable = false
        await #expect(throws: SynthesisError.noVoiceAvailable(voice: "en-US")) { try await collect(pipeline(noVoice).chunks(for: "Hi.")) }
        #expect(rendered.value == 0)
    }

    @Test func aSilentSegmentFailsTheRequestAfterTheChunksBeforeIt() async {
        let silentSecond = FakeSynthesizer(renderBody: { _, index in index == 1 ? [Float](repeating: 0, count: 500) : tone(amplitude: 0.1, frames: 500) })
        var received: [Int] = []
        do {
            for try await chunk in pipeline(silentSecond).chunks(for: "One. Two. Three.") { received.append(chunk.index) }
            Issue.record("expected the stream to fail")
        } catch {
            #expect(error as? SynthesisError == .synthesisFailed(segment: 1, reason: "the voice returned no audible PCM"))
        }
        #expect(received == [0], "chunks rendered before the failure are still delivered")
    }

    @Test func aRendererFailureIsReportedWithItsSegment() async {
        let failing = FakeSynthesizer(renderBody: { _, index in
            if index == 2 { throw SynthesisError.synthesisFailed(segment: 2, reason: "boom") }
            return tone(amplitude: 0.1, frames: 500)
        })
        await #expect(throws: SynthesisError.synthesisFailed(segment: 2, reason: "boom")) {
            try await collect(pipeline(failing).chunks(for: "One. Two. Three."))
        }
    }

    @Test func abandoningTheStreamCancelsTheRenderInFlight() async throws {
        let cancelled = Counter()
        let started = Counter()
        let blocking = FakeSynthesizer(renderBody: { _, index in
            guard index > 0 else { return tone(amplitude: 0.1, frames: 500) }
            started.bump()
            do { try await Task.sleep(nanoseconds: 60_000_000_000) } catch {
                cancelled.flag = true
                throw error
            }
            return tone(amplitude: 0.1, frames: 500)
        })
        for try await _ in pipeline(blocking).chunks(for: "One. Two. Three.") {
            // Leave only once the second render is actually in flight, so there is something to cancel.
            while started.value == 0 { try await Task.sleep(nanoseconds: 5_000_000) }
            break
        }
        for _ in 0..<400 where !cancelled.flag { try await Task.sleep(nanoseconds: 5_000_000) }
        #expect(started.value == 1)
        #expect(cancelled.flag, "dropping the stream must stop the synthesizer, not leave it speaking")
    }

    @Test func synthesizeStitchesTheChunksWithAManifest() async throws {
        let result = try await pipeline().synthesize("One. Two. Three.")
        #expect(result.manifest.map(\.text) == ["One.", "Two.", "Three."])
        #expect(result.format == PCMFormat(sampleRate: 22_050))
        #expect(result.audio.count == result.frameCount * MemoryLayout<Float>.size)
        #expect(result.manifest.first?.startFrame == 0)
        let starts = result.manifest.map(\.startFrame)
        #expect(starts == starts.sorted() && Set(starts).count == 3)
        let last = try #require(result.manifest.last)
        #expect(last.startFrame + last.durationFrames == result.frameCount)
        #expect(result.frameCount <= 3 * 2_205, "seams overlap, so the stitched stream is no longer than the parts")
    }

    @Test func stitchCrossfadesAtTheSeamAndKeepsTheTimeline() {
        let left: [Float] = [1, 1, 1, 1, 0.5, 0, 0, 0]
        let right: [Float] = [0, 0, 0.5, 1, 1, 1, 1, 1]
        let stitched = SynthesisPipeline.stitch([left, right])
        #expect(stitched.timings.count == 2)
        #expect(stitched.timings[0] == .init(startFrame: 0, durationFrames: 8))
        #expect(stitched.samples.count == 8 + 8 - (8 - stitched.timings[1].startFrame))
        #expect(stitched.samples.prefix(stitched.timings[1].startFrame) == left.prefix(stitched.timings[1].startFrame))
        #expect(SynthesisPipeline.stitch([]).samples.isEmpty)
        #expect(SynthesisPipeline.stitch([left]).samples == left)
    }

    @Test func chunksConvertToPlayableBuffers() throws {
        let chunk = AudioChunk(index: 0, count: 1, text: "Hi.", sourceRange: 0 ..< 3, samples: tone(amplitude: 0.1, frames: 441), format: PCMFormat(sampleRate: 22_050))
        let buffer = try #require(chunk.makeBuffer())
        #expect(buffer.frameLength == 441)
        #expect(buffer.format.sampleRate == 22_050 && buffer.format.channelCount == 1)
        #expect(abs(chunk.duration - 0.02) < 1e-9)
        let empty = AudioChunk(index: 0, count: 1, text: "", sourceRange: 0 ..< 0, samples: [], format: PCMFormat(sampleRate: 22_050))
        #expect(empty.makeBuffer() == nil)
    }

    @Test func resamplingKeepsTheDurationAndTheEndpoints() {
        let input: [Float] = (0..<100).map { Float($0) / 99 }
        let up = PCMResampler.linear(input, from: 16_000, to: 22_050)
        #expect(up.count == Int((100 * 22_050.0 / 16_000).rounded()))
        #expect(abs(up.first! - 0) < 1e-6 && abs(up.last! - 1) < 0.02)
        #expect(PCMResampler.linear(input, from: 16_000, to: 16_000) == input)
    }

    @Test func installedVoicesAreOrderedBestFirstAndFilterableByLanguage() {
        let all = VoiceInfo.installed()
        let qualities = all.map(\.quality)
        #expect(qualities == qualities.sorted(by: >))
        #expect(VoiceInfo.installed(language: "xx-nonexistent").isEmpty)
        for voice in VoiceInfo.installed(language: "en") { #expect(voice.language.lowercased().hasPrefix("en")) }
    }

    @Test func configurationClampsNonsensicalValues() {
        let configuration = SynthesisPipeline.Configuration(maxInputCharacters: 0, maxSegmentCharacters: -4, sampleRate: 0, targetRMS: 9, maximumGainDB: -3, speechRate: 5)
        #expect(configuration.maxInputCharacters == 1 && configuration.maxSegmentCharacters == 1)
        #expect(configuration.sampleRate == 1 && configuration.targetRMS == 1 && configuration.maximumGainDB == 0)
        #expect(configuration.speechRate == AVSpeechUtteranceMaximumSpeechRate)
    }
}

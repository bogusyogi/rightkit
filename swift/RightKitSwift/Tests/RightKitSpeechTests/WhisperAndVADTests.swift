import Foundation
import Testing

@testable import RightKitSpeech

private let hasWhisper = Fixtures.whisperModel != nil && Fixtures.hasSay

@Suite(.serialized)
struct WhisperEndToEndTests {
    @Test(.enabled(if: hasWhisper))
    func transcribesSynthesizedSpeechThroughRealCoreMLStages() async throws {
        let dir = try Fixtures.scratchDirectory()
        defer { try? FileManager.default.removeItem(at: dir) }
        let audio = try Fixtures.synthesize("The quick brown fox jumps over the lazy dog near the old stone bridge.", in: dir)
        let engine = try WhisperEngine(modelDirectory: try #require(Fixtures.whisperModel), language: "en")
        let words = Fixtures.words(try await engine.transcribe(fileURL: audio))
        for expected in ["quick", "brown", "fox", "lazy", "dog", "bridge"] {
            #expect(words.contains(expected), "missing \(expected) in \(words)")
        }
    }

    /// The Whisper mel front end is checked against the signal, not against a constant: a tone at a
    /// known frequency must light the matching mel band far above the others, and silence must sit at
    /// the log floor.
    @Test
    func logMelPlacesEnergyInTheRightBand() throws {
        let tone: [Float] = (0..<16_000).map { 0.5 * sin(2 * .pi * 1_000 * Float($0) / 16_000) }
        let mel = try WhisperMelSpectrogram.logMel(windowedSamples: tone)
        #expect(mel.count == 128 * 3000)
        #expect(mel.allSatisfy { $0.isFinite })

        func bandEnergy(_ band: Int) -> Float {  // mean over the first second (100 frames)
            (0..<100).reduce(Float(0)) { $0 + mel[band * 3000 + $1] } / 100
        }
        let peakBand = (0..<128).max { bandEnergy($0) < bandEnergy($1) }!
        // 1 kHz on a 0-8 kHz mel scale lands in the lower third.
        #expect((20...50).contains(peakBand), "1 kHz tone peaked in band \(peakBand)")
        #expect(bandEnergy(peakBand) - bandEnergy(120) > 0.5)

        // After the tone ends (frames 100+ are zero padding) the same band falls to the floor.
        let tail = (200..<300).reduce(Float(0)) { $0 + mel[peakBand * 3000 + $1] } / 100
        #expect(bandEnergy(peakBand) - tail > 0.5)
    }

    @Test(.enabled(if: hasWhisper))
    func tokenizerRoundTripsWhisperSpecials() throws {
        let tokenizer = try WhisperTokenizer(tokenizerURL: try #require(Fixtures.whisperModel).appendingPathComponent("tokenizer.json"))
        #expect(tokenizer.tokenID("<|endoftext|>") == 50_257)
        #expect(tokenizer.tokenID("<|startoftranscript|>") == 50_258)
        #expect(tokenizer.tokenID("<|en|>") == 50_259)
        let hello = try #require(tokenizer.tokenID("Hello"))
        #expect(try tokenizer.decode([50_258, 50_259, hello]) == "Hello")
    }
}

struct VoiceActivityDetectorEndToEndTests {
    /// Real synthesized speech between real silence: the detector must start on speech, hold through
    /// word gaps, end only after sustained quiet, and stay silent on the leading/trailing silence.
    @Test(.enabled(if: Fixtures.hasSay))
    func segmentsRealSpeechFromSilence() throws {
        let dir = try Fixtures.scratchDirectory()
        defer { try? FileManager.default.removeItem(at: dir) }
        let speech = try AudioSamples.loadMono16k(
            from: Fixtures.synthesize("Please schedule the quarterly budget meeting for Thursday afternoon.", in: dir))
        let quiet = (0..<16_000).map { _ in Float.random(in: -0.0008...0.0008) }
        let signal = quiet + speech + quiet + quiet

        let detector = VoiceActivityDetector()
        let frame = 320  // 20 ms
        var events: [(frame: Int, event: VoiceActivityEvent)] = []
        var index = 0
        var position = 0
        while position + frame <= signal.count {
            let event = signal[position..<(position + frame)].withUnsafeBufferPointer { detector.ingest(samples: $0) }
            if event != .speechContinued && event != .silence { events.append((index, event)) }
            position += frame
            index += 1
        }

        let started = events.filter { $0.event == .speechStarted }
        let ended = events.filter { $0.event == .speechEnded }
        let leadingFrames = 16_000 / frame
        let speechFrames = speech.count / frame
        #expect(started.count >= 1, "never detected speech")
        #expect(ended.count >= 1, "never ended speech")
        let first = try #require(started.first)
        let last = try #require(ended.last)
        #expect(first.frame >= leadingFrames - 5, "started inside the leading silence at frame \(first.frame)")
        #expect(first.frame <= leadingFrames + 25, "started too late at frame \(first.frame)")
        #expect(last.frame > leadingFrames + speechFrames - 25, "ended before the speech did at frame \(last.frame)")
        #expect(last.frame < leadingFrames + speechFrames + 60, "failed to end after the speech at frame \(last.frame)")
        #expect(!detector.isSpeechActive)

        detector.reset()
        #expect(detector.ingest(rms: 0) == .silence)
    }
}

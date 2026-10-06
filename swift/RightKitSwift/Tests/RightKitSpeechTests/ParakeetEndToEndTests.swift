import Foundation
import Testing

@testable import RightKitSpeech

private let sentenceA = "The quick brown fox jumps over the lazy dog near the old stone bridge."
private let sentenceB = "Please schedule the quarterly budget meeting for Thursday afternoon."
private let sentenceC = "Remember to bring the signed contract and the printed invoice."

private let hasParakeet = Fixtures.parakeetModel != nil && Fixtures.hasSay

@Suite(.serialized)
struct ParakeetEndToEndTests {
    /// The real MelSpectrogram, AudioEncoder, TextDecoder and Joint Core ML stages decoding real
    /// synthesized speech; validated by the words that come out.
    @Test(.enabled(if: hasParakeet))
    func transcribesSynthesizedSpeechThroughRealCoreMLStages() async throws {
        let dir = try Fixtures.scratchDirectory()
        defer { try? FileManager.default.removeItem(at: dir) }
        let audio = try Fixtures.synthesize(sentenceA, in: dir)

        let engine = try ParakeetEngine(modelDirectory: try #require(Fixtures.parakeetModel))
        let text = try await engine.transcribe(fileURL: audio)
        let words = Fixtures.words(text)

        for expected in ["quick", "brown", "fox", "lazy", "dog", "bridge"] {
            #expect(words.contains(expected), "missing \(expected) in: \(text)")
        }
        let timings = engine.lastTimings
        #expect(timings.tokens > 8)
        #expect(timings.jointCalls >= timings.tokens)
        #expect(timings.predictorCalls > 0)
    }

    /// Audio longer than the model's 15 s window exercises quiet-cut placement, per-window decode
    /// and the text-overlap de-duplication; every sentence must survive the seams exactly once.
    @Test(.enabled(if: hasParakeet))
    func longAudioSurvivesWindowSeams() async throws {
        let dir = try Fixtures.scratchDirectory()
        defer { try? FileManager.default.removeItem(at: dir) }
        let engine = try ParakeetEngine(modelDirectory: try #require(Fixtures.parakeetModel))

        var samples: [Float] = []
        for sentence in [sentenceA, sentenceB, sentenceC, sentenceB, sentenceA] {
            samples += try AudioSamples.loadMono16k(from: Fixtures.synthesize(sentence, in: dir))
            samples += [Float](repeating: 0, count: 8_000)  // 0.5 s pause between sentences
        }
        let seconds = Double(samples.count) / 16_000
        #expect(seconds > 30, "fixture must exceed two decode windows, was \(seconds)s")

        let text = try await engine.transcribe(samples: samples)
        let words = Fixtures.words(text)
        for expected in ["fox", "budget", "thursday", "contract", "invoice", "bridge"] {
            #expect(words.contains(expected), "missing \(expected) in: \(text)")
        }
        #expect(words.filter { $0 == "invoice" }.count == 1, "single occurrence must not duplicate at a seam: \(text)")
        #expect(words.filter { $0 == "budget" }.count == 2, "both budget sentences must survive: \(text)")
        #expect(words.filter { $0 == "fox" }.count == 2)
    }

    /// Context bias is a vocabulary phrase encoded against the model's own pieces; with it the
    /// decode must still produce the clean transcript (bias never corrupts unrelated words).
    @Test(.enabled(if: hasParakeet))
    func vocabularyBiasKeepsTranscriptIntact() async throws {
        let dir = try Fixtures.scratchDirectory()
        defer { try? FileManager.default.removeItem(at: dir) }
        let audio = try Fixtures.synthesize("Ask Zephyr to review the quarterly budget.", in: dir)
        let engine = try ParakeetEngine(modelDirectory: try #require(Fixtures.parakeetModel))
        engine.vocabulary = ["Zephyr"]
        let text = try await engine.transcribe(fileURL: audio)
        let words = Fixtures.words(text)
        #expect(words.contains("zephyr"), "biased phrase absent: \(text)")
        #expect(words.contains("budget"))
        #expect(words.contains("quarterly"))
    }

    /// Greedy and beam must both decode the same utterance; the beam width is a real tunable.
    @Test(.enabled(if: hasParakeet))
    func greedyAndBeamBothDecode() async throws {
        let dir = try Fixtures.scratchDirectory()
        defer { try? FileManager.default.removeItem(at: dir) }
        let audio = try Fixtures.synthesize(sentenceB, in: dir)
        var greedy = ParakeetOptions()
        greedy.beamWidth = 1
        let engine = try ParakeetEngine(modelDirectory: try #require(Fixtures.parakeetModel), options: greedy)
        let text = try await engine.transcribe(fileURL: audio)
        for expected in ["schedule", "quarterly", "budget", "thursday"] {
            #expect(Fixtures.words(text).contains(expected), "greedy missing \(expected): \(text)")
        }
    }

    /// `validate` loads every stage and writes the ready marker the app gates dictation on. Run on a
    /// symlinked copy so the donor model directory stays untouched.
    @Test(.enabled(if: hasParakeet))
    func validationWritesReadyMarkerAndEnablesGatedLoad() throws {
        let dir = try Fixtures.scratchDirectory()
        defer { try? FileManager.default.removeItem(at: dir) }
        let source = try #require(Fixtures.parakeetModel)
        for name in try FileManager.default.contentsOfDirectory(atPath: source.path)
        where name.hasSuffix(".mlmodelc") || name == "pipeline.json" || name == "vocab.txt" {
            try FileManager.default.createSymbolicLink(
                at: dir.appendingPathComponent(name), withDestinationURL: source.appendingPathComponent(name))
        }
        #expect(!ParakeetEngine.isReady(modelDirectory: dir))
        #expect(throws: SpeechError.modelNotReady) {
            _ = try ParakeetEngine(modelDirectory: dir, requireReadyMarker: true)
        }

        let report = try ParakeetEngine.validate(modelDirectory: dir)
        #expect(report.stages == ["MelSpectrogram", "AudioEncoder", "TextDecoder", "Joint"])
        #expect(report.isTDT)
        #expect(report.vocabularySize > 8000)
        #expect(ParakeetEngine.isReady(modelDirectory: dir))
        _ = try ParakeetEngine(modelDirectory: dir, requireReadyMarker: true)

        // A broken artifact is rejected by name, not with a Core ML stack trace.
        try FileManager.default.removeItem(at: dir.appendingPathComponent("Joint.mlmodelc"))
        #expect(throws: SpeechError.missingStage("Joint")) { _ = try ParakeetEngine.validate(modelDirectory: dir) }
    }
}

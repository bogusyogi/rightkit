import Foundation
import Testing

@testable import RightKitSpeech

/// S01: `ParakeetEngine` is checked `Sendable`. These tests drive its real async and blocking entry
/// points through a recording decoder, so they need no Core ML model.
struct ParakeetConcurrencyTests {
    /// Not Sendable on purpose: it is only legal to touch it through the engine's gate. Its counters
    /// are unsynchronised, so any overlap the gate admits shows up as `maxInFlight > 1` (or as a data
    /// race under `-sanitize=thread`).
    private final class RecordingDecoder: ParakeetDecoding {
        let timings = ParakeetCoreMLRuntime.TimingsBox()
        private(set) var inFlight = 0
        private(set) var maxInFlight = 0
        private(set) var calls = 0
        private(set) var tornVocabularies = 0
        private(set) var primes = 0

        private func decode(_ vocabulary: [String]) -> String {
            inFlight += 1
            maxInFlight = max(maxInFlight, inFlight)
            calls += 1
            // A vocabulary written as `n` copies of one phrase must be observed whole.
            if let first = vocabulary.first, vocabulary.contains(where: { $0 != first }) { tornVocabularies += 1 }
            Thread.sleep(forTimeInterval: 0.0003)  // widen the overlap window; no ordering depends on it
            inFlight -= 1
            return vocabulary.first ?? ""
        }

        func transcribe(audioURL: URL, vocabulary: [String]) throws -> String { decode(vocabulary) }
        func transcribe(samples: [Float], vocabulary: [String]) throws -> String { decode(vocabulary) }
        func transcribeStaticWindow(samples: [Float], vocabulary: [String], alreadyConditioned: Bool) throws -> String {
            decode(vocabulary)
        }
        func prime() throws { _ = decode([]); primes += 1 }
        /// Read after all work finished.
        var snapshot: (max: Int, calls: Int, torn: Int) { (maxInFlight, calls, tornVocabularies) }
    }

    private func makeEngine(_ decoder: RecordingDecoder) -> ParakeetEngine {
        ParakeetEngine(modelDirectory: URL(fileURLWithPath: "/nonexistent"), options: ParakeetOptions(), decoding: decoder)
    }

    @Test func engineIsCheckedSendable() {
        let engine = makeEngine(RecordingDecoder())
        let sendable: any Sendable = engine  // would not compile if Sendable were removed or unchecked-only
        _ = sendable
    }

    @Test func concurrentAsyncBlockingAndVocabularyWritesAreSerializedAndNeverTorn() async throws {
        let decoder = RecordingDecoder()
        let engine = makeEngine(decoder)
        let samples = [Float](repeating: 0, count: 16)

        try await withThrowingTaskGroup(of: Void.self) { group in
            for writer in 0..<4 {
                group.addTask {
                    for round in 0..<200 {
                        let size = (round % 9) + 1
                        engine.vocabulary = [String](repeating: "w\(writer)-\(round)", count: size)
                    }
                }
            }
            for _ in 0..<6 {
                group.addTask {
                    for _ in 0..<20 { _ = try await engine.transcribe(samples: samples) }
                }
                group.addTask {
                    for _ in 0..<20 { _ = try await engine.transcribe(fileURL: URL(fileURLWithPath: "/nonexistent.wav")) }
                }
            }
            for _ in 0..<4 {
                group.addTask {
                    // Blocking callers run on their own threads, as an app worker queue would.
                    try await Task.detached {
                        for _ in 0..<20 {
                            _ = try engine.transcribeBlocking(samples: samples)
                            _ = try engine.transcribeStaticWindow(samples: samples)
                        }
                        try engine.prime()
                    }.value
                }
            }
            try await group.waitForAll()
        }

        let result = decoder.snapshot
        #expect(result.max == 1, "decodes overlapped on the shared runtime")
        #expect(result.calls == 6 * 20 + 6 * 20 + 4 * 41)  // async samples + async file + (blocking, static window, prime)
        #expect(result.torn == 0, "a decode observed a half-written vocabulary")
    }

    @Test func aDecodeUsesTheVocabularyCurrentWhenItWasCalled() async throws {
        let decoder = RecordingDecoder()
        let engine = makeEngine(decoder)
        engine.vocabulary = ["first"]
        #expect(try engine.transcribeBlocking(samples: []) == "first")
        #expect(try engine.transcribeBlocking(samples: [], vocabulary: ["override"]) == "override")
        engine.vocabulary = ["second"]
        #expect(try await engine.transcribe(samples: []) == "second")
        #expect(engine.vocabulary == ["second"])
    }
}

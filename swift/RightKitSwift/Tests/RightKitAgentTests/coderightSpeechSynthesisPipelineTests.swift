import AVFoundation
import XCTest
@testable import RightKitAgent
@MainActor
final class SpeechSynthesisPipelineTests: XCTestCase {
    func testChunkTextPreservesUTF8SourceRanges() {
        let text = "  Hello 👋. Next sentence here.  "
        let segments = SpeechSynthesisPipeline.chunkText(text, maxCharacters: 24)

        XCTAssertEqual(
            segments.map(\.text),
            ["Hello 👋. ", "Next sentence here."]
        )
        XCTAssertEqual(segments.map(\.sourceRange), [2 ..< 14, 14 ..< 33])

        let bytes = Array(text.utf8)
        for segment in segments {
            XCTAssertEqual(
                String(decoding: bytes[segment.sourceRange], as: UTF8.self),
                segment.text
            )
        }
    }

    func testNativeSynthesisProducesFloat32MonoAudioAndManifest() async throws {
        guard AVSpeechSynthesisVoice(language: "en-US") != nil else {
            throw XCTSkip("The test host has no en-US system speech voice.")
        }

        let pipeline = SpeechSynthesisPipeline(
            configuration: .init(maxSegmentCharacters: 50)
        )
        let result = try await pipeline.synthesize(
            "This is a short native speech acceptance. It has two sentences."
        )

        XCTAssertFalse(result.audio.isEmpty)
        XCTAssertGreaterThan(result.frameCount, 0)
        XCTAssertEqual(result.audio.count, result.frameCount * MemoryLayout<Float>.size)
        XCTAssertEqual(result.format.channels, 1)
        XCTAssertEqual(result.format.bitsPerSample, 32)
        XCTAssertTrue(result.format.isFloat)
        XCTAssertFalse(result.manifest.isEmpty)
        XCTAssertEqual(result.manifest.count, 2)
        XCTAssertEqual(result.manifest.map(\.sourceRange), [0 ..< 42, 42 ..< 63])
        XCTAssertTrue(result.manifest.allSatisfy { $0.durationFrames > 0 })
    }

    func testNativePlaybackCompletesAndStopReleasesAudio() async throws {
        let playback = SpeechPlaybackController()
        defer { playback.stop() }
        playback.read(text: "Hello from CodeRight.")
        let started = Date().addingTimeInterval(10)
        while !playback.isPlaying && playback.errorMessage == nil && Date() < started {
            try await Task.sleep(for: .milliseconds(100))
        }
        XCTAssertTrue(playback.isPlaying, playback.errorMessage ?? "Playback did not start")
        let finished = Date().addingTimeInterval(15)
        while playback.isReading && Date() < finished { try await Task.sleep(for: .milliseconds(100)) }
        XCTAssertEqual(playback.state, .idle)
        XCTAssertNil(playback.errorMessage)
        playback.read(text: String(repeating: "This sentence keeps playback active for the stop check. ", count: 6))
        let restarted = Date().addingTimeInterval(10)
        while !playback.isPlaying && playback.errorMessage == nil && Date() < restarted {
            try await Task.sleep(for: .milliseconds(100))
        }
        XCTAssertTrue(playback.isPlaying, playback.errorMessage ?? "Playback did not restart")
        playback.stop()
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertEqual(playback.state, .idle)
        XCTAssertNil(playback.errorMessage)
    }
}

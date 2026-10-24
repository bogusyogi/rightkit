import Foundation
import Testing

@testable import RightKitSpeech

struct SpeechEngineSelectorRegistryTests {
    private func message(_ body: () throws -> Void) -> String? {
        do { try body(); return nil } catch let SpeechError.engineUnavailable(text) { return text } catch { return nil }
    }

    @Test func theAttributionTagsStayStable() {
        #expect(SpeechEngineKind.parakeet.rawValue == "parakeet")
        #expect(SpeechEngineKind.whisper.rawValue == "whisper")
        #expect(SpeechEngineKind.allCases == [.parakeet, .whisper])
    }

    @Test func fallbackReportsEveryEngineThatCannotRun() throws {
        let text = try #require(message { _ = try SpeechEngineSelector().resolveWithFallback(.parakeet) })
        #expect(text.contains("parakeet"))
        #expect(text.contains("whisper"))
    }
}

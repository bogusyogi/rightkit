import Foundation
import Testing

@testable import RightKitSpeech

struct SpeechEngineSelectorRegistryTests {
    private struct FakeApple: SpeechTranscribing {
        var kind: SpeechEngineKind { .apple }
        func transcribe(fileURL: URL) async throws -> String { "fake" }
    }

    private func message(_ body: () throws -> Void) -> String? {
        do { try body(); return nil } catch let SpeechError.engineUnavailable(text) { return text } catch { return nil }
    }

    @Test func anUnregisteredAppleEngineIsUnavailableAndNamedAsSuch() throws {
        let text = try #require(message { _ = try SpeechEngineSelector().resolve(.apple) })
        #expect(text.contains("apple"))
        #expect(text.contains("not linked"))
    }

    @Test func fallbackSkipsAnUnregisteredAppleEngineAndReportsIt() throws {
        let text = try #require(message { _ = try SpeechEngineSelector().resolveWithFallback(.apple) })
        #expect(text.contains("apple: not linked"))
    }

    @Test func aRegisteredFactoryIsUsedForApple() throws {
        var selector = SpeechEngineSelector()
        selector.registeredEngines[.apple] = { FakeApple() }
        #expect(try selector.resolve(.apple).kind == .apple)
        #expect(try selector.resolveWithFallback(.apple).kind == .apple)
    }

    @Test func theAttributionTagStaysStable() {
        #expect(SpeechEngineKind.apple.rawValue == "apple")
        #expect(SpeechEngineKind.allCases.contains(.apple))
    }
}

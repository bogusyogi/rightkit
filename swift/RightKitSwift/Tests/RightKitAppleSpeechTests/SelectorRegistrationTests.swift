import Foundation
import Testing

import RightKitAppleSpeech
import RightKitSpeech

struct SelectorRegistrationTests {
    @Test func registeringAppleSpeechLetsTheSelectorBuildIt() throws {
        var selector = SpeechEngineSelector()
        selector.registerAppleSpeech()
        let engine = try selector.resolve(.apple)
        #expect(engine.kind == .apple)
        #expect(engine is AppleSpeechEngine)
    }

    @Test func theRegisteredLocaleReachesTheEngine() throws {
        var selector = SpeechEngineSelector()
        selector.registerAppleSpeech(locale: Locale(identifier: "fr_FR"))
        let engine = try #require(try selector.resolve(.apple) as? AppleSpeechEngine)
        #expect(engine.locale.identifier == "fr_FR")
    }
}

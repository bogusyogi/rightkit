#if canImport(Speech) // Speech is unavailable on watchOS; RightKitAudio carries the watch recorder
import Foundation
import RightKitSpeech

public extension SpeechEngineSelector {
    /// Lets this selector build `.apple`. Without it `resolve(.apple)` throws and `resolveWithFallback`
    /// skips Apple Speech: RightKitSpeech itself does not link Speech.framework.
    mutating func registerAppleSpeech(locale: Locale = Locale(identifier: "en_US")) {
        registeredEngines[.apple] = { AppleSpeechEngine(locale: locale) }
    }
}
#endif

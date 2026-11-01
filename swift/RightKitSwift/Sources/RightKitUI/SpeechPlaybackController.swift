import AVFoundation
import Combine
import Foundation

@MainActor
final class SpeechPlaybackController: NSObject, ObservableObject, AVSpeechSynthesizerDelegate {
    @Published private(set) var isSpeaking = false
    private let synthesizer = AVSpeechSynthesizer()

    override init() {
        super.init()
        synthesizer.delegate = self
    }

    func speak(_ text: String, language: String = Locale.current.language.languageCode?.identifier ?? "en-US") {
        stop()
        let utterance = AVSpeechUtterance(string: text)
        utterance.voice = AVSpeechSynthesisVoice(language: language)
        utterance.rate = AVSpeechUtteranceDefaultSpeechRate
        synthesizer.speak(utterance)
        isSpeaking = true
    }

    func pause() { synthesizer.pauseSpeaking(at: .immediate); isSpeaking = false }
    func resume() { synthesizer.continueSpeaking(); isSpeaking = true }
    func stop() { synthesizer.stopSpeaking(at: .immediate); isSpeaking = false }

    nonisolated func speechSynthesizer(_ synthesizer: AVSpeechSynthesizer, didFinish utterance: AVSpeechUtterance) {
        Task { @MainActor in self.isSpeaking = false }
    }
}

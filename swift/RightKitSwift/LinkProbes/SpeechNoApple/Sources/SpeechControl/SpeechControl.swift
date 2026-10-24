import Speech

/// Positive control for the link probe: a target that really links Speech.framework, so the scanner is proven able to see it.
public enum SpeechControl {
    public static func recognizerAvailable() -> Bool {
        SFSpeechRecognizer(locale: Locale(identifier: "en_US"))?.isAvailable ?? false
    }
}

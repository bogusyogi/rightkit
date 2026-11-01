import Foundation

enum AudioCaptureRecoveryPolicy {
    static let failedInitializationCode = -1_080_000_240
    static let routeSettlingCode = 2_003_329_396
    // AVAudioSessionErrorCodeCannotStartPlaying ('!pla'). This reports
    // activation refusal, not which app or service owns audio priority.
    // Treat it as retryable without attributing it to screen recording.
    static let sessionActivationContendedCode = 561_015_905
    /// AVAudioSessionErrorCodeCannotInterruptOthers ('!int'). Raised by
    /// `setActive(true)` when a NON-mixable session is activated while the
    /// app is backgrounded. Every background-originated
    /// start is unconditionally mixable, so seeing this code no longer means
    /// "another app is busy" — it means OUR audio policy is wrong or stale
    /// (a stale category still on the session, or the wrong
    /// foreground/background classification). The recovery is to reconfigure
    /// and assert the options that actually landed, not to retry an
    /// identical configuration; `isSessionActivationError` therefore routes
    /// it to a full session rebuild.
    static let cannotInterruptOthersCode = 560_557_684

    static func isRetryable(_ error: NSError) -> Bool {
        // AVFAudio has emitted route-settling failures under both
        // NSOSStatusErrorDomain & com.apple.coreaudio.avfaudio. Code identifies
        // mechanism; domain does not reliably identify it across iOS releases.
        if error.code == failedInitializationCode
            || error.code == routeSettlingCode
            || error.code == sessionActivationContendedCode
            || error.code == cannotInterruptOthersCode {
            return true
        }
        let description = error.description.lowercased()
        return description.contains("format") || description.contains("input")
    }

    /// A user-facing reason for a *terminal* capture-start failure (i.e. one
    /// the bounded retry loop in `startCaptureWithRouteRecovery` could not
    /// recover from). Session codes identify the failed operation, not the
    /// competing client. Give a recovery action without inventing that cause.
    /// Unknown platform errors stay in content-free diagnostics;
    /// UI gets a stable recovery action rather than implementation details.
    static func userFacingFailureReason(_ error: Error) -> String {
        // `RecordingAudioPolicy.PolicyError.bluetoothInputNotSelected` lives
        // in the HeardRight app target only (it needs the App Group
        // UserDefaults switch), while this file also compiles into
        // Widgets. Matching the case name via `String(describing:)`
        // avoids importing that type here and keeps this file target-agnostic.
        // A capture path that started but delivers no PCM. Same target-
        // agnostic matching as below: the error carries a payload, so match
        // the case name by prefix.
        if String(describing: error).hasPrefix("capturePathDead") {
            return "The microphone started but no audio arrived. Reconnect your headset and try again."
        }
        if String(describing: error) == "phoneMicrophoneNotSelected" {
            return "The iPhone microphone could not be selected. Pause music and try again."
        }
        if String(describing: error) == "bluetoothInputNotSelected" {
            return "The Bluetooth microphone could not be selected. Reconnect your headset and try again."
        }
        if String(describing: error) == "sessionNotActivated" {
            return "The audio session could not be activated. Open HeardRight and try recording again."
        }
        let nsError = error as NSError
        switch nsError.code {
        case sessionActivationContendedCode:
            return "The microphone could not start. Open HeardRight and try recording again."
        case failedInitializationCode, routeSettlingCode:
            return "Audio input could not start. Check your microphone connection and try again."
        case cannotInterruptOthersCode:
            return "The microphone could not start in the background. Open HeardRight and try again."
        default:
            return "The microphone could not start. Check microphone access and the selected audio route, then try again."
        }
    }
}

#if os(iOS)
import SwiftUI
import UIKit

// MARK: - Haptic helpers

/// Lightweight haptic feedback that respects the system "mute haptics" preference.
///
/// One generator is allocated up front and `prepare()`-ed so first-tap latency is
/// minimal. Marked `@MainActor` because `UIImpactFeedbackGenerator` is documented as
/// not safe to use off the main thread.
@MainActor
enum HapticFeedback {
    /// Tap used to confirm a discrete action (start record, button press).
    static let impact = UIImpactFeedbackGenerator(style: .medium)

    /// Notification feedback keyed to a recording lifecycle event.
    static let recordingNotification = UINotificationFeedbackGenerator()

    /// Pre-warm generators so the first event does not have generator-spin-up latency.
    static func prepare() {
        impact.prepare()
        recordingNotification.prepare()
    }

    /// One-shot impact tap. Call from button actions / recording toggle.
    static func tap() {
        impact.impactOccurred()
        impact.prepare()
    }

    /// Recording started.
    static func recordStarted() {
        impact.impactOccurred(intensity: 0.9)
        impact.prepare()
    }

    /// Recording stopped -- couples impact + success notification for a "ding" feel.
    static func recordStopped() {
        impact.impactOccurred(intensity: 0.7)
        impact.prepare()
        recordingNotification.notificationOccurred(.success)
    }

    /// Transcription completed -- success notification, light impact.
    static func transcriptionCompleted() {
        recordingNotification.notificationOccurred(.success)
        recordingNotification.prepare()
    }

    /// Recording cancelled -- warning notification, no impact (user-initiated cancel).
    static func recordCancelled() {
        recordingNotification.notificationOccurred(.warning)
        recordingNotification.prepare()
    }
}

// MARK: - VoiceOver announcement helper

@MainActor
enum VoiceOverAnnouncement {
    /// Post an announcement that is read immediately by VoiceOver without moving focus.
    static func post(_ message: String) {
        guard UIAccessibility.isVoiceOverRunning else { return }
        UIAccessibility.post(notification: .announcement, argument: message)
    }

    /// Post a layout change notification (use when state change should move VO focus
    /// to a new element). Caller passes the new element.
    static func layoutChanged(to element: Any) {
        guard UIAccessibility.isVoiceOverRunning else { return }
        UIAccessibility.post(notification: .layoutChanged, argument: element)
    }

    /// Post a screen-change notification (use between major view swaps).
    static func screenChanged(to element: Any) {
        guard UIAccessibility.isVoiceOverRunning else { return }
        UIAccessibility.post(notification: .screenChanged, argument: element)
    }
}

// MARK: - Reduce-motion / reduce-transparency helpers

extension View {
    /// Wrap a press-down `scaleEffect` so it disappears when Reduce Motion is on.
    /// The view still exposes `hrMotionSafePress` so a ButtonStyle can render
    /// a non-animated "pressed" state (typically via opacity).
    func hrMotionSafePress() -> some View {
        modifier(MotionSafePressModifier())
    }
}

private struct MotionSafePressModifier: ViewModifier {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    func body(content: Content) -> some View {
        content.environment(\.hrMotionSafePress, MotionSafePress(reduceMotion: reduceMotion))
    }
}

struct MotionSafePress {
    let reduceMotion: Bool

    /// Use this inside ButtonStyle.body to drive `scaleEffect` -- returns `1.0`
    /// in both the resting and pressed states when reduce-motion is enabled, so
    /// the style can return identity without branching its own conditional.
    func resolvedScale(isPressed: Bool, pressedValue: CGFloat) -> CGFloat {
        reduceMotion ? 1.0 : (isPressed ? pressedValue : 1.0)
    }
}

private struct MotionSafePressKey: EnvironmentKey {
    static let defaultValue = MotionSafePress(reduceMotion: false)
}

extension EnvironmentValues {
    var hrMotionSafePress: MotionSafePress {
        get { self[MotionSafePressKey.self] }
        set { self[MotionSafePressKey.self] = newValue }
    }
}
#endif

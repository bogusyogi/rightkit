#if os(iOS)
import SwiftUI
import UniformTypeIdentifiers
#if canImport(UIKit)
import UIKit
#endif

/// Small platform facts shared by onboarding and help surfaces. Keep iPad
/// behavior explicit because it has no Action Button or Back Tap.
enum HRDeviceContext {
    static var isPad: Bool {
        #if canImport(UIKit)
        UIDevice.current.userInterfaceIdiom == .pad
        #else
        false
        #endif
    }

    static var isPhone: Bool {
        #if canImport(UIKit)
        UIDevice.current.userInterfaceIdiom == .phone
        #else
        true
        #endif
    }
}

struct PanelStyle: ViewModifier {
    let fill: Color
    let border: Color
    @Environment(\.accessibilityReduceTransparency) var reduceTransparency

    func body(content: Content) -> some View {
        content
            .padding(14)
            .background(fill, in: RoundedRectangle(cornerRadius: 16, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: 16, style: .continuous)
                    .stroke(reduceTransparency ? border : border.opacity(0.9), lineWidth: 1)
            )
            .shadow(color: .black.opacity(reduceTransparency ? 0.08 : 0.24), radius: 24, y: 14)
    }
}

struct SheetSectionStyle: ViewModifier {
    let surface: Color
    let line: Color

    func body(content: Content) -> some View {
        content
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(surface, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous).stroke(line, lineWidth: 1))
    }
}

extension View {
    func panelStyle(_ fill: Color, border: Color) -> some View {
        modifier(PanelStyle(fill: fill, border: border))
    }

    func sheetSectionStyle(surface: Color, line: Color) -> some View {
        modifier(SheetSectionStyle(surface: surface, line: line))
    }
}

enum ActiveSheet: Identifiable {
    case history
    case settings
    case textRules
    case polish
    case about
    case voiceCommands

    var id: String {
        switch self {
        case .history:
            return "history"
        case .settings:
            return "settings"
        case .textRules:
            return "text-rules"
        case .polish:
            return "polish"
        case .about:
            return "about"
        case .voiceCommands:
            return "voice-commands"
        }
    }
}

struct VocabularyImportStatus: Identifiable, Equatable {
    let id = UUID()
    let title: String
    let message: String
}

struct TranscriptEditAlert: Identifiable, Equatable {
    let id = UUID()
    let title: String
    let message: String
    let canDelete: Bool
}

struct HRPrimaryButtonStyle: ButtonStyle {
    let disabled: Bool
    let ink: Color
    let ember: Color
    @Environment(\.hrMotionSafePress) var motionSafePress

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.subheadline.weight(.bold))
            .foregroundStyle(disabled ? ink.opacity(0.58) : ink)
            .padding(.horizontal, 12)
            .frame(minHeight: 44)
            .background(disabled ? Color.white.opacity(0.08) : ember, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
            .scaleEffect(motionSafePress.resolvedScale(isPressed: configuration.isPressed, pressedValue: 0.98))
            .opacity(configuration.isPressed && !motionSafePress.reduceMotion ? 0.92 : 1)
    }
}

struct HRSecondaryButtonStyle: ButtonStyle {
    let paper: Color
    let coffee: Color
    let border: Color
    @Environment(\.accessibilityReduceTransparency) var reduceTransparency
    @Environment(\.hrMotionSafePress) var motionSafePress

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.caption.weight(.bold))
            .foregroundStyle(paper)
            .padding(.horizontal, 11)
            .frame(minHeight: 44)
            .background(coffee, in: RoundedRectangle(cornerRadius: 9, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: 9, style: .continuous)
                    .stroke(reduceTransparency ? border : border.opacity(0.9), lineWidth: 1)
            )
            .scaleEffect(motionSafePress.resolvedScale(isPressed: configuration.isPressed, pressedValue: 0.97))
    }
}

/// Dedicated style for the main record button. The background fill and stroke are
/// applied by the call site (so they can react to `dictation.isRecording` /
/// `dictation.isTranscribing`); this style only controls label + press feedback.
/// `paper` is the label color used in both resting and pressed states (the recording
/// background switches to darker red, so a darker pressed label would clash).
struct HRAccessibleRecordButtonStyle: ButtonStyle {
    let paper: Color
    @Environment(\.hrMotionSafePress) var motionSafePress

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.title3.weight(.bold))
            .foregroundStyle(configuration.isPressed ? paper.opacity(0.78) : paper)
            .frame(maxWidth: .infinity, minHeight: 56)
            .padding(.vertical, 17)
            .scaleEffect(motionSafePress.resolvedScale(isPressed: configuration.isPressed, pressedValue: 0.98))
    }
}

struct HRTextFieldStyle: TextFieldStyle {
    let paper: Color
    let line: Color
    let ink: Color

    // swiftlint:disable:next identifier_name - SwiftUI TextFieldStyle protocol requirement
    func _body(configuration: TextField<Self._Label>) -> some View {
        configuration
            .font(.subheadline.weight(.semibold))
            .foregroundStyle(ink)
            .padding(.horizontal, 11)
            .frame(minHeight: 42)
            .background(paper, in: RoundedRectangle(cornerRadius: 11, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 11, style: .continuous).stroke(line, lineWidth: 1))
    }
}

struct HRChipButtonStyle: ButtonStyle {
    let ink: Color
    let paper: Color
    let line: Color
    @Environment(\.accessibilityReduceTransparency) var reduceTransparency
    @Environment(\.hrMotionSafePress) var motionSafePress

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.caption.weight(.bold))
            .foregroundStyle(ink)
            .lineLimit(1)
            .minimumScaleFactor(0.8)
            .padding(.horizontal, 10)
            .frame(minHeight: 44)
            .frame(maxWidth: .infinity)
            .background(paper, in: Capsule())
            .overlay(Capsule().stroke(reduceTransparency ? line : line.opacity(0.9), lineWidth: 1))
            .scaleEffect(motionSafePress.resolvedScale(isPressed: configuration.isPressed, pressedValue: 0.97))
    }
}

enum ContentViewLaunchOverride {
    static var requestedSheet: ActiveSheet? {
        #if DEBUG
        let arguments = ProcessInfo.processInfo.arguments
        guard let index = arguments.firstIndex(of: "--open-sheet"),
              arguments.indices.contains(arguments.index(after: index)) else { return nil }
        switch arguments[arguments.index(after: index)] {
        case "history": return .history
        case "words": return .textRules
        case "polish": return .polish
        case "commands": return .voiceCommands
        case "settings": return .settings
        default: return nil
        }
        #else
        return nil
        #endif
    }

    static var previewSystemSurfaces: Bool {
        #if DEBUG
        ProcessInfo.processInfo.arguments.contains("--preview-recording-system-surfaces")
        #else
        false
        #endif
    }

    static var forceHome: Bool {
        #if DEBUG
        ProcessInfo.processInfo.arguments.contains("--force-home")
        #else
        false
        #endif
    }

    /// Hidden-QA override: render the in-app recording pill and the dock's
    /// recording state without a live capture session (no model needed).
    static var previewRecordingPill: Bool {
        #if DEBUG
        ProcessInfo.processInfo.arguments.contains("--preview-recording-pill")
        #else
        false
        #endif
    }

    static var resetOnboarding: Bool {
        #if DEBUG
        ProcessInfo.processInfo.arguments.contains("--reset-onboarding")
        #else
        false
        #endif
    }

    static var completeOnboarding: Bool {
        #if DEBUG
        ProcessInfo.processInfo.arguments.contains("--complete-onboarding")
        #else
        false
        #endif
    }

    static func applyOnboardingStateIfRequested() {
        #if DEBUG
        if resetOnboarding {
            UserDefaults.standard.set(false, forKey: "heardright.onboarding.complete")
        }
        if completeOnboarding {
            UserDefaults.standard.set(true, forKey: "heardright.onboarding.complete")
        }
        #endif
    }

    static func applyShortcutQueueSeedIfRequested(store: SharedTranscriptStore = SharedTranscriptStore()) {
        #if DEBUG
        let arguments = ProcessInfo.processInfo.arguments
        if arguments.contains("--seed-latest-transcript") {
            let seeded = [
                "Shortcut smoke latest transcript.",
                "Testing hands-free dictation with John.",
                "Summarize the meeting notes after lunch.",
                "Remind me to send the revised note tomorrow morning.",
            ]
            store.clearTranscripts()
            for text in seeded.reversed() {
                store.saveLatestTranscript(text, polishedText: text)
            }
        }
        guard let action = shortcutActionArgument(in: arguments) else {
            _ = store.takePendingShortcutAction()
            return
        }
        store.savePendingShortcutAction(action)
        #endif
    }

    static func shortcutActionArgument(in arguments: [String]) -> HeardRightShortcutAction? {
        #if DEBUG
        if let flagIndex = arguments.firstIndex(of: "--queue-shortcut"),
           arguments.indices.contains(arguments.index(after: flagIndex)) {
            return HeardRightShortcutAction(rawValue: arguments[arguments.index(after: flagIndex)])
        }
        if let raw = arguments.first(where: { $0.hasPrefix("--queue-shortcut=") })?.split(separator: "=", maxSplits: 1).last {
            return HeardRightShortcutAction(rawValue: String(raw))
        }
        return nil
        #else
        return nil
        #endif
    }
}
#endif

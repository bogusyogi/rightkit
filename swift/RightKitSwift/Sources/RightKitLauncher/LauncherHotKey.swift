#if os(macOS)
import Carbon.HIToolbox
import CoreGraphics
import Foundation
import RightKitHotkeys

/// Pulse launcher: which key combination opens it.
enum LauncherHotkeyChoice: String, CaseIterable {
    case optionSpace
    case commandSpace
    case controlSpace
    /// Offered when the person keeps Option+Space for another app (`LauncherHotkeyPriority`).
    case optionShiftSpace
    case controlOptionSpace

    var keyCode: UInt32 { UInt32(kVK_Space) }

    var modifiers: RightKitHotkeyModifiers {
        switch self {
        case .optionSpace: return .option
        case .commandSpace: return .command
        case .controlSpace: return .control
        case .optionShiftSpace: return [.option, .shift]
        case .controlOptionSpace: return [.control, .option]
        }
    }

    /// The same modifiers as event flags, for the event tap that takes the key ahead of other apps.
    var cgFlags: CGEventFlags {
        switch self {
        case .optionSpace: return .maskAlternate
        case .commandSpace: return .maskCommand
        case .controlSpace: return .maskControl
        case .optionShiftSpace: return [.maskAlternate, .maskShift]
        case .controlOptionSpace: return [.maskControl, .maskAlternate]
        }
    }

    var display: String {
        switch self {
        case .optionSpace: return "Option+Space"
        case .commandSpace: return "Command+Space"
        case .controlSpace: return "Control+Space"
        case .optionShiftSpace: return "Option+Shift+Space"
        case .controlOptionSpace: return "Control+Option+Space"
        }
    }
}

/// A key combination typed as text, such as "cmd+opt+k" or "ctrl+shift+1".
/// At least one modifier is required, so a binding never takes plain typing.
/// RightKit has no text parser for combos yet (rightkit#189), so Pulse keeps this one.
typealias LauncherKeyCombo = RightKitHotkeyCombo

extension RightKitHotkeyCombo {
    static func parse(_ text: String) -> RightKitHotkeyCombo? {
        let parts = text.lowercased()
            .split(separator: "+")
            .map { $0.trimmingCharacters(in: .whitespaces) }
        guard parts.count >= 2, let key = parts.last, let code = keyCodes[key] else { return nil }
        var modifiers: RightKitHotkeyModifiers = []
        for part in parts.dropLast() {
            switch part {
            case "cmd", "command": modifiers.insert(.command)
            case "opt", "option", "alt": modifiers.insert(.option)
            case "ctrl", "control": modifiers.insert(.control)
            case "shift": modifiers.insert(.shift)
            default: return nil
            }
        }
        return RightKitHotkeyCombo(keyCode: code, modifiers: modifiers)
    }

    private static let keyCodes: [String: UInt32] = {
        var table: [String: UInt32] = [
            "space": UInt32(kVK_Space),
            "return": UInt32(kVK_Return),
            "tab": UInt32(kVK_Tab),
        ]
        let letters: [(String, Int)] = [
            ("a", kVK_ANSI_A), ("b", kVK_ANSI_B), ("c", kVK_ANSI_C), ("d", kVK_ANSI_D),
            ("e", kVK_ANSI_E), ("f", kVK_ANSI_F), ("g", kVK_ANSI_G), ("h", kVK_ANSI_H),
            ("i", kVK_ANSI_I), ("j", kVK_ANSI_J), ("k", kVK_ANSI_K), ("l", kVK_ANSI_L),
            ("m", kVK_ANSI_M), ("n", kVK_ANSI_N), ("o", kVK_ANSI_O), ("p", kVK_ANSI_P),
            ("q", kVK_ANSI_Q), ("r", kVK_ANSI_R), ("s", kVK_ANSI_S), ("t", kVK_ANSI_T),
            ("u", kVK_ANSI_U), ("v", kVK_ANSI_V), ("w", kVK_ANSI_W), ("x", kVK_ANSI_X),
            ("y", kVK_ANSI_Y), ("z", kVK_ANSI_Z),
            ("0", kVK_ANSI_0), ("1", kVK_ANSI_1), ("2", kVK_ANSI_2), ("3", kVK_ANSI_3),
            ("4", kVK_ANSI_4), ("5", kVK_ANSI_5), ("6", kVK_ANSI_6), ("7", kVK_ANSI_7),
            ("8", kVK_ANSI_8), ("9", kVK_ANSI_9),
        ]
        for (name, code) in letters { table[name] = UInt32(code) }
        return table
    }()
}

/// The one registrar for every Carbon hotkey the notch owns, so launcher, clipboard strip,
/// window and settings shortcuts share a single event handler.
@MainActor
enum NotchHotkeys {
    static let registrar = RightKitHotkeyRegistrar()

    /// The Carbon status a registration failure maps to, for callers that report it.
    static func status(_ error: RightKitHotkeyError) -> OSStatus {
        switch error {
        case .alreadyRegistered, .unavailable: return OSStatus(eventHotKeyExistsErr)
        case .carbon(let status): return status
        }
    }
}

/// Hotkeys by caller id over the shared registrar. Needs no permission and no event tap.
@MainActor
final class LauncherHotKey {
    private var registered: [UInt32: UInt32] = [:]

    /// Binds a combination to an id, replacing any earlier binding with that id.
    /// Returns noErr on success, otherwise the Carbon error.
    func bind(id: UInt32, keyCode: UInt32, modifiers: RightKitHotkeyModifiers, _ action: @escaping () -> Void) -> OSStatus {
        unbind(id)
        do {
            registered[id] = try NotchHotkeys.registrar.register(
                RightKitHotkeyCombo(keyCode: keyCode, modifiers: modifiers)
            ) { event in
                guard event.phase == .pressed else { return }
                LauncherTiming.markHotkey()
                action()
            }
            return noErr
        } catch {
            return NotchHotkeys.status(error)
        }
    }

    func unbind(_ id: UInt32) {
        if let ref = registered.removeValue(forKey: id) { NotchHotkeys.registrar.unregister(ref) }
    }

    /// Removes every binding whose id is at or above `start`.
    func unbind(from start: UInt32) {
        for id in Array(registered.keys) where id >= start { unbind(id) }
    }
}
#endif

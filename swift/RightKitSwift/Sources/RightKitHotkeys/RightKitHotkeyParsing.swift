import Foundation

#if os(macOS)
import Carbon.HIToolbox
import CoreGraphics

extension RightKitHotkeyModifiers {
    /// The same modifiers as event flags, for an event tap that takes the key ahead of other apps.
    public var cgEventFlags: CGEventFlags {
        var flags: CGEventFlags = []
        if contains(.command) { flags.insert(.maskCommand) }
        if contains(.shift) { flags.insert(.maskShift) }
        if contains(.option) { flags.insert(.maskAlternate) }
        if contains(.control) { flags.insert(.maskControl) }
        return flags
    }

    /// Modifiers read from event flags (anything but command, shift, option and control is ignored).
    public init(cgEventFlags flags: CGEventFlags) {
        var result: RightKitHotkeyModifiers = []
        if flags.contains(.maskCommand) { result.insert(.command) }
        if flags.contains(.maskShift) { result.insert(.shift) }
        if flags.contains(.maskAlternate) { result.insert(.option) }
        if flags.contains(.maskControl) { result.insert(.control) }
        self = result
    }
}

extension RightKitHotkeyCombo {
    /// A combo typed as text, such as "cmd+opt+k" or "ctrl+shift+1". Case-insensitive; spaces around
    /// `+` are ignored. At least one modifier is required, so a binding never takes plain typing.
    /// Modifier names: cmd/command, opt/option/alt, ctrl/control, shift. Keys: a-z, 0-9, space,
    /// return, tab, escape/esc, delete, left/right/up/down, f1-f12. Returns nil for anything else.
    public static func parse(_ text: String) -> RightKitHotkeyCombo? {
        let parts = text.lowercased()
            .split(separator: "+", omittingEmptySubsequences: false)
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

    private static var keyCodes: [String: UInt32] {
        var table: [String: UInt32] = [
            "space": UInt32(kVK_Space), "return": UInt32(kVK_Return), "tab": UInt32(kVK_Tab),
            "escape": UInt32(kVK_Escape), "esc": UInt32(kVK_Escape), "delete": UInt32(kVK_Delete),
            "left": UInt32(kVK_LeftArrow), "right": UInt32(kVK_RightArrow),
            "up": UInt32(kVK_UpArrow), "down": UInt32(kVK_DownArrow),
        ]
        let named: [(String, Int)] = [
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
            ("f1", kVK_F1), ("f2", kVK_F2), ("f3", kVK_F3), ("f4", kVK_F4), ("f5", kVK_F5),
            ("f6", kVK_F6), ("f7", kVK_F7), ("f8", kVK_F8), ("f9", kVK_F9), ("f10", kVK_F10),
            ("f11", kVK_F11), ("f12", kVK_F12),
        ]
        for (name, code) in named { table[name] = UInt32(code) }
        return table
    }
}
#endif

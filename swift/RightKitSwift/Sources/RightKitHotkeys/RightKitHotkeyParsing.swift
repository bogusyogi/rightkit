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
    /// A typed combo with at least one modifier. Case and whitespace around `+` are ignored.
    /// See `RightKitHotkeyText` for canonical text and additional key families.
    public static func parse(_ text: String) -> RightKitHotkeyCombo? {
        RightKitHotkeyText.parse(text)?.combo
    }
}

/// Parsed macOS hotkey text with its Carbon combo and canonical spelling.
public struct RightKitHotkeyText: Equatable, Sendable {
    /// Controls which aliases, separators and key families a settings format accepts.
    public struct Options: OptionSet, Sendable {
        public let rawValue: UInt16
        public init(rawValue: UInt16) { self.rawValue = rawValue }

        public static let trimWhitespace = Self(rawValue: 1 << 0)
        public static let allowModifierAliases = Self(rawValue: 1 << 1)
        public static let ignoreEmptySeparators = Self(rawValue: 1 << 2)
        public static let rejectRepeatedModifiers = Self(rawValue: 1 << 3)
        public static let navigationKeys = Self(rawValue: 1 << 4)
        public static let punctuationKeys = Self(rawValue: 1 << 5)
        public static let escapeKey = Self(rawValue: 1 << 6)
        public static let functionKeys = Self(rawValue: 1 << 7)
    }

    public let combo: RightKitHotkeyCombo
    public let key: String

    /// Modifier order matches the menu's control, option, shift and command order.
    public var canonicalText: String {
        let names = Self.modifierNames.compactMap { name, modifier in
            combo.modifiers.contains(modifier) ? name : nil
        }
        return (names + [key]).joined(separator: "+")
    }

    /// Accepts ASCII letters, number-row digits, Space, Return and Tab, with opt-in key families.
    /// At least one modifier is required so a global binding never takes ordinary typing.
    public static func parse(
        _ text: String,
        options: Options = [.trimWhitespace, .allowModifierAliases, .navigationKeys, .escapeKey, .functionKeys]
    ) -> Self? {
        let parts = text.lowercased().split(
            separator: "+", omittingEmptySubsequences: options.contains(.ignoreEmptySeparators)
        ).map { part in
            options.contains(.trimWhitespace) ? part.trimmingCharacters(in: .whitespaces) : String(part)
        }
        guard parts.count >= 2, let key = parts.last, let match = keyCodes[key],
              options.isSuperset(of: match.requiredOptions)
        else { return nil }

        var modifiers: RightKitHotkeyModifiers = []
        for part in parts.dropLast() {
            let canonical = modifierNames.first { $0.0 == part }?.1
            let alias = options.contains(.allowModifierAliases) ? modifierAliases[part] : nil
            guard let modifier = canonical ?? alias else { return nil }
            if options.contains(.rejectRepeatedModifiers), modifiers.contains(modifier) { return nil }
            modifiers.insert(modifier)
        }
        return Self(combo: RightKitHotkeyCombo(keyCode: match.code, modifiers: modifiers), key: key == "esc" ? "escape" : key)
    }

    private static let modifierNames: [(String, RightKitHotkeyModifiers)] = [
        ("ctrl", .control), ("opt", .option), ("shift", .shift), ("cmd", .command),
    ]

    private static let modifierAliases: [String: RightKitHotkeyModifiers] = [
        "control": .control, "option": .option, "alt": .option, "command": .command,
    ]

    private static let keyCodes: [String: (code: UInt32, requiredOptions: Options)] = {
        let groups: [(Options, [(String, Int)])] = [
            ([], [
                ("a", kVK_ANSI_A), ("b", kVK_ANSI_B), ("c", kVK_ANSI_C), ("d", kVK_ANSI_D),
                ("e", kVK_ANSI_E), ("f", kVK_ANSI_F), ("g", kVK_ANSI_G), ("h", kVK_ANSI_H),
                ("i", kVK_ANSI_I), ("j", kVK_ANSI_J), ("k", kVK_ANSI_K), ("l", kVK_ANSI_L),
                ("m", kVK_ANSI_M), ("n", kVK_ANSI_N), ("o", kVK_ANSI_O), ("p", kVK_ANSI_P),
                ("q", kVK_ANSI_Q), ("r", kVK_ANSI_R), ("s", kVK_ANSI_S), ("t", kVK_ANSI_T),
                ("u", kVK_ANSI_U), ("v", kVK_ANSI_V), ("w", kVK_ANSI_W), ("x", kVK_ANSI_X),
                ("y", kVK_ANSI_Y), ("z", kVK_ANSI_Z), ("0", kVK_ANSI_0), ("1", kVK_ANSI_1),
                ("2", kVK_ANSI_2), ("3", kVK_ANSI_3), ("4", kVK_ANSI_4), ("5", kVK_ANSI_5),
                ("6", kVK_ANSI_6), ("7", kVK_ANSI_7), ("8", kVK_ANSI_8), ("9", kVK_ANSI_9),
                ("space", kVK_Space), ("return", kVK_Return), ("tab", kVK_Tab),
            ]),
            (.navigationKeys, [
                ("left", kVK_LeftArrow), ("right", kVK_RightArrow), ("up", kVK_UpArrow), ("down", kVK_DownArrow),
                ("delete", kVK_Delete),
            ]),
            (.punctuationKeys, [
                ("'", kVK_ANSI_Quote), (",", kVK_ANSI_Comma), ("-", kVK_ANSI_Minus), (".", kVK_ANSI_Period),
                ("/", kVK_ANSI_Slash), (";", kVK_ANSI_Semicolon), ("=", kVK_ANSI_Equal), ("[", kVK_ANSI_LeftBracket),
                ("\\", kVK_ANSI_Backslash), ("]", kVK_ANSI_RightBracket), ("`", kVK_ANSI_Grave),
            ]),
            (.escapeKey, [
                ("escape", kVK_Escape), ("esc", kVK_Escape),
            ]),
            (.functionKeys, [
                ("f1", kVK_F1), ("f2", kVK_F2), ("f3", kVK_F3), ("f4", kVK_F4),
                ("f5", kVK_F5), ("f6", kVK_F6), ("f7", kVK_F7), ("f8", kVK_F8),
                ("f9", kVK_F9), ("f10", kVK_F10), ("f11", kVK_F11), ("f12", kVK_F12),
            ]),
        ]
        var codes: [String: (code: UInt32, requiredOptions: Options)] = [:]
        for (options, entries) in groups {
            for (key, code) in entries { codes[key] = (UInt32(code), options) }
        }
        return codes
    }()
}
#endif

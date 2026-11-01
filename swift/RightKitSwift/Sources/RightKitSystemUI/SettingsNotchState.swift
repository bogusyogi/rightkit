#if os(macOS)
import Foundation
import RightKitHub

// The notch's published state (`notch-state.json`, the JSON the Tauri hub's `notch_state` returns),
// decoded with a tolerant mirror of `NotchState` in hub/src/views/Settings.tsx. Every field the
// notch may omit is optional, and a field that fails to decode on its own never loses the rest.

/// A platform fact the Settings pages branch on, as the React views branch on `isWindows`.
enum SettingsPlatform {
    /// True when this hub is compiled for Windows (SwiftRight).
    static let isWindows: Bool = {
        #if os(Windows)
        return true
        #else
        return false
        #endif
    }()
}

/// Any JSON value. The notch's `settings` hold booleans, numbers, strings and string lists.
enum SettingsJSON: Sendable, Equatable, Codable {
    case null
    case bool(Bool)
    case number(Double)
    case string(String)
    case array([SettingsJSON])
    case object([String: SettingsJSON])

    init(from decoder: any Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() {
            self = .null
        } else if let value = try? container.decode(Bool.self) {
            self = .bool(value)
        } else if let value = try? container.decode(Double.self) {
            self = .number(value)
        } else if let value = try? container.decode(String.self) {
            self = .string(value)
        } else if let value = try? container.decode([SettingsJSON].self) {
            self = .array(value)
        } else if let value = try? container.decode([String: SettingsJSON].self) {
            self = .object(value)
        } else {
            self = .null
        }
    }

    func encode(to encoder: any Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .null: try container.encodeNil()
        case .bool(let value): try container.encode(value)
        case .number(let value): try container.encode(value)
        case .string(let value): try container.encode(value)
        case .array(let value): try container.encode(value)
        case .object(let value): try container.encode(value)
        }
    }

    var boolValue: Bool? {
        if case .bool(let value) = self { return value }
        return nil
    }

    var stringValue: String? {
        if case .string(let value) = self { return value }
        return nil
    }

    var numberValue: Double? {
        if case .number(let value) = self, value.isFinite { return value }
        return nil
    }

    /// A number, bool or string as the text `String(value)` gives in the page: "5", "true", "medium".
    var scalarText: String? {
        switch self {
        case .bool(let value): return value ? "true" : "false"
        case .number(let value): return SettingsFormat.number(value)
        case .string(let value): return value
        default: return nil
        }
    }

    /// A list of strings: a JSON array, or JSON text holding one (the page's `asList`).
    var stringList: [String] {
        switch self {
        case .array(let items):
            return items.map { $0.scalarText ?? "" }
        case .string(let text):
            let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
            if trimmed.isEmpty { return [] }
            if let decoded = try? JSONDecoder().decode([String].self, from: Data(text.utf8)) {
                return decoded
            }
            return []
        default:
            return []
        }
    }

    static func strings(_ list: [String]) -> SettingsJSON {
        .array(list.map { SettingsJSON.string($0) })
    }
}

struct SettingsAppRef: Decodable, Sendable, Equatable {
    var id: String
    var name: String
}

struct SettingsDisplay: Decodable, Sendable, Equatable {
    var id: String
    var name: String
}

/// One permission the notch lists (Settings.tsx `Permission`).
struct SettingsPermission: Decodable, Sendable, Equatable {
    var id: String
    var title: String
    var why: String
    /// "granted", "needsApproval", "off" or "unknown".
    var status: String
    var required: Bool?
    /// The features that need it; nil when the notch does not say.
    var needs: [String]?
}

struct SettingsLimit: Decodable, Sendable, Equatable {
    var label: String?
    var usedFraction: Double?
    var seconds: Double?
}

struct SettingsClaudeWindow: Decodable, Sendable, Equatable {
    var label: String
    var usedFraction: Double
    /// Epoch seconds when the window resets.
    var resetsAt: Double
    var seconds: Double?
}

struct SettingsClaudeAccount: Decodable, Sendable, Equatable, Identifiable {
    var id: String
    var name: String
    var email: String?
    var plan: String?
    var active: Bool?
    var onDisk: Bool?
    /// Epoch seconds of the last reading; nil before the first.
    var capturedAt: Double?
    var windows: [SettingsClaudeWindow]?
}

struct SettingsAccount: Decodable, Sendable, Equatable, Identifiable {
    var id: String
    var name: String
    var connected: Bool?
    var usesKeychain: Bool?
    var refusedAccess: Bool?
    var needsRenewal: Bool?
    var summary: String?
    var signInTitle: String?
    var signInExplanation: String?
    var limits: [SettingsLimit]?
    var lastReadAt: String?
    var lastError: String?
    var notice: String?
    var claudeAccounts: [SettingsClaudeAccount]?
}

struct SettingsGauge: Decodable, Sendable, Equatable, Identifiable {
    var id: String
    var name: String
    var glyph: String?
    var connected: Bool?
    var order: Int?
}

struct SettingsHotkeyState: Decodable, Sendable, Equatable {
    var shortcut: String?
    var display: String?
    var error: String?
}

struct SettingsWindowShortcut: Decodable, Sendable, Equatable, Identifiable {
    var id: String
    var title: String
    var group: String?
    var groupTitle: String?
    /// Shortcut text such as "ctrl+opt+left"; empty when the action has none.
    var shortcut: String?
    var display: String?
    var defaultShortcut: String?
    var error: String?

    enum CodingKeys: String, CodingKey {
        case id, title, group, groupTitle, shortcut, display, error
        case defaultShortcut = "default"
    }
}

struct SettingsWindowManagement: Decodable, Sendable, Equatable {
    var enabled: Bool?
    var actions: [SettingsWindowShortcut]?
}

struct SettingsCutPasteResult: Decodable, Sendable, Equatable {
    var name: String
    var ok: Bool
    var detail: String
}

struct SettingsConveniences: Decodable, Sendable, Equatable {
    var accessibility: Bool?
    var inputMonitoring: Bool?
    var wanted: Bool?
    /// "off", "running", "needsAccessibility" or "failed".
    var fnStatus: String?
    var fnDetail: String?
    var active: Bool?
    var runningApps: [SettingsAppRef]?
    var autoQuitApps: [SettingsAppRef]?
    var cutPasteResults: [SettingsCutPasteResult]?
    var windowManagement: SettingsWindowManagement?
    var settingsHotkey: SettingsHotkeyState?
}

struct SettingsAutoQuit: Decodable, Sendable, Equatable {
    var autoQuitApps: [SettingsAppRef]?
    var runningApps: [SettingsAppRef]?
}

/// Pulse's updater as the notch reports it (Settings.tsx `UpdateState`).
struct SettingsUpdateState: Decodable, Sendable, Equatable {
    var current: String?
    var build: String?
    var available: String?
    var lastChecked: String?
    /// "idle", "checking", "upToDate", "available", "downloading", "installing" or "failed".
    var status: String?
    var message: String?
    var progress: Double?
}

struct SettingsNotchState: Sendable, Equatable {
    var platform: String?
    var permissions: [SettingsPermission]?
    var permissionErrors: [String: String]
    var conveniences: SettingsConveniences?
    var version: String
    var settings: [String: SettingsJSON]
    var options: [String: [String]]
    var displays: [SettingsDisplay]
    var accounts: [SettingsAccount]
    var gauges: [SettingsGauge]?
    var providerOrder: [String]
    var launcherStatus: String?
    var clipboardStatus: String?
    var clipboardCount: Double?
    var clipboardBytes: Double?
    var windowManagement: SettingsWindowManagement?
    var autoQuit: SettingsAutoQuit?
    var settingsHotkey: SettingsHotkeyState?
    var helper: String?
    var helperError: String?
    var updates: SettingsUpdateState?
    /// The hub child's supervision state: `running`, `starting`, `restarting`, `latched: <reason>` or
    /// `not installed` (top level of notch-state.json; absent from an older notch).
    var hubProcess: String?
}

extension SettingsNotchState: Decodable {
    private enum Keys: String, CodingKey {
        case platform, permissions, permissionErrors, conveniences, version, settings, options, displays
        case accounts, gauges, providerOrder, launcherStatus, clipboardStatus, clipboardCount, clipboardBytes
        case windowManagement, autoQuit, settingsHotkey, helper, helperError, updates, hubProcess
    }

    /// Reads one field; a field that is missing, null or of the wrong shape is nil.
    private static func field<T: Decodable>(_ container: KeyedDecodingContainer<Keys>, _ key: Keys) -> T? {
        (try? container.decodeIfPresent(T.self, forKey: key)) ?? nil
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: Keys.self)
        platform = Self.field(container, .platform)
        permissions = Self.field(container, .permissions)
        permissionErrors = Self.field(container, .permissionErrors) ?? [:]
        conveniences = Self.field(container, .conveniences)
        version = Self.field(container, .version) ?? "?"
        settings = Self.field(container, .settings) ?? [:]
        options = Self.field(container, .options) ?? [:]
        displays = Self.field(container, .displays) ?? []
        accounts = Self.field(container, .accounts) ?? []
        gauges = Self.field(container, .gauges)
        providerOrder = Self.field(container, .providerOrder) ?? []
        launcherStatus = Self.field(container, .launcherStatus)
        clipboardStatus = Self.field(container, .clipboardStatus)
        clipboardCount = Self.field(container, .clipboardCount)
        clipboardBytes = Self.field(container, .clipboardBytes)
        windowManagement = Self.field(container, .windowManagement)
        autoQuit = Self.field(container, .autoQuit)
        settingsHotkey = Self.field(container, .settingsHotkey)
        helper = Self.field(container, .helper)
        helperError = Self.field(container, .helperError)
        updates = Self.field(container, .updates)
        hubProcess = Self.field(container, .hubProcess)
    }

    /// Only the `settings` object of a state, for the settings store's diffing.
    static func settings(from data: Data) -> [String: SettingsJSON]? {
        struct Only: Decodable {
            var settings: [String: SettingsJSON]?
        }
        return (try? JSON.decoder.decode(Only.self, from: data))?.settings
    }

    static func decode(_ data: Data) -> SettingsNotchState? {
        try? JSON.decoder.decode(SettingsNotchState.self, from: data)
    }
}

/// Small wording helpers shared by the Settings pages.
enum SettingsFormat {
    /// "5" for 5.0, "0.35" for 0.35.
    static func number(_ value: Double) -> String {
        if value == value.rounded(), abs(value) < 1e15 {
            return String(Int64(value))
        }
        return String(value)
    }

    /// "1.05" with two decimals (the scale row's value).
    static func twoDecimals(_ value: Double) -> String {
        let cents = Int((value * 100).rounded())
        let whole = cents / 100
        let rest = abs(cents % 100)
        return "\(whole).\(rest < 10 ? "0" : "")\(rest)"
    }

    /// "70%" from 0...1.
    static func percent(_ fraction: Double) -> String {
        "\(Int((fraction * 100).rounded()))%"
    }

    /// An engine, platform or decode failure in words.
    static func describe(_ error: any Error) -> String {
        if let engine = error as? EngineError { return engine.message }
        if let platform = error as? HubPlatformError { return platform.description }
        return String(describing: error)
    }
}
#endif

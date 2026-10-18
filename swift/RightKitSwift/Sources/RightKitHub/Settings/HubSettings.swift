import Foundation

// A port of the headless model in `@rightkit/settings-ui` (headless.ts, core.ts, search-core.tsx):
// a registry of settings, each with a key, a title, a description, a documented default, a group and
// search keywords, plus search over them. The model only; storage stays in `rightkit-app-settings`
// behind the engine, and rendering is `HubSettingsView`. Every feature is a setting with a switch
// and a documented default, and every setting is searchable.

/// A stored setting value. Settings are JSON; the hub edits booleans and strings, and keeps
/// numbers and anything else (`other`) without loss of type.
public enum HubSettingValue: Sendable, Equatable, Codable {
    case bool(Bool)
    case string(String)
    case number(Double)
    /// A JSON null, array or object the hub does not edit.
    case other

    public init(from decoder: any Decoder) throws {
        let container = try decoder.singleValueContainer()
        if let value = try? container.decode(Bool.self) {
            self = .bool(value)
        } else if let value = try? container.decode(Double.self) {
            self = .number(value)
        } else if let value = try? container.decode(String.self) {
            self = .string(value)
        } else {
            self = .other
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .bool(let value): try container.encode(value)
        case .string(let value): try container.encode(value)
        case .number(let value): try container.encode(value)
        case .other: try container.encodeNil()
        }
    }

    /// The boolean, or nil for any other type.
    public var boolValue: Bool? {
        if case .bool(let value) = self { return value }
        return nil
    }

    /// The string, or nil for any other type.
    public var stringValue: String? {
        if case .string(let value) = self { return value }
        return nil
    }
}

/// One option of a choice setting.
public struct HubChoice: Sendable, Equatable, Identifiable, Hashable {
    /// The stored value.
    public let value: String
    public var title: String
    public var detail: String?

    public var id: String { value }

    public init(value: String, title: String, detail: String? = nil) {
        self.value = value
        self.title = title
        self.detail = detail
    }
}

/// How a setting is edited.
public enum HubSettingKind: Sendable, Equatable {
    /// An on/off switch. The value is `.bool`.
    case toggle
    /// One of a few options. The value is `.string` holding a choice's `value`.
    case choice([HubChoice])
    /// Free text. The value is `.string`; it is saved on submit.
    case text(placeholder: String)
}

/// One setting. `key` is the engine key (`settings.get` / `settings.set`).
public struct HubSetting: Sendable, Equatable, Identifiable {
    /// The stable engine key, for example `overview.showMenuBar`.
    public let key: String
    public var title: String
    /// What the setting does, in a sentence.
    public var detail: String
    /// The documented default. It applies when the engine holds no value.
    public var defaultValue: HubSettingValue
    public var kind: HubSettingKind
    /// Extra words that find this setting.
    public var keywords: [String]

    public var id: String { key }

    public init(
        key: String,
        title: String,
        detail: String,
        defaultValue: HubSettingValue,
        kind: HubSettingKind,
        keywords: [String] = []
    ) {
        self.key = key
        self.title = title
        self.detail = detail
        self.defaultValue = defaultValue
        self.kind = kind
        self.keywords = keywords
    }

    /// An on/off setting.
    public static func toggle(
        key: String,
        title: String,
        detail: String,
        defaultValue: Bool,
        keywords: [String] = []
    ) -> HubSetting {
        HubSetting(key: key, title: title, detail: detail, defaultValue: .bool(defaultValue), kind: .toggle, keywords: keywords)
    }

    /// A one-of-few setting. `defaultValue` should be one of the choices' values.
    public static func choice(
        key: String,
        title: String,
        detail: String,
        defaultValue: String,
        choices: [HubChoice],
        keywords: [String] = []
    ) -> HubSetting {
        HubSetting(key: key, title: title, detail: detail, defaultValue: .string(defaultValue), kind: .choice(choices), keywords: keywords)
    }

    /// A free-text setting.
    public static func text(
        key: String,
        title: String,
        detail: String,
        defaultValue: String,
        placeholder: String = "",
        keywords: [String] = []
    ) -> HubSetting {
        HubSetting(key: key, title: title, detail: detail, defaultValue: .string(defaultValue), kind: .text(placeholder: placeholder), keywords: keywords)
    }

    /// The default as words, for the description shown to the user.
    public var defaultDescription: String {
        switch defaultValue {
        case .bool(let value):
            return value ? "on" : "off"
        case .string(let value):
            if case .choice(let choices) = kind, let match = choices.first(where: { $0.value == value }) {
                return match.title
            }
            return value.isEmpty ? "empty" : value
        case .number(let value):
            return String(value)
        case .other:
            return "none"
        }
    }
}

/// A titled group of settings, shown as one card.
public struct HubSettingsGroup: Sendable, Equatable, Identifiable {
    public let id: String
    public var title: String
    public var detail: String?
    public var settings: [HubSetting]
    /// Extra words that find every setting in the group.
    public var keywords: [String]

    public init(
        id: String,
        title: String,
        detail: String? = nil,
        settings: [HubSetting],
        keywords: [String] = []
    ) {
        self.id = id
        self.title = title
        self.detail = detail
        self.settings = settings
        self.keywords = keywords
    }
}

/// Search over settings, ported from `parseSearchTerms` and `searchSections`.
public enum HubSettingsSearch {
    /// Lower-cased, whitespace-separated terms. Every term must match somewhere in a setting's text.
    public static func terms(_ query: String) -> [String] {
        query.lowercased().split(whereSeparator: { $0.isWhitespace }).map { String($0) }
    }

    /// The lower-cased text a query is matched against: the group title and keywords, and the
    /// setting's key, title, description and keywords.
    public static func haystack(_ setting: HubSetting, in group: HubSettingsGroup) -> String {
        ([group.title, setting.key, setting.title, setting.detail] + group.keywords + setting.keywords)
            .joined(separator: " ")
            .lowercased()
    }

    /// True when every term appears in the setting's `haystack`. No terms matches everything.
    public static func matches(_ setting: HubSetting, in group: HubSettingsGroup, terms: [String]) -> Bool {
        if terms.isEmpty { return true }
        let text = haystack(setting, in: group)
        return terms.allSatisfy { text.contains($0) }
    }

    /// The groups reduced to the settings that match, dropping groups left empty. An empty query
    /// returns the groups unchanged.
    public static func filter(_ groups: [HubSettingsGroup], query: String) -> [HubSettingsGroup] {
        let parsed = Self.terms(query)
        if parsed.isEmpty { return groups }
        var out: [HubSettingsGroup] = []
        for group in groups {
            var reduced = group
            reduced.settings = group.settings.filter { matches($0, in: group, terms: parsed) }
            if !reduced.settings.isEmpty { out.append(reduced) }
        }
        return out
    }

    /// Keys that appear more than once across the groups. A registry must have none.
    public static func duplicateKeys(in groups: [HubSettingsGroup]) -> [String] {
        var seen = Set<String>()
        var duplicates = Set<String>()
        for group in groups {
            for setting in group.settings where !seen.insert(setting.key).inserted {
                duplicates.insert(setting.key)
            }
        }
        return duplicates.sorted()
    }
}

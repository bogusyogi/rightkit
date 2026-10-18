import Foundation
import Observation

/// The state behind `HubSettingsView`: the stored values over a `HubSettingsStore`, with the
/// documented defaults applied. Changes made elsewhere (the CLI, another window) arrive through
/// the store's `observe()` stream.
@MainActor
@Observable
public final class HubSettingsModel {
    /// The current values, by key. A key with no entry uses the setting's default.
    public private(set) var values: [String: HubSettingValue] = [:]
    /// The last failure, cleared by the next success.
    public private(set) var lastError: String?

    private let store: any HubSettingsStore
    private let keys: [String]
    private var saved: [String: HubSettingValue] = [:]

    public init(store: any HubSettingsStore, groups: [HubSettingsGroup]) {
        self.store = store
        self.keys = groups.flatMap { group in group.settings.map { $0.key } }
    }

    /// Loads every key, then follows changes until cancelled. Run it from `.task`.
    public func start() async {
        await load(keys)
        for await changed in store.observe() {
            if Task.isCancelled { break }
            let known = changed.filter { keys.contains($0) }
            await load(changed.isEmpty ? keys : known)
        }
    }

    private func load(_ requested: [String]) async {
        if requested.isEmpty { return }
        do {
            let got = try await store.get(requested)
            for key in requested {
                values[key] = got[key]
                saved[key] = got[key]
            }
            lastError = nil
        } catch {
            lastError = Self.describe(error)
        }
    }

    /// The value in effect: the stored one, or the documented default.
    public func value(for setting: HubSetting) -> HubSettingValue {
        values[setting.key] ?? setting.defaultValue
    }

    /// The switch state of a toggle setting.
    public func isOn(_ setting: HubSetting) -> Bool {
        value(for: setting).boolValue ?? false
    }

    /// The text of a choice or text setting.
    public func text(for setting: HubSetting) -> String {
        value(for: setting).stringValue ?? ""
    }

    /// True when the setting is at its documented default.
    public func isDefault(_ setting: HubSetting) -> Bool {
        value(for: setting) == setting.defaultValue
    }

    /// Stores a value. The change shows at once and is rolled back if the store refuses it.
    public func set(_ setting: HubSetting, _ value: HubSettingValue) async {
        values[setting.key] = value
        do {
            try await store.set(setting.key, value)
            saved[setting.key] = value
            lastError = nil
        } catch {
            values[setting.key] = saved[setting.key]
            lastError = Self.describe(error)
        }
    }

    /// Changes a text setting on screen only; `commit` stores it.
    public func edit(_ setting: HubSetting, text: String) {
        values[setting.key] = .string(text)
    }

    /// Stores the value currently on screen for a setting.
    public func commit(_ setting: HubSetting) async {
        await set(setting, value(for: setting))
    }

    /// Puts a setting back to its documented default.
    public func reset(_ setting: HubSetting) async {
        await set(setting, setting.defaultValue)
    }

    private static func describe(_ error: any Error) -> String {
        if let engine = error as? EngineError { return engine.message }
        return String(describing: error)
    }
}

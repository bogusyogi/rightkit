#if os(macOS)
import Foundation
import RightKitHub

/// The settings store over the notch bridge, for RightKitHub's `HubSettingsModel`.
///
/// Reads come from the notch state (`notch.state()`, the JSON `notch_state` returns), and writes
/// are the page's `notch_command` object `{"command":"set","key":...,"value":...}`
/// (Settings.tsx:363). Changes the notch publishes arrive through `notch.stateUpdates()` and
/// are diffed key by key.
struct NotchSettingsStore: HubSettingsStore {
    let notch: any NotchBridge
    /// Keys whose value is one of a list of choices. The notch may publish a choice as a number
    /// or a bool; the kit's choice rows compare strings, so these are read as text.
    let choiceKeys: Set<String>
    /// False for a one-shot read (the off-screen renderer): `observe()` ends at once.
    let observes: Bool

    init(notch: any NotchBridge, choiceKeys: Set<String>, observes: Bool = true) {
        self.notch = notch
        self.choiceKeys = choiceKeys
        self.observes = observes
    }

    func get(_ keys: [String]) async throws -> [String: HubSettingValue] {
        guard let data = await notch.state(), let published = SettingsNotchState.settings(from: data) else {
            return [:]
        }
        var out: [String: HubSettingValue] = [:]
        let wanted = keys.isEmpty ? Array(published.keys) : keys
        for key in wanted {
            guard let value = published[key], let converted = Self.convert(value, asChoice: choiceKeys.contains(key)) else {
                continue
            }
            out[key] = converted
        }
        return out
    }

    func set(_ key: String, _ value: HubSettingValue) async throws {
        let json: SettingsJSON
        switch value {
        case .bool(let flag): json = .bool(flag)
        case .string(let text): json = .string(text)
        case .number(let number): json = .number(number)
        case .other: json = .null
        }
        let body: [String: SettingsJSON] = ["command": .string("set"), "key": .string(key), "value": json]
        try await notch.command(try JSON.encoder.encode(body))
    }

    func observe() -> AsyncStream<[String]> {
        let notch = self.notch
        let observes = self.observes
        return AsyncStream<[String]> { continuation in
            if !observes {
                continuation.finish()
                return
            }
            let task = Task {
                var previous: [String: SettingsJSON] = [:]
                if let data = await notch.state(), let first = SettingsNotchState.settings(from: data) {
                    previous = first
                }
                for await data in notch.stateUpdates() {
                    if Task.isCancelled { break }
                    guard let next = SettingsNotchState.settings(from: data) else { continue }
                    var changed: [String] = []
                    for key in Set(previous.keys).union(next.keys) where previous[key] != next[key] {
                        changed.append(key)
                    }
                    previous = next
                    if !changed.isEmpty { continuation.yield(changed.sorted()) }
                }
                continuation.finish()
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    private static func convert(_ value: SettingsJSON, asChoice: Bool) -> HubSettingValue? {
        switch value {
        case .null: return nil
        case .bool(let flag): return asChoice ? .string(flag ? "true" : "false") : .bool(flag)
        case .number(let number): return asChoice ? .string(SettingsFormat.number(number)) : .number(number)
        case .string(let text): return .string(text)
        case .array, .object: return .other
        }
    }
}
#endif

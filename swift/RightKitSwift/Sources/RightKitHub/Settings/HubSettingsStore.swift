import Foundation

/// Where setting values live. The hub reads and writes through this and never touches storage.
public protocol HubSettingsStore: Sendable {
    /// The stored values for `keys`. Keys with no stored value are absent from the result; an empty
    /// `keys` asks for every stored value.
    func get(_ keys: [String]) async throws -> [String: HubSettingValue]

    /// Stores one value.
    func set(_ key: String, _ value: HubSettingValue) async throws

    /// The keys that changed, each time something changes (including changes from other processes).
    /// An empty array means "anything may have changed".
    func observe() -> AsyncStream<[String]>
}

/// The real store: the engine's settings commands.
///
/// - `settings.get {keys?}` returns `{values}`.
/// - `settings.set {key, value}` returns `{}`.
/// - The `settings` topic publishes `{keys}` when values change.
///
/// The engine mounts `rightkit-app-settings`' command set; see docs/swiftui-hub.md.
public struct EngineSettingsStore: HubSettingsStore {
    public static let getMethod = "settings.get"
    public static let setMethod = "settings.set"
    public static let topic = "settings"

    private struct GetParams: Encodable {
        let keys: [String]?
    }

    private struct GetResult: Decodable {
        let values: [String: HubSettingValue]
    }

    private struct SetParams: Encodable {
        let key: String
        let value: HubSettingValue
    }

    private struct Changed: Decodable {
        let keys: [String]
    }

    private let engine: any EngineClient

    public init(engine: any EngineClient) {
        self.engine = engine
    }

    public func get(_ keys: [String]) async throws -> [String: HubSettingValue] {
        let result = try await engine.call(
            Self.getMethod,
            GetParams(keys: keys.isEmpty ? nil : keys),
            as: GetResult.self
        )
        return result.values
    }

    public func set(_ key: String, _ value: HubSettingValue) async throws {
        _ = try await engine.call(
            Self.setMethod,
            SetParams(key: key, value: value),
            as: EngineEmpty.self
        )
    }

    public func observe() -> AsyncStream<[String]> {
        let engine = engine
        return AsyncStream<[String]> { continuation in
            let task = Task {
                do {
                    for try await event in engine.subscribe([EngineSettingsStore.topic]) {
                        if let changed = try? JSONDecoder().decode(Changed.self, from: event.payload) {
                            continuation.yield(changed.keys)
                        }
                    }
                } catch {
                    // The stream ends; the model keeps its last values.
                }
                continuation.finish()
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }
}

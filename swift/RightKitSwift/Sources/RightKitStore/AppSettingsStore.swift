import Foundation

/// A typed settings value. `.null` is only meaningful in a patch, where it resets the key to its
/// declared default (the same meaning `rightkit-app-settings` gives a JSON `null` by default); a
/// stored or read value is never `.null`.
public enum AppSettingValue: Equatable, Sendable {
    case bool(Bool)
    case int(Int)
    case double(Double)
    case string(String)
    case null

    public var kind: AppSettingKind? {
        switch self {
        case .bool: .bool
        case .int: .int
        case .double: .double
        case .string: .string
        case .null: nil
        }
    }
}

public enum AppSettingKind: String, Codable, Sendable {
    case bool, int, double, string
}

/// One declared setting: its key, its kind, the value used while nothing is stored, and whether a
/// patch from outside the owner (a change request relayed by another process) may change it.
public struct AppSettingDefinition: Equatable, Sendable {
    public let key: String
    public let kind: AppSettingKind
    public let defaultValue: AppSettingValue
    public let isProtected: Bool

    public init(key: String, kind: AppSettingKind, defaultValue: AppSettingValue, isProtected: Bool = false) {
        self.key = key
        self.kind = kind
        self.defaultValue = defaultValue
        self.isProtected = isProtected
    }
}

public enum AppSettingsError: Error, Equatable, Sendable {
    case invalidManifest(String)
    case undeclaredKey(String)
    case notAllowed(String)
    case protectedKey(String)
    case typeMismatch(key: String, expected: AppSettingKind)
    /// A value read back did not equal the value written; every key of the patch was restored.
    case persistenceFailed(key: String)
    case lockUnavailable
    case defaultsUnavailable
}

/// The declared settings of one app. Build it in code, or decode the JSON form:
///
/// ```json
/// { "settings": [ { "key": "theme", "kind": "string", "default": "dark", "protected": false } ] }
/// ```
///
/// (`kind` is `bool`, `int`, `double` or `string`; `default` must match it; `protected` is optional.)
public struct AppSettingsManifest: Equatable, Sendable {
    public let definitions: [AppSettingDefinition]
    private let index: [String: AppSettingDefinition]

    public init(_ definitions: [AppSettingDefinition]) throws {
        var index: [String: AppSettingDefinition] = [:]
        for d in definitions {
            guard !d.key.isEmpty, !d.key.contains("\0") else {
                throw AppSettingsError.invalidManifest("invalid key '\(d.key)'")
            }
            guard index[d.key] == nil else { throw AppSettingsError.invalidManifest("duplicate key '\(d.key)'") }
            guard d.defaultValue.kind == d.kind else {
                throw AppSettingsError.invalidManifest("default of '\(d.key)' is not a \(d.kind.rawValue)")
            }
            index[d.key] = d
        }
        self.definitions = definitions
        self.index = index
    }

    public func definition(for key: String) -> AppSettingDefinition? { index[key] }

    public static func decode(json: Data) throws -> AppSettingsManifest {
        guard let root = (try? JSONSerialization.jsonObject(with: json)) as? [String: Any],
              let list = root["settings"] as? [[String: Any]]
        else { throw AppSettingsError.invalidManifest("expected {\"settings\": [...]}") }
        var out: [AppSettingDefinition] = []
        for entry in list {
            guard let key = entry["key"] as? String, let kindName = entry["kind"] as? String,
                  let kind = AppSettingKind(rawValue: kindName), let raw = entry["default"]
            else { throw AppSettingsError.invalidManifest("a setting needs key, kind and default") }
            guard let value = AppSettingsCoding.value(fromJSON: raw, as: kind) else {
                throw AppSettingsError.invalidManifest("default of '\(key)' is not a \(kind.rawValue)")
            }
            out.append(AppSettingDefinition(key: key, kind: kind, defaultValue: value,
                                            isProtected: (entry["protected"] as? Bool) ?? false))
        }
        return try AppSettingsManifest(out)
    }
}

enum AppSettingsCoding {
    private static func isBool(_ n: NSNumber) -> Bool { CFGetTypeID(n) == CFBooleanGetTypeID() }

    /// A value of `kind` from a JSON or UserDefaults object, or nil when the type does not match.
    static func value(fromJSON raw: Any, as kind: AppSettingKind) -> AppSettingValue? {
        switch kind {
        case .string:
            return (raw as? String).map(AppSettingValue.string)
        case .bool:
            guard let n = raw as? NSNumber, isBool(n) else { return nil }
            return .bool(n.boolValue)
        case .int:
            guard let n = raw as? NSNumber, !isBool(n), let i = Int(exactly: n.doubleValue),
                  n.doubleValue == Double(i) else { return nil }
            return .int(i)
        case .double:
            guard let n = raw as? NSNumber, !isBool(n) else { return nil }
            return .double(n.doubleValue)
        }
    }

    static func object(_ value: AppSettingValue) -> Any? {
        switch value {
        case .bool(let v): v
        case .int(let v): v
        case .double(let v): v
        case .string(let v): v
        case .null: nil
        }
    }
}

/// A declared-key settings store over the App Group `UserDefaults` (or an injected suite), for
/// Apple apps that keep preferences in defaults. It mirrors the guarantees of the Rust
/// `rightkit-app-settings` `Store`: only declared keys, typed values, a defaults overlay, a
/// `patch` that is all-or-nothing, `null` resets a key, a monotonic `revision`, change events after
/// a successful write, and cross-process mutual exclusion through the App Group lock. A write is
/// reported only after it reads back; if any key of a patch fails to persist, every key of that
/// patch is restored. Values live at their plain keys (optionally behind `keyPrefix`), so existing
/// `@AppStorage` and `UserDefaults` readers keep working.
///
/// Not covered here (Rust owners / tooling): manifest-to-Swift/Rust/TS code generation and the
/// cross-language parity check; a UTF-8 BOM option (a file format concern, no file here); relaying a
/// change request to the owning process (use `AppGroupCommandQueue`).
public final class AppSettingsStore: @unchecked Sendable {
    public struct Change: Equatable, Sendable {
        public let keys: Set<String>
        public let revision: UInt64
    }

    private let configuration: AppGroupConfiguration
    public let manifest: AppSettingsManifest
    private let keyPrefix: String
    private let lock = NSLock()
    private var handlers: [UUID: @Sendable (Change) -> Void] = [:]
    private var lastSeenRevision: UInt64

    public init(configuration: AppGroupConfiguration, manifest: AppSettingsManifest, keyPrefix: String = "") {
        self.configuration = configuration
        self.manifest = manifest
        self.keyPrefix = keyPrefix
        self.lastSeenRevision = 0
        self.lastSeenRevision = revision
    }

    private var revisionKey: String { keyPrefix + "__rk_settings_revision" }
    private func storageKey(_ key: String) -> String { keyPrefix + key }

    /// Monotonic count of successful writes, 0 before the first.
    public var revision: UInt64 {
        (configuration.defaults?.object(forKey: revisionKey) as? NSNumber)?.uint64Value ?? 0
    }

    /// The current value of a declared key: the stored value when it has the declared type,
    /// otherwise the default. `nil` for an undeclared key.
    public func value(_ key: String) -> AppSettingValue? {
        guard let d = manifest.definition(for: key) else { return nil }
        return stored(d) ?? d.defaultValue
    }

    /// Every declared key with its current value.
    public func snapshot() -> [String: AppSettingValue] {
        var out: [String: AppSettingValue] = [:]
        for d in manifest.definitions { out[d.key] = stored(d) ?? d.defaultValue }
        return out
    }

    private func stored(_ d: AppSettingDefinition) -> AppSettingValue? {
        guard let raw = configuration.defaults?.object(forKey: storageKey(d.key)) else { return nil }
        return AppSettingsCoding.value(fromJSON: raw, as: d.kind)
    }

    /// Apply `changes` atomically. Nothing is written unless every entry is valid.
    ///
    /// - Parameters:
    ///   - allow: when set, keys outside it are refused (`notAllowed`).
    ///   - includeProtected: protected keys are refused (`protectedKey`) unless this is true. The
    ///     owning process passes true for its own changes; a relayed request leaves it false.
    /// - Returns: the new snapshot. A patch that changes nothing writes nothing and keeps the revision.
    @discardableResult
    public func patch(
        _ changes: [String: AppSettingValue],
        allow: Set<String>? = nil,
        includeProtected: Bool = false
    ) throws -> [String: AppSettingValue] {
        var plan: [(AppSettingDefinition, AppSettingValue)] = []
        for (key, value) in changes.sorted(by: { $0.key < $1.key }) {
            guard let d = manifest.definition(for: key) else { throw AppSettingsError.undeclaredKey(key) }
            if let allow, !allow.contains(key) { throw AppSettingsError.notAllowed(key) }
            if d.isProtected && !includeProtected { throw AppSettingsError.protectedKey(key) }
            if value != .null && value.kind != d.kind { throw AppSettingsError.typeMismatch(key: key, expected: d.kind) }
            plan.append((d, value))
        }
        return try apply(plan)
    }

    /// Remove the stored value of each key so it reads as its default. Undeclared keys are refused.
    @discardableResult
    public func reset(_ keys: [String], includeProtected: Bool = false) throws -> [String: AppSettingValue] {
        var changes: [String: AppSettingValue] = [:]
        for k in keys { changes[k] = .null }
        return try patch(changes, includeProtected: includeProtected)
    }

    private func apply(_ plan: [(AppSettingDefinition, AppSettingValue)]) throws -> [String: AppSettingValue] {
        guard let defaults = configuration.defaults else { throw AppSettingsError.defaultsUnavailable }
        let changed: Change? = try withLock {
            // Skip entries that already hold the requested outcome.
            let work = plan.filter { d, v in
                let target = (v == .null) ? nil : v
                let have = defaults.object(forKey: storageKey(d.key)) == nil ? nil : stored(d)
                return target != have
            }
            guard !work.isEmpty else { return nil }
            let before = work.map { defaults.object(forKey: storageKey($0.0.key)) }
            let oldRevision = defaults.object(forKey: revisionKey)
            func restore() {
                for (i, entry) in work.enumerated() {
                    let k = storageKey(entry.0.key)
                    if let o = before[i] { defaults.set(o, forKey: k) } else { defaults.removeObject(forKey: k) }
                }
                if let o = oldRevision { defaults.set(o, forKey: revisionKey) } else { defaults.removeObject(forKey: revisionKey) }
            }
            for (d, v) in work {
                if let obj = AppSettingsCoding.object(v) {
                    defaults.set(obj, forKey: storageKey(d.key))
                } else {
                    defaults.removeObject(forKey: storageKey(d.key))
                }
            }
            let next = revision &+ 1
            defaults.set(NSNumber(value: next), forKey: revisionKey)
            for (d, v) in work {
                let readBack = defaults.object(forKey: storageKey(d.key)).flatMap {
                    AppSettingsCoding.value(fromJSON: $0, as: d.kind)
                }
                let want: AppSettingValue? = (v == .null) ? nil : v
                if readBack != want {
                    restore()
                    configuration.report(.persistenceFailed(key: d.key))
                    throw AppSettingsError.persistenceFailed(key: d.key)
                }
            }
            guard revision == next else {
                restore()
                configuration.report(.persistenceFailed(key: revisionKey))
                throw AppSettingsError.persistenceFailed(key: revisionKey)
            }
            return Change(keys: Set(work.map(\.0.key)), revision: next)
        }
        if let changed { publish(changed) }
        return snapshot()
    }

    private func withLock<T>(_ body: () throws -> T) throws -> T {
        do {
            return try configuration.locked(body)
        } catch AppGroupStoreError.lockUnavailable {
            throw AppSettingsError.lockUnavailable
        }
    }

    // MARK: Change events

    /// Called after a successful write by this store, and by `pollExternal()` when another process
    /// changed the revision. Returns a token for `unsubscribe`.
    @discardableResult
    public func subscribe(_ handler: @escaping @Sendable (Change) -> Void) -> UUID {
        let id = UUID()
        lock.lock(); handlers[id] = handler; lock.unlock()
        return id
    }

    public func unsubscribe(_ id: UUID) {
        lock.lock(); handlers[id] = nil; lock.unlock()
    }

    /// Detect a write made by another process: when the stored revision moved since this store last
    /// saw it, notify subscribers (with every declared key, as the writer's key set is unknown) and
    /// return the change; otherwise `nil`.
    @discardableResult
    public func pollExternal() -> Change? {
        let now = revision
        lock.lock()
        let moved = now != lastSeenRevision
        lock.unlock()
        guard moved else { return nil }
        let change = Change(keys: Set(manifest.definitions.map(\.key)), revision: now)
        publish(change)
        return change
    }

    private func publish(_ change: Change) {
        lock.lock()
        lastSeenRevision = change.revision
        let current = Array(handlers.values)
        lock.unlock()
        for h in current { h(change) }
    }
}

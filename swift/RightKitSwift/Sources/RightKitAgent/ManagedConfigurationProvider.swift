import Foundation
@preconcurrency import ManagedApp
import Observation

// ManagedAppConfigurationProvider owns its configuration stream internally.
// Current SDK headers do not declare Sendable, yet `configurations(_:)` is
// async and must cross this main-actor bridge. Keep this assertion local to
// the Apple framework boundary rather than weakening app-owned types.
extension ManagedAppConfigurationProvider: @retroactive @unchecked Sendable {}

// ENG-112. Reads admin-pushed connection presets from Apple's two supported
// app-configuration channels and republishes one validated, deduplicated
// list. See `ManagedConfiguration.swift` for the narrow, secret-free schema.
//
// Channels, in merge precedence order (declarative wins on a dedup collision):
//   1. Declarative — `ManagedAppConfigurationProvider.configurations(_:)`,
//      Apple's live managed-app-configuration stream. DDM/MDM owns its writer;
//      the app owns subscription, validation, merge, and UI projection.
//   2. Legacy — `com.apple.configuration.managed`, the standard key MDM
//      vendors populate via Apple's managed-app-configuration mechanism
//      (`NSUserDefaultsDidChangeNotification` fires when it updates). Value
//      shape can be JSON data/string or a property-list dictionary, all
//      decoded through the same `ManagedConfigurationPayload`.
//
// Republish trigger: `UserDefaults.didChangeNotification`. That is the
// actual notification managed-defaults changes surface through — there is
// no `com.apple.configuration.managed.changed` notification name in
// UIKit/Foundation; using the real one is what makes this provider observe
// live MDM pushes instead of only reading once at init.
@MainActor
@Observable
public final class ManagedConfigurationProvider {
    /// The actual key Apple's managed-app-configuration mechanism uses.
    public static let legacyChannelKey = "com.apple.configuration.managed"

    /// Read-only: presets are admin-supplied and never mutated by the app.
    public private(set) var presets: [ManagedConnectionPreset] = []
    /// True once at least one MDM-populated channel has been observed.
    public private(set) var hasManagedConfiguration = false

    private let defaults: UserDefaults
    private var declarativePresets: [ManagedConnectionPreset] = []
    private var legacyPresets: [ManagedConnectionPreset] = []
    /// Written on main actor and cancelled during teardown to stop Apple's
    /// indefinite configuration stream.
    nonisolated(unsafe) private var declarativeObservation: Task<Void, Never>?
    /// `nonisolated(unsafe)` so `deinit` (nonisolated in Swift 6) can read the
    /// token to unregister. Safe: written exactly once, in `init` on the main
    /// actor, and read only in `deinit`, which cannot race that write because
    /// deinit runs after the last reference is gone.
    nonisolated(unsafe) private var observer: NSObjectProtocol?

    public init(
        defaults: UserDefaults = .standard,
        declarativeUpdates: AsyncStream<ManagedConfigurationPayload?>? = nil
    ) {
        self.defaults = defaults
        reload()
        if let declarativeUpdates {
            observeDeclarativeUpdates(declarativeUpdates)
        } else {
            observeManagedAppConfiguration()
        }
        // `nonisolated(unsafe)` on the token capture below is not needed: we
        // only store the NSObjectProtocol token, and the notification
        // handler hops back to the main actor via Task before touching
        // `self`.
        observer = NotificationCenter.default.addObserver(
            forName: UserDefaults.didChangeNotification,
            object: defaults,
            queue: nil
        ) { [weak self] _ in
            Task { @MainActor [weak self] in
                self?.reload()
            }
        }
    }

    deinit {
        declarativeObservation?.cancel()
        if let observer {
            NotificationCenter.default.removeObserver(observer)
        }
    }

    /// Re-reads both channels, validates, and merges declarative-over-legacy.
    /// Safe to call with no MDM present: both channels resolve to `[]` and
    /// `presets` becomes `[]`, not a crash or placeholder value.
    public func reload() {
        legacyPresets = Self.readPayload(defaults: defaults, key: Self.legacyChannelKey)
        republish()
    }

    private func observeManagedAppConfiguration() {
        let provider = ManagedAppConfigurationProvider()
        declarativeObservation = Task { @MainActor [weak self, provider] in
            let updates = await provider.configurations(ManagedConfigurationPayload.self)
            for await payload in updates {
                guard !Task.isCancelled else { return }
                self?.replaceDeclarativePresets(payload?.presets ?? [])
            }
        }
    }

    private func observeDeclarativeUpdates(_ updates: AsyncStream<ManagedConfigurationPayload?>) {
        declarativeObservation = Task { @MainActor [weak self] in
            for await payload in updates {
                guard !Task.isCancelled else { return }
                self?.replaceDeclarativePresets(payload?.presets ?? [])
            }
        }
    }

    private func replaceDeclarativePresets(_ values: [ManagedConnectionPreset]) {
        declarativePresets = values
        republish()
    }

    private func republish() {
        // Declarative values precede legacy values, so they win an identity
        // collision under first-wins validation/deduplication.
        let merged = declarativePresets + legacyPresets
        presets = ManagedConfigurationValidator.validateAndDedup(merged)
        hasManagedConfiguration = !declarativePresets.isEmpty || !legacyPresets.isEmpty
    }

    /// Looks up one preset by kind + host, mirroring `ManagedConnectionPreset.id`.
    public func preset(kind: ManagedConnectionPreset.Kind, host: String) -> ManagedConnectionPreset? {
        let id = ManagedConnectionPreset.identity(kind: kind, endpoint: "https://\(host)")
        return presets.first { $0.id == id }
    }

    private static func readPayload(defaults: UserDefaults, key: String) -> [ManagedConnectionPreset] {
        guard let raw = defaults.object(forKey: key) else { return [] }

        // Accept either raw JSON `Data`, a JSON `String`, or an already
        // decoded `[String: Any]` dictionary. Managed defaults are commonly a
        // plist dictionary, while deployment tools also supply JSON blobs.
        let data: Data?
        switch raw {
        case let value as Data:
            data = value
        case let value as String:
            data = value.data(using: .utf8)
        case let value as [String: Any]:
            data = try? JSONSerialization.data(withJSONObject: value)
        default:
            data = nil
        }

        guard let data,
            let object = try? JSONSerialization.jsonObject(with: data),
            payloadShapeIsSafe(object),
            let payload = try? JSONDecoder().decode(ManagedConfigurationPayload.self, from: data)
        else {
            return []
        }
        return payload.presets
    }

    /// Managed configuration carries connection metadata only. Keeping this
    /// check before `Codable` decoding matters because `Codable` ignores
    /// unknown keys by default; otherwise a vendor payload with `apiKey` or
    /// `token` would be accepted after silently discarding secret material.
    ///
    /// Both supported channels intentionally use one narrow schema. New
    /// metadata must be modeled here before an MDM payload can provide it.
    private static func payloadShapeIsSafe(_ object: Any) -> Bool {
        guard let payload = object as? [String: Any],
            Set(payload.keys) == ["presets"],
            let rawPresets = payload["presets"] as? [Any]
        else { return false }

        let permittedPresetKeys: Set<String> = [
            "kind", "displayName", "endpoint", "secretSlotKind", "secretSlotID",
        ]
        return rawPresets.allSatisfy { rawPreset in
            guard let preset = rawPreset as? [String: Any] else { return false }
            return Set(preset.keys).isSubset(of: permittedPresetKeys)
        }
    }
}

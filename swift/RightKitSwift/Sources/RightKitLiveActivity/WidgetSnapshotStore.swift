import Foundation
import RightKitStore
import WidgetKit

/// App Group persistence boundary for the widget snapshot.
enum WidgetSnapshotStoreError: Error {
    case appGroupUnavailable
    case invalidSnapshot
}

/// Revisioned envelope persisted through RightKitStore. `WidgetSnapshot` stays the public model.
struct WidgetSnapshotRecord: RevisionedState {
    var revision: UInt64 = 0
    var updatedAt: Date = .distantPast
    var snapshot: WidgetSnapshot = .empty
}

enum WidgetSnapshotStore {
    static let appGroupIdentifier = "group.app.coderight.ios"
    /// RightKitStore key. Pre-0.4 builds stored a raw ISO8601 `WidgetSnapshot` under `legacySnapshotKey`.
    static let snapshotKey = "coderight.widget.snapshot.revisioned"
    static let legacySnapshotKey = "coderight.widget.snapshot"
    static let maxSessions = 6

    static func load(defaults: UserDefaults? = nil) throws -> WidgetSnapshot {
        let store = try revisionedStore(defaults: defaults)
        if let record = store.snapshot { return record.snapshot }
        // Fallback until the first revisioned write: the old raw value is read, never rewritten here.
        let resolved = try resolvedDefaults(defaults)
        guard let data = resolved.data(forKey: legacySnapshotKey) else { return .empty }
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        guard let snapshot = try? decoder.decode(WidgetSnapshot.self, from: data) else {
            throw WidgetSnapshotStoreError.invalidSnapshot
        }
        return snapshot
    }

    static func write(
        sessions: [WidgetSession],
        attentionSessionIDs: Set<String> = [],
        runtimeLabel: String? = nil,
        generatedAt: Date = Date(),
        defaults: UserDefaults? = nil,
        reloadTimelines: Bool = true
    ) throws {
        let projected = sessions.prefix(maxSessions).map { session in
            WidgetSession(
                id: session.id,
                title: session.title,
                project: session.project,
                needsAttention: attentionSessionIDs.contains(session.id),
                updatedAt: session.updatedAt
            )
        }
        let snapshot = WidgetSnapshot(
            generatedAt: generatedAt,
            runtimeLabel: runtimeLabel,
            attentionCount: sessions.filter { attentionSessionIDs.contains($0.id) }.count,
            recentSessions: Array(projected)
        )
        try write(snapshot, defaults: defaults, reloadTimelines: reloadTimelines)
    }

    static func write(
        _ snapshot: WidgetSnapshot,
        defaults: UserDefaults? = nil,
        reloadTimelines: Bool = true
    ) throws {
        let store = try revisionedStore(defaults: defaults)
        try store.write { record in
            record.snapshot = snapshot
        }
        guard reloadTimelines else { return }
        WidgetCenter.shared.reloadAllTimelines()
    }

    /// Builds the RightKitStore accessor. Injected suites (tests) have no App Group container, so
    /// their cross-process lock lives in the temporary directory; production locks in the container.
    private static func revisionedStore(defaults: UserDefaults?) throws -> RevisionedStateStore<WidgetSnapshotRecord> {
        let resolved = try resolvedDefaults(defaults)
        var configuration = AppGroupConfiguration(groupID: appGroupIdentifier, defaultsOverride: resolved)
        if defaults != nil {
            configuration.lockDirectory = FileManager.default.temporaryDirectory
        }
        return RevisionedStateStore<WidgetSnapshotRecord>(configuration: configuration, key: snapshotKey)
    }

    private static func resolvedDefaults(_ defaults: UserDefaults?) throws -> UserDefaults {
        if let defaults { return defaults }
        guard let shared = UserDefaults(suiteName: appGroupIdentifier) else {
            throw WidgetSnapshotStoreError.appGroupUnavailable
        }
        return shared
    }
}

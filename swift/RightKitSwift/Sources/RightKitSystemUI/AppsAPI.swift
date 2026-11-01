import RightKitEngineClient
import Foundation
import RightKitHub

/// The Apps commands as the engine sends and takes them (`engine/src/contract.rs`, `hub/src/api.ts`).
/// Keys follow the Rust serde names; optional fields are optional here so a field the engine omits
/// never fails a decode.
enum AppsAPI {
    /// One installed app (`AppEntry`).
    struct Entry: Decodable, Sendable, Hashable, Identifiable {
        let name: String
        let path: String
        let bundleId: String?
        let version: String?
        let sizeBytes: Int64
        /// Seconds since the Unix epoch; nil when unknown.
        let lastUsed: Int64?
        let running: Bool
        /// Why uninstall is not offered, when it is not.
        let protectedReason: String?

        var id: String { path }

        enum CodingKeys: String, CodingKey {
            case name
            case path
            case bundleId = "bundle_id"
            case version
            case sizeBytes = "size_bytes"
            case lastUsed = "last_used"
            case running
            case protectedReason = "protected"
        }
    }

    /// `apps_cached`: the saved inventory.
    struct Cached: Decodable, Sendable {
        let savedAt: Int64?
        let apps: [Entry]

        enum CodingKeys: String, CodingKey {
            case savedAt = "saved_at"
            case apps
        }
    }

    /// One file or folder tied to an app (`RelatedItem`).
    struct Related: Decodable, Sendable, Hashable, Identifiable {
        let path: String
        let label: String
        /// "Application", "User Library", "System Library", "Your app data", "Shared app data" or "Installer receipt".
        let location: String
        let exact: Bool
        /// exact, helper, group, prefix, team, name, receipt, install or publisher.
        let confidence: String
        let reason: String
        /// Root-owned: Finder asks for an administrator password.
        let admin: Bool
        let sizeBytes: Int64
        let preselected: Bool

        var id: String { path }

        enum CodingKeys: String, CodingKey {
            case path
            case label
            case location
            case exact
            case confidence
            case reason
            case admin
            case sizeBytes = "size_bytes"
            case preselected
        }
    }

    /// A login or background item that belongs to an app.
    struct Background: Decodable, Sendable, Hashable {
        let kind: String
        let label: String
        let path: String?
    }

    /// `app_detail`: the app, its leftovers, background items and installer receipts.
    struct Detail: Decodable, Sendable {
        let app: Entry
        let items: [Related]
        let background: [Background]
        let receipts: [String]
    }

    /// The params of `app_uninstall`, exactly as the page sends them: `path`, `bundleId` (null when the
    /// app has none) and `items`.
    struct UninstallParams: Encodable, Sendable {
        let path: String
        let bundleId: String?
        let items: [String]

        enum CodingKeys: String, CodingKey {
            case path
            case bundleId
            case items
        }

        func encode(to encoder: any Encoder) throws {
            var container = encoder.container(keyedBy: CodingKeys.self)
            try container.encode(path, forKey: .path)
            // An explicit null, as the page sends; the default encoding would leave the key out.
            try container.encode(bundleId, forKey: .bundleId)
            try container.encode(items, forKey: .items)
        }
    }

    /// `app_uninstall`'s result.
    struct UninstallResult: Decodable, Sendable {
        struct Moved: Decodable, Sendable, Hashable {
            let path: String
            let bytes: Int64
        }

        struct Failed: Decodable, Sendable, Hashable {
            let path: String
            let error: String
        }

        let moved: [Moved]
        let failed: [Failed]
        /// Same value as `movedBytes`; moved to the Trash frees nothing until it is emptied.
        let bytesMoved: Int64
        let movedBytes: Int64

        enum CodingKeys: String, CodingKey {
            case moved
            case failed
            case bytesMoved = "bytes_moved"
            case movedBytes = "moved_bytes"
        }
    }

    /// The update state of one app (`AppUpdate`).
    struct Update: Decodable, Sendable, Hashable {
        let path: String
        let name: String
        let bundleId: String?
        let installedVersion: String?
        /// app_store, homebrew, sparkle, winget or none.
        let source: String
        /// available, current, app_store, unknown or unavailable.
        let state: String
        let latestVersion: String?
        let cask: String?
        let storeUrl: String?
        let reason: String?
        let checkedAt: Int64

        enum CodingKeys: String, CodingKey {
            case path
            case name
            case bundleId = "bundle_id"
            case installedVersion = "installed_version"
            case source
            case state
            case latestVersion = "latest_version"
            case cask
            case storeUrl = "store_url"
            case reason
            case checkedAt = "checked_at"
        }
    }

    /// `apps_updates_cached`.
    struct UpdateReport: Decodable, Sendable {
        let checkedAt: Int64?
        let apps: [Update]

        enum CodingKeys: String, CodingKey {
            case checkedAt = "checked_at"
            case apps
        }
    }

    /// One login or background item that starts by itself (`StartupItem`).
    struct StartupItem: Decodable, Sendable, Hashable {
        let name: String
        let kind: String
        /// user, all users or system.
        let scope: String
        let path: String?
        let program: String?
        /// Nil when it cannot be told.
        let enabled: Bool?
        let vendor: String?
    }

    /// `startup_items`.
    struct StartupReport: Decodable, Sendable {
        let rows: [StartupItem]
        let notes: [String]
    }

    /// Params that are one path.
    struct PathParams: Encodable, Sendable {
        let path: String
    }

    /// `app_icons` params.
    struct PathsParams: Encodable, Sendable {
        let paths: [String]
    }

    /// `apps_updates_refresh` params.
    struct ForceParams: Encodable, Sendable {
        let force: Bool
    }
}

/// The progress of one app's update, as the `apps-update-job` event reported it.
struct AppsUpdateJob: Sendable, Equatable {
    enum State: Sendable, Equatable {
        case running
        case done
        case failed
        /// Homebrew did not report back in time; nothing says it worked.
        case unconfirmed
    }

    var state: State
    var message: String
}

/// Which apps the list shows: every app, ones unused for 90+ days, or ones with an update.
enum AppsFilter: String, Sendable, Hashable {
    case all
    case unused
    case updates
}

/// The engine calls the Apps section makes. Each is the Tauri command of the same name.
struct AppsClient: Sendable {
    let engine: any EngineClient

    private static let none = Data("{}".utf8)

    func cached() async throws -> AppsAPI.Cached {
        try await engine.call("apps_cached", EngineEmpty(), as: AppsAPI.Cached.self, timeout: .seconds(15))
    }

    /// Starts the background refresh. The engine answers at once; its rows and `apps-inventory` event
    /// are not available yet, so the caller re-reads `cached()` until the saved list is newer.
    func refresh() async throws {
        _ = try await engine.call("apps_refresh", params: Self.none, timeout: .seconds(30))
    }

    func summary(path: String) async throws -> AppsAPI.Entry {
        try await engine.call("app_summary", AppsAPI.PathParams(path: path), as: AppsAPI.Entry.self, timeout: .seconds(30))
    }

    /// An app's leftovers in one answer (the page streams `app_leftovers`; its events are not available yet).
    func detail(path: String) async throws -> AppsAPI.Detail {
        try await engine.call("app_detail", AppsAPI.PathParams(path: path), as: AppsAPI.Detail.self, timeout: .seconds(300))
    }

    func icons(paths: [String]) async throws -> [String: String] {
        try await engine.call("app_icons", AppsAPI.PathsParams(paths: paths), as: [String: String].self, timeout: .seconds(60))
    }

    func updatesCached() async throws -> AppsAPI.UpdateReport {
        try await engine.call("apps_updates_cached", EngineEmpty(), as: AppsAPI.UpdateReport.self, timeout: .seconds(15))
    }

    func updatesRefresh(force: Bool) async throws {
        let body = try JSONEncoder().encode(AppsAPI.ForceParams(force: force))
        _ = try await engine.call("apps_updates_refresh", params: body, timeout: .seconds(30))
    }

    /// Returns "running", "opened" or "store".
    func update(path: String) async throws -> String {
        try await engine.call("app_update", AppsAPI.PathParams(path: path), as: String.self, timeout: .seconds(60))
    }

    func startupItems() async throws -> AppsAPI.StartupReport {
        try await engine.call("startup_items", EngineEmpty(), as: AppsAPI.StartupReport.self, timeout: .seconds(60))
    }

    func uninstall(path: String, bundleId: String?, items: [String]) async throws -> AppsAPI.UninstallResult {
        let params = AppsAPI.UninstallParams(path: path, bundleId: bundleId, items: items)
        return try await engine.call("app_uninstall", params, as: AppsAPI.UninstallResult.self, timeout: .seconds(900))
    }

    func reveal(path: String) async throws {
        let body = try JSONEncoder().encode(AppsAPI.PathParams(path: path))
        _ = try await engine.call("reveal", params: body, timeout: .seconds(30))
    }
}

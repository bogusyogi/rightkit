#if os(macOS)
import Foundation

// DTOs for the Storage section. They mirror `hub/src/api.ts` (and the engine's serde types), so the
// property names keep the engine's snake_case. Fields the engine may leave out are optional.
// Every type carries the `Storage` prefix: the module is shared with the other sections.

struct StorageVolume: Codable, Sendable, Equatable, Identifiable {
    let name: String
    let mount_point: String
    /// Nil for a network volume that did not answer in time (never zero).
    let total_bytes: Int64?
    /// Physically free: what the file system would let a write use now.
    let available_bytes: Int64?
    /// macOS only: more space the system could reclaim; never added to the free figure.
    let reclaimable_bytes: Int64?
    let removable: Bool?
    let isInternal: Bool
    let disk_image: Bool
    /// "local" or "network".
    let kind: String
    let detail: String?
    /// "ok" or "not responding".
    let state: String

    var id: String { mount_point }
    var isNetwork: Bool { kind == "network" }
    var isResponding: Bool { state == "ok" }

    enum CodingKeys: String, CodingKey {
        case name, mount_point, total_bytes, available_bytes, reclaimable_bytes, removable
        case isInternal = "internal"
        case disk_image, kind, detail, state
    }
}

struct StorageRow: Codable, Sendable, Equatable, Identifiable {
    let path: String
    let name: String
    let is_dir: Bool
    let bytes: Int64
    /// A "smaller files" total, not a real path.
    let summary: Bool?
    /// Modified time in Unix seconds (a folder's: the newest below it); 0 or absent when unknown.
    let modified: Double?

    var id: String { path }
    /// Menu and keyboard actions apply to real items, never the "smaller files" total.
    var isActionable: Bool { summary != true }
}

struct StorageFolder: Codable, Sendable, Equatable {
    var path: String
    var root: String
    var rows: [StorageRow]
    var total_children: Int?
    var incomplete: Bool?
    var needs_access: Bool?
    /// Items the scan could not read (long paths, access denied, entry limit).
    var unread: Int?
    var limited: Bool?
    var root_label: String
    /// Unix seconds when this data was scanned.
    var scanned_at: Double
    /// Shown from the last saved scan rather than a scan made this session.
    var from_snapshot: Bool?
    /// Rows carry modified times; false for a scan saved before they were kept.
    var has_dates: Bool?
}

struct StorageScanStatus: Codable, Sendable, Equatable {
    let running: Bool
    let running_root: String?
    let has_index: Bool?
    let root: String?
    let scanned_at: Double?
    let from_snapshot: Bool?
    /// The index is kept current while the hub runs; without it a saved scan only gets older.
    let live_refresh: Bool?
}

struct StorageChange: Codable, Sendable, Equatable {
    let path: String
    /// Signed change in bytes.
    let bytes: Int64
}

struct StorageGrowth: Codable, Sendable, Equatable {
    let available: Bool
    /// Unix seconds of the scan compared against.
    let since: Double?
    let grown: [StorageChange]
    let shrunk: [StorageChange]
    let reason: String?
}

/// A range of modified times in Unix seconds: `since` inclusive, `before` exclusive.
struct StorageChangedRange: Codable, Sendable, Equatable {
    var since: Double?
    var before: Double?
}

struct StorageSearchResult: Codable, Sendable, Equatable {
    let rows: [StorageRow]
    /// Matches the date range left out for having no date.
    let undated: Int
    let has_dates: Bool?
}

/// An item's identity when the person asked for an action on it (device and inode). The engine
/// checks it again right before anything moves.
struct StorageFileIdentity: Codable, Sendable, Equatable {
    let dev: UInt64
    let ino: UInt64
    let is_dir: Bool
}

/// What the inspector reads for one item; a nil field could not be read.
struct StorageInspect: Codable, Sendable, Equatable {
    let is_dir: Bool
    let is_link: Bool?
    let logical_bytes: Int64?
    let created_at: Double?
    let modified_at: Double?
    let files: Int?
    let folders: Int?
}

struct StorageMovePlan: Codable, Sendable, Equatable {
    let target: String
    let same_volume: Bool
}

struct StorageMoveResult: Codable, Sendable, Equatable {
    let target: String
    let copied: Bool?
}

struct StorageSkipped: Codable, Sendable, Equatable {
    let path: String
    let reason: String
}

// ---- Findings (cleanup) ----

struct StorageFinding: Codable, Sendable, Equatable, Identifiable {
    let id: String
    let rule_id: String
    let rule_name: String?
    let category: String?
    let name: String
    let path: String
    let bytes: Int64
    let apparent_bytes: Int64?
    let partial: Bool
    /// "safe", "review" or "info".
    let risk: String
    let eligible: Bool
    let preselected: Bool?
    let reason: String
    let action: String
    let dev: String
    let ino: String
}

struct StorageChromeSnapshots: Codable, Sendable, Equatable {
    let count: Int
    let apparent_bytes: Int64
    let reclaimable_bytes: Int64
    let reclaimable_known: Bool
    let running: Bool
    let since_at: Double?
    let since_count: Int?
}

struct StorageCleanupReport: Codable, Sendable, Equatable {
    let chrome_snapshots: StorageChromeSnapshots?
    let findings: [StorageFinding]
    let safe_bytes: Int64
    let review_bytes: Int64
    let scanned_at: Double
}

struct StorageCleanupApplyResult: Codable, Sendable, Equatable {
    let moved_items: Int
    let moved_bytes: Int64
    let skipped: [StorageSkipped]
    let activity_id: String?
}

// ---- Drive health ----

struct StorageSelfTest: Codable, Sendable, Equatable {
    let kind: String
    let result: String
    let passed: Bool?
    let power_on_hours: Int64?
}

/// One successful SMART reading. `at` is Unix seconds.
struct StorageHealthReading: Codable, Sendable, Equatable {
    let at: Double
    let passed: Bool?
    let temperature_c: Double?
    let wear_percent: Double?
    let written_bytes: Int64?
    let power_on_hours: Int64?
    let critical_warning: Int64?
    let media_errors: Int64?
    let self_tests: [StorageSelfTest]
}

struct StorageHealthAlert: Codable, Sendable, Equatable, Identifiable {
    let id: String
    let at: Double
    let disk: String
    let kind: String
    let message: String
}

struct StorageDriveCard: Codable, Sendable, Equatable {
    let mount: String
    let disk: String?
    let model: String?
    /// "ok", "warning", "unavailable" or "unknown".
    let status: String
    let reachable: Bool?
    let latest: StorageHealthReading?
    let history: [StorageHealthReading]
}

struct StorageHealthReport: Codable, Sendable, Equatable {
    let tool_available: Bool
    let sampled_at: Double?
    let drives: [StorageDriveCard]
    /// Newest first.
    let alerts: [StorageHealthAlert]
}

// ---- Duplicates ----

struct StorageDuplicateGroup: Codable, Sendable, Equatable {
    let kept_path: String
    let extras: [String]
    let size_bytes: Int64
}

struct StorageDuplicateReport: Codable, Sendable, Equatable {
    let groups: [StorageDuplicateGroup]
    let skipped: [StorageSkipped]
    let diagnostics: [String]
    let truncated: Bool
    let files_considered: Int
    let bytes_read: Int64?
}

struct StorageDuplicatesTrashResult: Codable, Sendable, Equatable {
    struct Moved: Codable, Sendable, Equatable {
        let path: String
        let bytes: Int64
    }
    let moved: [Moved]
    let skipped: [StorageSkipped]
    let moved_bytes: Int64
}

// ---- Params ----

struct StoragePathParams: Encodable, Sendable {
    let path: String
}

/// The answer of `media_compress` (the app #202): measured from the written copy, never estimated.
struct StorageCompressOutcome: Decodable, Sendable {
    let output: String
    let source_bytes: Int64
    let output_bytes: Int64
    let saved_bytes: Int64
}

/// The answer of `storage_preview`: the image file's bytes, base64 (the engine's one non-JSON-shaped result).
struct StoragePreviewReply: Decodable, Sendable {
    let base64: String
}

struct StorageScanParams: Encodable, Sendable {
    /// Nil scans the home folder (the startup disk); a mount point scans that volume.
    let path: String?
}

struct StorageSearchParams: Encodable, Sendable {
    let query: String
    let minBytes: Int64?
    let changed: StorageChangedRange?
}

struct StorageIdentityParams: Encodable, Sendable {
    let path: String
    let dev: UInt64
    let ino: UInt64
}

struct StorageMovePlanParams: Encodable, Sendable {
    let path: String
    let dev: UInt64
    let ino: UInt64
    let destination: String
}

struct StorageMoveParams: Encodable, Sendable {
    let path: String
    let dev: UInt64
    let ino: UInt64
    let destination: String
    let copyAcrossVolumes: Bool
}

struct StorageMountParams: Encodable, Sendable {
    let mount: String
}

struct StorageMountsParams: Encodable, Sendable {
    let mounts: [String]
}

struct StorageCleanupItem: Encodable, Sendable {
    let rule_id: String
    let path: String
    let dev: String
    let ino: String
}

struct StorageCleanupApplyParams: Encodable, Sendable {
    let items: [StorageCleanupItem]
}

struct StorageIdParams: Encodable, Sendable {
    let id: String
}

struct StorageDuplicateTrashItem: Encodable, Sendable {
    let kept: String
    let path: String
}

struct StorageDuplicatesTrashParams: Encodable, Sendable {
    let items: [StorageDuplicateTrashItem]
}
#endif

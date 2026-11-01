import Foundation
import RightKitHub

/// The Cleanup commands as the engine sends and takes them (`engine/src/cleanup.rs`,
/// `hub/src/views/Cleanup.tsx`). Fields the engine may omit (older saved findings and history) are
/// optional here, so an old file never fails a decode.
enum CleanupAPI {
    /// One thing the scan found.
    struct Finding: Decodable, Sendable, Hashable, Identifiable {
        let id: String
        let ruleId: String
        let category: String
        let name: String
        let path: String
        let bytes: Int64
        let partial: Bool
        /// The size is the most deleting could free; hard links or clones may free less.
        let upperBound: Bool?
        /// "safe", "review" or "info".
        let risk: String
        let eligible: Bool
        let preselected: Bool
        let reason: String
        let action: String
        /// Decimal strings: inode numbers can exceed what a JSON number holds.
        let dev: String
        let ino: String

        enum CodingKeys: String, CodingKey {
            case id
            case ruleId = "rule_id"
            case category
            case name
            case path
            case bytes
            case partial
            case upperBound = "upper_bound"
            case risk
            case eligible
            case preselected
            case reason
            case action
            case dev
            case ino
        }

        var isUpperBound: Bool { upperBound ?? false }

        /// One predicate for what can be selected, totalled and submitted. The engine still decides
        /// again when the move runs, and unknown liveness never passes.
        var movable: Bool {
            eligible && action == "Move to Trash" && (risk == "safe" || risk == "review") && !dev.isEmpty
                && !ino.isEmpty
        }
    }

    /// Chrome's leftover signing snapshots.
    struct ChromeSnapshots: Decodable, Sendable, Hashable {
        let count: Int
        let apparentBytes: Int64
        let reclaimableBytes: Int64
        let reclaimableKnown: Bool
        let running: Bool
        let sinceAt: Int64?
        let sinceCount: Int?

        enum CodingKeys: String, CodingKey {
            case count
            case apparentBytes = "apparent_bytes"
            case reclaimableBytes = "reclaimable_bytes"
            case reclaimableKnown = "reclaimable_known"
            case running
            case sinceAt = "since_at"
            case sinceCount = "since_count"
        }
    }

    /// `cleanup_scan` and `cleanup_cached`.
    struct Report: Decodable, Sendable {
        let chromeSnapshots: ChromeSnapshots?
        let findings: [Finding]
        let safeBytes: Int64
        let reviewBytes: Int64
        let scannedAt: Int64

        enum CodingKeys: String, CodingKey {
            case chromeSnapshots = "chrome_snapshots"
            case findings
            case safeBytes = "safe_bytes"
            case reviewBytes = "review_bytes"
            case scannedAt = "scanned_at"
        }
    }

    struct Skipped: Decodable, Sendable, Hashable {
        let path: String
        let reason: String
    }

    /// What the drive itself reported as available before and after a cleanup.
    struct VolumeFree: Decodable, Sendable, Hashable {
        let volume: String
        let beforeAvailable: Int64
        let afterAvailable: Int64

        enum CodingKeys: String, CodingKey {
            case volume
            case beforeAvailable = "before_available"
            case afterAvailable = "after_available"
        }
    }

    /// `cleanup_apply`'s result.
    struct ApplyResult: Decodable, Sendable {
        let movedItems: Int
        let movedBytes: Int64
        let skipped: [Skipped]
        let activityId: String?
        let warnings: [String]?
        let freeSpace: [VolumeFree]?

        enum CodingKeys: String, CodingKey {
            case movedItems = "moved_items"
            case movedBytes = "moved_bytes"
            case skipped
            case activityId = "activity_id"
            case warnings
            case freeSpace = "free_space"
        }
    }

    /// `cleanup_restore`'s result.
    struct RestoreResult: Decodable, Sendable {
        let restoredItems: Int
        let restoredBytes: Int64
        let skipped: [Skipped]
        let warnings: [String]?

        enum CodingKeys: String, CodingKey {
            case restoredItems = "restored_items"
            case restoredBytes = "restored_bytes"
            case skipped
            case warnings
        }
    }

    /// One moved item in the history.
    struct ActivityItem: Decodable, Sendable, Hashable {
        let path: String
        let trashPath: String?
        let bytes: Int64
        let restored: Bool?
        /// Set when a crash left this item's state unknown; Undo refuses it.
        let interrupted: String?

        enum CodingKeys: String, CodingKey {
            case path
            case trashPath = "trash_path"
            case bytes
            case restored
            case interrupted
        }

        var isRestored: Bool { restored ?? false }
    }

    /// One cleanup run in the history.
    struct Activity: Decodable, Sendable, Hashable, Identifiable {
        let id: String
        let at: Int64
        let bytes: Int64
        let items: [ActivityItem]
        let skipped: Int?
        let unknown: Int?
        /// True when Pulse was interrupted during this cleanup and settled it afterwards.
        let recovered: Bool?
        let freeSpace: [VolumeFree]?

        enum CodingKeys: String, CodingKey {
            case id
            case at
            case bytes
            case items
            case skipped
            case unknown
            case recovered
            case freeSpace = "free_space"
        }
    }

    /// `cleanup_history`: `{actions, warnings}`.
    struct History: Decodable, Sendable {
        let actions: [Activity]
        let warnings: [String]
    }

    /// One item of `cleanup_apply`'s params, with the identity the scan recorded.
    struct ApplyItem: Encodable, Sendable {
        let ruleId: String
        let path: String
        let dev: String
        let ino: String

        enum CodingKeys: String, CodingKey {
            case ruleId = "rule_id"
            case path
            case dev
            case ino
        }
    }

    /// `cleanup_apply`'s params.
    struct ApplyParams: Encodable, Sendable {
        let items: [ApplyItem]
    }

    /// `cleanup_restore`'s params.
    struct RestoreParams: Encodable, Sendable {
        let id: String
    }
}

/// The engine calls the Cleanup section makes. Each is the Tauri command of the same name.
struct CleanupClient: Sendable {
    let engine: any EngineClient

    /// The saved findings, or nil when none were saved. Never scans.
    func cached() async throws -> CleanupAPI.Report? {
        let data = try await engine.call("cleanup_cached", params: Data("{}".utf8), timeout: .seconds(15))
        // A saved report is a JSON object; none saved is `null`.
        if String(decoding: data, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines) == "null" {
            return nil
        }
        return try JSONDecoder().decode(CleanupAPI.Report.self, from: data)
    }

    func scan() async throws -> CleanupAPI.Report {
        try await engine.call("cleanup_scan", EngineEmpty(), as: CleanupAPI.Report.self, timeout: .seconds(900))
    }

    /// Moves findings to the Trash. Each item carries `rule_id`, `path`, `dev` and `ino`; the engine
    /// checks them again right before it moves anything.
    func apply(_ items: [CleanupAPI.Finding]) async throws -> CleanupAPI.ApplyResult {
        let params = CleanupAPI.ApplyParams(
            items: items.map { CleanupAPI.ApplyItem(ruleId: $0.ruleId, path: $0.path, dev: $0.dev, ino: $0.ino) }
        )
        return try await engine.call("cleanup_apply", params, as: CleanupAPI.ApplyResult.self, timeout: .seconds(900))
    }

    func history() async throws -> CleanupAPI.History {
        try await engine.call("cleanup_history", EngineEmpty(), as: CleanupAPI.History.self, timeout: .seconds(60))
    }

    func restore(id: String) async throws -> CleanupAPI.RestoreResult {
        try await engine.call(
            "cleanup_restore",
            CleanupAPI.RestoreParams(id: id),
            as: CleanupAPI.RestoreResult.self,
            timeout: .seconds(600)
        )
    }
}

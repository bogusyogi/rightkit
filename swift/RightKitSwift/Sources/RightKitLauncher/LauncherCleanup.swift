import Foundation

/// What the launcher can say about the hub's last cleanup scan. Read from the saved findings
/// file only: the launcher never starts a scan or a `pulse` process.
struct LauncherCleanupSummary: Equatable, Sendable {
    /// Every finding in the saved report.
    let items: Int
    /// The bytes the report marks safe to remove (`safe_bytes`).
    let freeBytes: Int64
    /// When the saved scan finished.
    let scannedAt: Date
}

enum LauncherCleanupFindings {
    /// The file and format the hub writes (`hub/src-tauri/src/cleanup.rs`, `cache.rs`):
    /// `{ "version": 1, "data": <report> }` in `~/Library/Application Support/the app`.
    static let formatVersion = 1

    private static var url: URL {
        // The launcher's one the app folder (a scratch folder under test).
        LauncherStorage.pulseDirectory.appendingPathComponent("cleanup-findings-v1.json")
    }

    private struct Saved: Decodable {
        let version: Int
        let data: Report
    }

    private struct Report: Decodable {
        let findings: [Finding]
        let safeBytes: UInt64
        let scannedAt: Double

        enum CodingKeys: String, CodingKey {
            case findings
            case safeBytes = "safe_bytes"
            case scannedAt = "scanned_at"
        }
    }

    /// No fields are kept: only the number of findings is read.
    private struct Finding: Decodable {}

    /// The saved summary, or nil when no scan has been saved or the file is unreadable or
    /// from another format version. Synchronous file read: call off the main actor.
    static func load() -> LauncherCleanupSummary? {
        guard let bytes = try? Data(contentsOf: url),
              let saved = try? JSONDecoder().decode(Saved.self, from: bytes),
              saved.version == formatVersion
        else { return nil }
        return LauncherCleanupSummary(
            items: saved.data.findings.count,
            freeBytes: Int64(clamping: saved.data.safeBytes),
            scannedAt: Date(timeIntervalSince1970: saved.data.scannedAt))
    }

    /// "Found 2 h ago", from the scan's own time.
    static func foundText(_ scannedAt: Date, now: Date = Date()) -> String {
        let seconds = max(0, now.timeIntervalSince(scannedAt))
        if seconds < 60 { return "Found just now" }
        if seconds < 3_600 { return "Found \(Int(seconds / 60)) min ago" }
        if seconds < 86_400 { return "Found \(Int(seconds / 3_600)) h ago" }
        return "Found \(Int(seconds / 86_400)) d ago"
    }
}

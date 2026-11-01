import Foundation
import RightKitHub
import SwiftUI

/// Wording and small pure helpers for the Storage section, ported from `hub/src/views/Storage.tsx`,
/// `storageEstimate.ts`, `chart.ts` and `api.ts`.
enum StorageText {
    // Windows has the Recycle Bin and File Explorer; the Mac has the Trash and Finder.
    #if os(Windows)
    static let trash = "Recycle Bin"
    static let fileManager = "File Explorer"
    static let accessNotice = "Some folders couldn't be read. Allow file system access in Windows Settings to include them."
    static let accessButton = "Open File system settings"
    #else
    static let trash = "Trash"
    static let fileManager = "Finder"
    static let accessNotice = "Some folders couldn't be read. Grant Full Disk Access to include them."
    static let accessButton = "Open Full Disk Access"
    #endif

    /// Wording for a size the scan could not prove would be freed. Same text as Cleanup's estimate note.
    static let estimate = "An estimate from allocated size on disk, not file size. Hard links and clones can make the real gain smaller."

    /// A size as the Tauri hub's `bytes()` writes it: decimal units, one decimal below 100 ("27.4 GB",
    /// "39.0 MB", "8 B"). `ByteCountFormatter` says "27.19 GB" and "Zero KB", which the page never does.
    static func bytes(_ value: Int64) -> String {
        let units = ["B", "KB", "MB", "GB", "TB"]
        var amount = Double(value)
        var unit = 0
        while amount >= 1000 && unit < units.count - 1 {
            amount /= 1000
            unit += 1
        }
        if amount >= 100 || unit == 0 { return "\(Int(amount.rounded())) \(units[unit])" }
        let tenths = Int((amount * 10).rounded())
        return "\(tenths / 10).\(tenths % 10) \(units[unit])"
    }

    /// Any removal figure is allocated size, so it is an upper bound; a partial scan reads as a floor.
    static func upTo(_ bytes: Int64, partial: Bool = false) -> String {
        "\(partial ? "≥ " : "up to ")\(StorageText.bytes(bytes))"
    }

    /// Items in the Trash keep their space until the Trash is emptied.
    static var trashFreesNothing: String {
        "Items moved to the \(trash) free no space until the \(trash) is emptied."
    }

    static func signedBytes(_ value: Int64) -> String {
        "\(value < 0 ? "−" : "+")\(StorageText.bytes(value < 0 ? -value : value))"
    }

    static func plural(_ n: Int, _ word: String) -> String {
        if n == 1 { return "\(n) \(word)" }
        let es = ["ch", "sh", "s", "x", "z"].contains { word.hasSuffix($0) }
        return "\(n) \(word)\(es ? "es" : "s")"
    }

    static func copies(_ n: Int) -> String { "\(n) cop\(n == 1 ? "y" : "ies")" }

    /// "1,234", as the page's `toLocaleString()`.
    static func count(_ n: Int) -> String {
        let formatter = NumberFormatter()
        formatter.numberStyle = .decimal
        return formatter.string(from: NSNumber(value: n)) ?? String(n)
    }

    /// "~/Documents" for "/Users/demo/Documents".
    static func shortPath(_ path: String) -> String {
        let prefix = "/Users/"
        guard path.hasPrefix(prefix) else { return path }
        let rest = path.dropFirst(prefix.count)
        if rest.isEmpty { return path }
        guard let slash = rest.firstIndex(of: "/") else { return "~" }
        if slash == rest.startIndex { return path }
        return "~" + rest[slash...]
    }

    /// "just now", "5 min ago", "2 h ago", or the date for anything older.
    static func ago(_ secs: Double, now: Date = Date()) -> String {
        let elapsed = max(0, now.timeIntervalSince1970 - secs)
        if elapsed < 90 { return "just now" }
        if elapsed < 3600 { return "\(Int((elapsed / 60).rounded())) min ago" }
        if elapsed < 86400 { return "\(Int((elapsed / 3600).rounded())) h ago" }
        return monthDay(secs)
    }

    /// "Oct 14".
    static func monthDay(_ secs: Double) -> String {
        let formatter = DateFormatter()
        formatter.setLocalizedDateFormatFromTemplate("MMMd")
        return formatter.string(from: Date(timeIntervalSince1970: secs))
    }

    /// "Oct 14, 2026".
    static func fullDay(_ secs: Double) -> String {
        let formatter = DateFormatter()
        formatter.dateStyle = .medium
        formatter.timeStyle = .none
        return formatter.string(from: Date(timeIntervalSince1970: secs))
    }

    /// A time as the local date and time.
    static func localTime(_ secs: Double) -> String {
        let formatter = DateFormatter()
        formatter.dateStyle = .medium
        formatter.timeStyle = .short
        return formatter.string(from: Date(timeIntervalSince1970: secs))
    }

    /// "136.3 GB more reclaimable by macOS" when it is at least 1 GB and 2% of the drive, else nil.
    static func reclaimableNote(_ volume: StorageVolume) -> String? {
        guard let r = volume.reclaimable_bytes, let total = volume.total_bytes, total > 0 else { return nil }
        if r >= 1_000_000_000 && Double(r) >= Double(total) * 0.02 {
            return "\(StorageText.bytes(r)) more reclaimable by macOS"
        }
        return nil
    }

    /// A number as JavaScript prints it: no trailing ".0".
    static func trimmed(_ value: Double) -> String {
        value == value.rounded() ? String(Int64(value)) : String(value)
    }

    /// The tone of a 0-1 share, matching the notch rings.
    static func tone(_ fraction: Double) -> HubTone {
        if fraction >= 0.85 { return .bad }
        if fraction >= 0.6 { return .warn }
        return .ok
    }

    /// Findings survive leaving and returning to Storage; they are only rescanned on demand.
    static let staleSeconds: Double = 10 * 60
    static func isStale(_ secs: Double, now: Date = Date()) -> Bool {
        now.timeIntervalSince1970 - secs > staleSeconds
    }

    /// Without a live refresh, a saved scan younger than this is shown as it is on open.
    static let snapshotSeconds: Double = 6 * 60 * 60
    static func isOld(_ secs: Double, now: Date = Date()) -> Bool {
        now.timeIntervalSince1970 - secs > snapshotSeconds
    }

    /// A failure as the page shows it: the engine's message.
    static func describe(_ error: any Error) -> String {
        if let engine = error as? EngineError { return engine.message }
        return String(describing: error)
    }
}

// ---- Kinds (colours of the bars) ----

/// The nine kinds and their colours are Petal's (`hub/src/chart.ts`); plain path rules judge kind,
/// so a wrong guess only costs a wrong colour. It never decides what is safe to delete.
enum StorageKind: String, CaseIterable, Sendable {
    case apps, downloads, developer, caches, media, documents, appdata, system, mixed

    var label: String {
        switch self {
        case .apps: return "Apps"
        case .downloads: return "Downloads & Trash"
        case .developer: return "Developer files"
        case .caches: return "Caches & logs"
        case .media: return "Photos, video & music"
        case .documents: return "Documents"
        case .appdata: return "App data & backups"
        case .system: return "System"
        case .mixed: return "Other"
        }
    }

    var color: Color {
        switch self {
        case .apps: return Color(red: 0xd9 / 255, green: 0x59 / 255, blue: 0x26 / 255)
        case .downloads: return Color(red: 0x39 / 255, green: 0x87 / 255, blue: 0xe5 / 255)
        case .developer: return Color(red: 0x00 / 255, green: 0x83 / 255, blue: 0x00 / 255)
        case .caches: return Color(red: 0x19 / 255, green: 0x9e / 255, blue: 0x70 / 255)
        case .media: return Color(red: 0xd5 / 255, green: 0x51 / 255, blue: 0x81 / 255)
        case .documents: return Color(red: 0xc9 / 255, green: 0x85 / 255, blue: 0x00 / 255)
        case .appdata: return Color(red: 0x90 / 255, green: 0x85 / 255, blue: 0xe9 / 255)
        case .system: return Color(red: 0x5b / 255, green: 0x5e / 255, blue: 0x66 / 255)
        case .mixed: return Color(red: 0x6b / 255, green: 0x6e / 255, blue: 0x78 / 255)
        }
    }

    private static let mediaExtensions: Set<String> = [
        "mov", "mp4", "m4v", "mkv", "avi", "mp3", "m4a", "wav", "flac", "jpg", "jpeg", "png", "heic", "raw",
        "cr2", "dng", "photoslibrary", "musiclibrary", "fcpbundle",
    ]
    private static let developerNames: Set<String> = [
        "node_modules", "target", ".build", "DerivedData", ".git", ".cargo", ".rustup", ".npm", ".pnpm-store",
        ".gradle", ".nvm", "Developer", "Pods", "venv", ".venv", "dist",
    ]

    /// The kind of a path, from plain rules (`kindOf` in `chart.ts`).
    static func of(path: String, isDir: Bool) -> StorageKind {
        let name = path.split(separator: "/", omittingEmptySubsequences: true).last.map(String.init) ?? ""
        let rel = StorageText.shortPath(path)
        if rel.contains("/Library/Caches") || rel.contains("/Library/Logs") || name == ".cache" || name == "Caches" {
            return .caches
        }
        if rel.hasPrefix("/Applications") || name.hasSuffix(".app") { return .apps }
        if developerNames.contains(name) || rel.contains("/Library/Developer") || rel.contains("/node_modules/") {
            return .developer
        }
        if rel.hasPrefix("~/Downloads") || rel.hasPrefix("~/.Trash") { return .downloads }
        let ext = name.lastIndex(of: ".").map { String(name[name.index(after: $0)...]).lowercased() } ?? ""
        if mediaExtensions.contains(ext) || ["~/Movies", "~/Music", "~/Pictures"].contains(where: { rel == $0 || rel.hasPrefix($0 + "/") }) {
            return .media
        }
        if ["~/Documents", "~/Desktop"].contains(where: { rel == $0 || rel.hasPrefix($0 + "/") }) { return .documents }
        if rel.hasPrefix("~/Library") { return .appdata }
        if ["/System", "/private", "/usr", "/bin", "/sbin", "/var", "/opt", "/cores", "/Library"]
            .contains(where: { rel == $0 || rel.hasPrefix($0 + "/") }) {
            return .system
        }
        return isDir ? .mixed : .documents
    }
}

// ---- Filters ----

/// The Kind filter. Folders and app bundles are told apart by the row, the rest by extension.
enum StorageKindFilter: String, CaseIterable, Sendable, Hashable {
    case all, folders, documents, images, video, audio, archives, code, apps, other

    var label: String {
        switch self {
        case .all: return "All kinds"
        case .folders: return "Folders"
        case .documents: return "Documents"
        case .images: return "Images"
        case .video: return "Video"
        case .audio: return "Audio"
        case .archives: return "Archives"
        case .code: return "Code"
        case .apps: return "Apps"
        case .other: return "Other"
        }
    }

    /// Extensions per file kind, checked in order. A file in none of them is Other.
    private static let fileKinds: [(StorageKindFilter, Set<String>)] = [
        (.apps, ["dmg", "pkg", "exe", "msi"]),
        (.documents, ["pdf", "doc", "docx", "pages", "rtf", "txt", "md", "odt", "xls", "xlsx", "numbers", "csv", "ppt", "pptx", "key", "epub"]),
        (.images, ["jpg", "jpeg", "png", "gif", "heic", "heif", "tif", "tiff", "webp", "svg", "bmp", "raw", "cr2", "dng", "psd"]),
        (.video, ["mov", "mp4", "m4v", "mkv", "avi", "webm", "mpg", "mpeg"]),
        (.audio, ["mp3", "m4a", "wav", "flac", "aac", "aiff", "ogg"]),
        (.archives, ["zip", "tar", "gz", "tgz", "bz2", "xz", "7z", "rar", "iso"]),
        (.code, ["js", "jsx", "ts", "tsx", "rs", "swift", "py", "go", "java", "kt", "c", "h", "cpp", "hpp", "cs", "rb", "php", "sh", "json", "yaml", "yml", "toml", "html", "css", "scss", "xml", "sql"]),
    ]

    /// The filter kind an item belongs to. Only real items reach here (not the "smaller files" total).
    static func of(_ row: StorageRow) -> StorageKindFilter {
        if row.is_dir { return row.name.lowercased().hasSuffix(".app") ? .apps : .folders }
        let name = row.name
        var ext = ""
        if let dot = name.lastIndex(of: "."), dot != name.startIndex {
            ext = String(name[name.index(after: dot)...]).lowercased()
        }
        for (kind, extensions) in fileKinds where extensions.contains(ext) { return kind }
        return .other
    }
}

/// Size filter: an item must be larger than the threshold (0 means any size). Decimal units, as the page shows them.
enum StorageSizeFilter {
    static let options: [HubChoiceOption<Int64>] = [
        HubChoiceOption(value: 0, title: "Any size"),
        HubChoiceOption(value: 100_000_000, title: "Over 100 MB"),
        HubChoiceOption(value: 1_000_000_000, title: "Over 1 GB"),
        HubChoiceOption(value: 10_000_000_000, title: "Over 10 GB"),
    ]

    static func label(_ value: Int64) -> String {
        options.first { $0.value == value }?.title ?? "Any size"
    }
}

/// Changed filter: when an item (a folder: anything inside it) was last modified.
enum StorageChangedFilter: String, CaseIterable, Sendable, Hashable {
    case any, week, month, year, older

    var label: String {
        switch self {
        case .any: return "Any time"
        case .week: return "Last 7 days"
        case .month: return "Last 30 days"
        case .year: return "Last year"
        case .older: return "Older than a year"
        }
    }

    /// The modified-time range this choice means at `now` (Unix seconds); nil for any time.
    func range(now: Double) -> StorageChangedRange? {
        let day: Double = 86_400
        switch self {
        case .any: return nil
        case .week: return StorageChangedRange(since: now - 7 * day, before: nil)
        case .month: return StorageChangedRange(since: now - 30 * day, before: nil)
        case .year: return StorageChangedRange(since: now - 365 * day, before: nil)
        case .older: return StorageChangedRange(since: nil, before: now - 365 * day)
        }
    }
}

// ---- Findings, grouped ----

struct StorageFindingGroup: Identifiable, Sendable {
    let key: String
    let label: String
    var items: [StorageFinding]
    var bytes: Int64
    var partial: Bool
    /// "safe", or "review" when any item needs review.
    var risk: String
    let reason: String
    let discovered: Bool

    var id: String { key }

    /// "3 items", "2 projects", or nothing for a single item.
    var countText: String {
        items.count == 1 ? "" : discovered ? StorageText.plural(items.count, "project") : StorageText.plural(items.count, "item")
    }

    static func group(_ list: [StorageFinding]) -> [StorageFindingGroup] {
        var order: [String] = []
        var map: [String: StorageFindingGroup] = [:]
        for finding in list {
            if var group = map[finding.rule_id] {
                group.items.append(finding)
                group.bytes += finding.bytes
                group.partial = group.partial || finding.partial
                if finding.risk != "safe" { group.risk = "review" }
                map[finding.rule_id] = group
            } else {
                let label = finding.rule_name ?? finding.name
                let labelOrName = label.isEmpty ? finding.name : label
                order.append(finding.rule_id)
                map[finding.rule_id] = StorageFindingGroup(
                    key: finding.rule_id,
                    label: labelOrName,
                    items: [finding],
                    bytes: finding.bytes,
                    partial: finding.partial,
                    risk: finding.risk == "safe" ? "safe" : "review",
                    reason: finding.reason,
                    discovered: finding.name.hasPrefix("\(labelOrName) · ")
                )
            }
        }
        return order.compactMap { map[$0] }.sorted { $0.bytes > $1.bytes }
    }
}

/// The symbol of a note: a command, a running app, or plain information.
func storageNoteSymbol(_ finding: StorageFinding) -> String {
    let text = "\(finding.action) \(finding.reason)".lowercased()
    if text.contains("xcrun") || text.contains("terminal") || text.contains("command") { return "terminal" }
    if text.contains("in use") || text.contains("running") { return "waveform.path.ecg" }
    return "info.circle"
}

import Foundation
import Observation
import RightKitHub

/// One timeline row from `activity_timeline`. `at` is Unix seconds; `open` names the hub section
/// that owns the record ("cleanup", "apps", "storage", "chat") or is nil.
struct ActivityRowDTO: Codable, Sendable, Hashable {
    let at: Int64
    let kind: String
    let summary: String
    let open: String?

    /// The page's row identity: the same record is never shown twice when pages overlap.
    var key: String { "\(at)|\(kind)|\(summary)" }
}

/// The `activity_timeline` result. `unreadable` names sources that exist but could not be read.
struct ActivityTimelineDTO: Decodable, Sendable {
    let rows: [ActivityRowDTO]
    let unreadable: [String]?
    let totals: [ActivityTotalDTO]?
}

/// Totals for one UTC period as the engine reads them over the merged records (#324); `from` and
/// `to` are Unix seconds, UTC.
struct ActivityTotalDTO: Decodable, Sendable, Hashable {
    let period: String
    let from: Int64
    let to: Int64
    let events: Int
    let transfers: Int
    let movedBytes: Int64

    enum CodingKeys: String, CodingKey {
        case period, from, to, events, transfers
        case movedBytes = "moved_bytes"
    }
}

/// The `activity_timeline` params. `before` is Unix seconds, exclusive; nil is omitted, which the
/// engine reads the same as the page's `null`.
struct ActivityTimelineParams: Encodable, Sendable {
    let limit: Int
    let before: Int64?
}

/// One day's rows under a heading ("Today", "Yesterday" or the date).
struct ActivityDayGroup: Sendable {
    let label: String
    let rows: [ActivityRowDTO]
}

/// Activity's state: the rows loaded so far, the kind filter and the paging. Mirrors
/// `hub/src/views/Activity.tsx`.
@MainActor
@Observable
final class ActivityModel {
    /// Rows per page, both for the newest page and for each "Load older".
    static let page = 50
    /// Refresh while the section is open. Never faster than 30 seconds.
    static let refreshSeconds = 30
    /// The kinds in filter order; only kinds present in the loaded rows are offered.
    static let kindOrder = ["Cleanup", "Undo", "Uninstall", "App update", "Drive health", "Scan", "Send", "Chat", "Remote", "File index"]
    /// Hub section titles for the "Open ..." button.
    static let sectionTitles = ["cleanup": "Cleanup", "apps": "Apps", "storage": "Storage", "chat": "Chat"]

    private let engine: any EngineClient

    /// nil until the first page arrives.
    private(set) var rows: [ActivityRowDTO]?
    private(set) var unreadable: [String] = []
    private(set) var totals: [ActivityTotalDTO] = []
    private(set) var more = false
    private(set) var busy = false
    private(set) var error: String?
    var filter = "All"

    init(engine: any EngineClient) {
        self.engine = engine
    }

    // MARK: Derived

    /// The kinds present in the loaded rows, in filter order.
    var kinds: [String] {
        let present = Set((rows ?? []).map(\.kind))
        return Self.kindOrder.filter { present.contains($0) }
    }

    /// The filter in effect: "All" when the chosen kind is no longer present.
    var currentFilter: String { kinds.contains(filter) ? filter : "All" }

    var visibleRows: [ActivityRowDTO] {
        let current = currentFilter
        return (rows ?? []).filter { current == "All" || $0.kind == current }
    }

    /// The visible rows grouped by day, newest first.
    var groups: [ActivityDayGroup] {
        let now = Date()
        var out: [(label: String, rows: [ActivityRowDTO])] = []
        for row in visibleRows {
            let label = ActivityFormat.dayLabel(at: row.at, now: now)
            if let last = out.last, last.label == label {
                out[out.count - 1].rows.append(row)
            } else {
                out.append((label, [row]))
            }
        }
        return out.map { ActivityDayGroup(label: $0.label, rows: $0.rows) }
    }

    // MARK: Loading

    /// Loads the newest page. `preparedView` awaits this so the off-screen render has data.
    func load() async {
        await refresh()
    }

    /// The section's refresh loop: an optional first load, then one refresh every 30 seconds until
    /// the view goes away (the `.task` is cancelled).
    func run(loadFirst: Bool) async {
        if loadFirst { await refresh() }
        while !Task.isCancelled {
            try? await Task.sleep(for: .seconds(Self.refreshSeconds))
            if Task.isCancelled { break }
            await refresh()
        }
    }

    /// Replaces the newest page, keeping older rows the user has already loaded.
    func refresh() async {
        guard !busy else { return }
        busy = true
        defer { busy = false }
        do {
            let page = try await fetch(before: nil)
            let oldest = page.rows.last?.at
            let previous = rows ?? []
            let older = oldest.map { cutoff in previous.filter { $0.at < cutoff } } ?? []
            rows = page.rows + older
            unreadable = page.unreadable ?? []
            totals = page.totals ?? []
            more = more || page.rows.count >= Self.page
            error = nil
        } catch is CancellationError {
            return
        } catch {
            self.error = Self.describe(error)
        }
    }

    /// Appends the next older page.
    func loadOlder() async {
        guard !busy, let last = rows?.last else { return }
        busy = true
        defer { busy = false }
        do {
            // `before` is exclusive; one second past the oldest row shown brings back its whole
            // second, and rows already shown are dropped below.
            let page = try await fetch(before: last.at + 1)
            let seen = Set((rows ?? []).map(\.key))
            rows = (rows ?? []) + page.rows.filter { !seen.contains($0.key) }
            unreadable = page.unreadable ?? []
            more = page.rows.count >= Self.page
            error = nil
        } catch is CancellationError {
            return
        } catch {
            self.error = Self.describe(error)
        }
    }

    private func fetch(before: Int64?) async throws -> ActivityTimelineDTO {
        try await engine.call(
            "activity_timeline",
            ActivityTimelineParams(limit: Self.page, before: before),
            as: ActivityTimelineDTO.self,
            timeout: .seconds(30)
        )
    }

    private static func describe(_ error: any Error) -> String {
        (error as? EngineError)?.message ?? String(describing: error)
    }
}

/// Day and clock wording for the timeline: "Today", "Yesterday", or "Tue, Oct 13, 2026"; clock as
/// "9:41 AM". Formatters are main-actor state.
@MainActor
enum ActivityFormat {
    private static let dateFormatter: DateFormatter = {
        let formatter = DateFormatter()
        formatter.setLocalizedDateFormatFromTemplate("EEEMMMdy")
        return formatter
    }()

    private static let clockFormatter: DateFormatter = {
        let formatter = DateFormatter()
        formatter.timeStyle = .short
        formatter.dateStyle = .none
        return formatter
    }()

    static func dayLabel(at seconds: Int64, now: Date) -> String {
        let calendar = Calendar.current
        let date = Date(timeIntervalSince1970: Double(seconds))
        let day = calendar.startOfDay(for: date)
        let today = calendar.startOfDay(for: now)
        if day == today { return "Today" }
        if let yesterday = calendar.date(byAdding: .day, value: -1, to: today), day == yesterday {
            return "Yesterday"
        }
        return dateFormatter.string(from: date)
    }

    static func clock(at seconds: Int64) -> String {
        clockFormatter.string(from: Date(timeIntervalSince1970: Double(seconds)))
    }
}

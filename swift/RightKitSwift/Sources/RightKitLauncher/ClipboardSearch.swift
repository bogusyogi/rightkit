#if os(macOS)
import Foundation

// Search, ranking and filters (docs/clipboard-spec.md 5). A linear scan over the items held in
// memory; the strip and the launcher's clip mode both call `search`, so ranking is identical.

/// 5.1: case and diacritics are ignored.
private let matchOptions: String.CompareOptions = [.caseInsensitive, .diacriticInsensitive]

extension LauncherClipboard {
    // MARK: Parsing

    /// 5.1: splits a query into plain terms and the prefixes it recognises (`type:` or `kind:`,
    /// `app:`, `board:`, `is:pinned`, `before:`, `after:`). Anything else, including a prefix
    /// with a value it cannot use, stays a plain term. `before:` keeps items created before
    /// that local day began; `after:` keeps items created on or after it began.
    nonisolated static func parse(query: String) -> ClipQuery {
        var terms: [String] = []
        var chips: [String] = []
        var filters = ClipFilters()
        let calendar = Calendar.current
        // Composed text (an input method's marked text is never searched) compares in one form.
        let composed = query.precomposedStringWithCanonicalMapping
        for token in composed.split(whereSeparator: { $0.isWhitespace }).map(String.init) {
            guard let colon = token.firstIndex(of: ":") else {
                terms.append(token)
                continue
            }
            let key = token[..<colon].lowercased()
            let value = String(token[token.index(after: colon)...])
            let lowered = value.lowercased()
            switch key {
            case "type", "kind":
                if let kind = kindNamed(lowered) {
                    filters.kinds.insert(kind)
                    chips.append("\(key):\(lowered)")
                } else {
                    terms.append(token)
                }
            case "app":
                if value.isEmpty { terms.append(token) } else {
                    filters.appTerms.append(value)
                    chips.append("app:\(value)")
                }
            case "board":
                if value.isEmpty { terms.append(token) } else {
                    filters.boardTerms.append(value)
                    chips.append("board:\(value)")
                }
            case "is":
                if lowered == "pinned" {
                    filters.pinnedOnly = true
                    chips.append("is:pinned")
                } else {
                    terms.append(token)
                }
            case "before", "after":
                if let day = day(from: value) {
                    let start = calendar.startOfDay(for: day)
                    if key == "before" { filters.before = start } else { filters.after = start }
                    chips.append("\(key):\(value)")
                } else {
                    terms.append(token)
                }
            default:
                terms.append(token)
            }
        }
        return ClipQuery(terms: terms, chips: chips, filters: filters)
    }

    private nonisolated static func kindNamed(_ name: String) -> ClipKind? {
        switch name {
        case "text", "texts": return .text
        case "link", "links": return .link
        case "colour", "colours", "color", "colors": return .colour
        case "code": return .code
        case "file", "files": return .file
        case "image", "images": return .image
        case "rich": return .rich
        default: return nil
        }
    }

    /// `yyyy-MM-dd` in the local time zone.
    private nonisolated static func day(from text: String) -> Date? {
        let parts = text.split(separator: "-", omittingEmptySubsequences: false).map { Int($0) }
        guard parts.count == 3, let year = parts[0], let month = parts[1], let day = parts[2] else { return nil }
        var components = DateComponents()
        components.year = year
        components.month = month
        components.day = day
        let calendar = Calendar.current
        guard let date = calendar.date(from: components) else { return nil }
        // 2026-02-31 rolls over to March: not a day.
        let back = calendar.dateComponents([.year, .month, .day], from: date)
        guard back.year == year, back.month == month, back.day == day else { return nil }
        return date
    }

    /// The UI's filters and the query's prefixes together (5.3: all combine with AND; several
    /// kinds or apps picked are alternatives).
    private nonisolated static func merged(_ base: ClipFilters, _ more: ClipFilters) -> ClipFilters {
        var out = base
        out.kinds.formUnion(more.kinds)
        out.apps.formUnion(more.apps)
        out.appTerms += more.appTerms
        out.boardTerms += more.boardTerms
        out.pinnedOnly = out.pinnedOnly || more.pinnedOnly
        if let before = more.before { out.before = min(out.before ?? before, before) }
        if let after = more.after { out.after = max(out.after ?? after, after) }
        return out
    }

    // MARK: Search

    /// 5.1 matching, 5.2 ranking, 5.3 filters. An empty query sorts by `lastUsedAt`, newest
    /// first; a board view with no query follows the board's own order.
    func search(_ query: String, filters: ClipFilters = ClipFilters(), limit: Int? = nil) -> [ClipItem] {
        let parsed = Self.parse(query: query)
        let active = Self.merged(filters, parsed.filters)
        let terms = parsed.terms
        let now = Date().timeIntervalSince1970
        let calendar = Calendar.current

        var boardNames: [String: String] = [:]
        for board in boards { boardNames[board.id] = board.name }
        let window = Self.dateWindow(active.date, calendar: calendar)
        let ranged = Self.rangeBounds(active, window: window, calendar: calendar)

        var kept: [(item: ClipItem, score: Int)] = []
        for item in items {
            guard passes(item, active, ranged: ranged, boardNames: boardNames) else { continue }
            if terms.isEmpty {
                kept.append((item, 0))
            } else if let score = Self.score(item, terms: terms, boardNames: boardNames, now: now) {
                kept.append((item, score))
            }
        }

        if !terms.isEmpty {
            kept.sort { $0.score != $1.score ? $0.score > $1.score : $0.item.lastUsedAt > $1.item.lastUsedAt }
        } else if let boardID = active.board, boardID != "pinned",
                  let board = boards.first(where: { $0.id == boardID }) {
            var place: [String: Int] = [:]
            for (index, id) in board.itemOrder.enumerated() { place[id] = index }
            kept.sort { left, right in
                switch (place[left.item.id], place[right.item.id]) {
                case let (a?, b?): return a < b
                case (_?, nil): return true
                case (nil, _?): return false
                case (nil, nil): return left.item.lastUsedAt > right.item.lastUsedAt
                }
            }
        }
        let result = kept.map { $0.item }
        if let limit { return Array(result.prefix(max(0, limit))) }
        return result
    }

    private func passes(_ item: ClipItem, _ filters: ClipFilters,
                        ranged: (from: Double?, to: Double?), boardNames: [String: String]) -> Bool {
        if !filters.kinds.isEmpty, !filters.kinds.contains(item.kind) { return false }
        if !filters.apps.isEmpty {
            guard let app = item.sourceApp, filters.apps.contains(app.id) else { return false }
        }
        for fragment in filters.appTerms {
            guard let app = item.sourceApp,
                  Self.contains(app.id, fragment) || Self.contains(app.name, fragment) else { return false }
        }
        if let board = filters.board {
            if board == "pinned" {
                if !item.pinned { return false }
            } else if !item.boards.contains(board) {
                return false
            }
        }
        for fragment in filters.boardTerms {
            guard item.boards.contains(where: { boardNames[$0].map { Self.contains($0, fragment) } ?? false })
            else { return false }
        }
        if filters.pinnedOnly, !item.pinned { return false }
        if let from = ranged.from, item.createdAt < from { return false }
        if let to = ranged.to, item.createdAt >= to { return false }
        if let stack = filters.stackId, item.meta.stackId != stack { return false }
        return true
    }

    /// The date filter and the `before:` and `after:` prefixes as one half-open range of
    /// Unix seconds: from (included), to (excluded). Local day boundaries.
    private nonisolated static func rangeBounds(_ filters: ClipFilters, window: (from: Date?, to: Date?),
                                                calendar: Calendar) -> (from: Double?, to: Double?) {
        var from = window.from?.timeIntervalSince1970
        var to = window.to?.timeIntervalSince1970
        if let after = filters.after?.timeIntervalSince1970 { from = max(from ?? after, after) }
        if let before = filters.before?.timeIntervalSince1970 { to = min(to ?? before, before) }
        return (from, to)
    }

    private nonisolated static func dateWindow(_ filter: ClipDateFilter, calendar: Calendar) -> (from: Date?, to: Date?) {
        let today = calendar.startOfDay(for: Date())
        func days(_ count: Int) -> Date { calendar.date(byAdding: .day, value: count, to: today) ?? today }
        switch filter {
        case .any: return (nil, nil)
        case .today: return (today, nil)
        case .yesterday: return (days(-1), today)
        case .last7: return (days(-6), nil)
        case .last30: return (days(-29), nil)
        case .range(let start, let end):
            let first = calendar.startOfDay(for: min(start, end))
            let last = calendar.startOfDay(for: max(start, end))
            return (first, calendar.date(byAdding: .day, value: 1, to: last))
        }
    }

    // MARK: Matching and ranking

    private nonisolated static func contains(_ text: String, _ term: String) -> Bool {
        text.range(of: term, options: matchOptions) != nil
    }

    /// nil when some term matches nothing (all terms must match). Otherwise the 5.2 points.
    private nonisolated static func score(_ item: ClipItem, terms: [String], boardNames: [String: String],
                                          now: Double) -> Int? {
        let firstLine = item.preview.split(whereSeparator: { $0.isNewline }).first.map(String.init) ?? ""
        let names = (item.meta.names ?? []).joined(separator: " ")
        let boardText = item.boards.compactMap { boardNames[$0] }
        var points = 0.0
        for term in terms {
            var matched = false
            // The preview, best rule only.
            if !item.preview.isEmpty, contains(item.preview, term) {
                matched = true
                if firstLine.range(of: term, options: matchOptions.union(.anchored)) != nil {
                    points += 40
                } else if matchesWordStart(item.preview, term) {
                    points += 25
                } else {
                    points += 15
                }
            }
            // The source app, host, file names and hex.
            var meta = false
            if let name = item.sourceApp?.name, contains(name, term) { meta = true }
            if let host = item.meta.host, contains(host, term) { meta = true }
            if !names.isEmpty, contains(names, term) { meta = true }
            if let hex = item.meta.hex, contains(hex, term) { meta = true }
            if meta {
                matched = true
                points += 20
            }
            if boardText.contains(where: { contains($0, term) }) {
                matched = true
                points += 10
            }
            if contains(item.kind.rawValue, term) { matched = true }
            if !matched { return nil }
        }
        if item.pinned { points += 10 }
        let ageDays = max(0, now - item.lastUsedAt) / 86_400
        points += 10 * exp(-ageDays / 14)
        return Int((points * 100).rounded())
    }

    private nonisolated static func matchesWordStart(_ text: String, _ term: String) -> Bool {
        var range = text.startIndex..<text.endIndex
        while let found = text.range(of: term, options: matchOptions, range: range) {
            if found.lowerBound == text.startIndex { return true }
            let before = text[text.index(before: found.lowerBound)]
            if !(before.isLetter || before.isNumber) { return true }
            range = found.upperBound..<text.endIndex
        }
        return false
    }

    // MARK: Lookups

    /// The apps seen in the history, most used first (5.3).
    func apps() -> [ClipSourceApp] {
        var uses: [String: (app: ClipSourceApp, count: Int)] = [:]
        for item in items {
            guard let app = item.sourceApp else { continue }
            let known = uses[app.id]?.count ?? 0
            uses[app.id] = (uses[app.id]?.app ?? app, known + 1 + item.useCount)
        }
        return uses.values.sorted {
            $0.count != $1.count ? $0.count > $1.count : $0.app.name.localizedCaseInsensitiveCompare($1.app.name) == .orderedAscending
        }.map { $0.app }
    }

    func board(named name: String) -> ClipBoard? {
        let wanted = name.trimmingCharacters(in: .whitespaces)
        return boards.first { $0.name.compare(wanted, options: .caseInsensitive) == .orderedSame }
    }
}
#endif

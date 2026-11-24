import Foundation

/// One read of a bounded history. A cursor belongs to the snapshot that produced it.
/// Positions are opaque ASCII ordering keys; the first '#' starts a tied occurrence.
public struct RightKitHistoryWindow<Row, Total>: Sendable
where Row: Decodable & Identifiable & Sendable, Row.ID == String, Total: Decodable & Sendable {
    public struct Entry: Decodable, Sendable {
        public let row: Row
        let position: String
        let cursor: String
    }

    struct Page: Decodable, Sendable {
        let entries: [Entry]
        let totals: [Total]
        let unreadable: [String]
        let next: String?
        enum CodingKeys: String, CodingKey {
            case entries, totals, unreadable
            case next = "next_cursor"
        }
    }

    public struct Reply: Decodable, Sendable {
        let page: Page?
        enum CodingKeys: String, CodingKey { case expired }
        public init(from decoder: any Decoder) throws {
            let keyed = try decoder.container(keyedBy: CodingKeys.self)
            if try keyed.decodeIfPresent(Bool.self, forKey: .expired) == true {
                page = nil
            } else {
                page = try Page(from: decoder)
            }
        }
    }

    private var entries: [Entry]
    public var rows: [Row] { entries.map(\.row) }
    public private(set) var totals: [Total]
    public private(set) var unreadable: [String]
    private var cursor: String?
    public var hasMore: Bool { cursor != nil }

    @MainActor
    public static func read(previous: Self?, older: Bool,
                            fetch: @MainActor (Int, String?) async throws -> Reply) async throws -> Self {
        if older, let previous, !previous.hasMore { return previous }
        if let result = try await readWindow(previous, append: older, fetch: fetch) { return result }
        guard let refreshed = try await readWindow(previous, append: false, fetch: fetch) else {
            throw RightKitHistoryError.expired
        }
        if older && refreshed.hasMore {
            guard let extended = try await readWindow(refreshed, append: true, fetch: fetch) else {
                throw RightKitHistoryError.expired
            }
            return extended
        }
        return refreshed
    }

    @MainActor
    private static func readWindow(_ previous: Self?, append: Bool,
                                   fetch: @MainActor (Int, String?) async throws -> Reply) async throws -> Self? {
        var output = Self(entries: [], totals: [], unreadable: [], cursor: nil)
        if append, let previous { output = previous }
        let retained = previous?.entries.last?.position.components(separatedBy: "#").first
        var first = true
        while true {
            try Task.checkCancellation()
            let limit = first ? 50 : 200
            let reply = try await fetch(limit, output.cursor)
            try Task.checkCancellation()
            guard let page = reply.page else { return nil }
            guard page.entries.count <= limit,
                  page.next == nil || page.next == page.entries.last?.cursor else {
                throw RightKitHistoryError.invalidPage
            }
            var last = output.entries.last?.position
            for item in page.entries {
                guard !item.row.id.isEmpty, !item.position.isEmpty, !item.cursor.isEmpty,
                      last.map({ item.position < $0 }) ?? true else {
                    throw RightKitHistoryError.invalidPage
                }
                last = item.position
            }
            if !append, var previous, !previous.entries.isEmpty, !page.unreadable.isEmpty {
                previous.unreadable = page.unreadable
                return previous
            }
            if first && !append { output.totals = page.totals }
            if append {
                for warning in page.unreadable where !output.unreadable.contains(warning) {
                    output.unreadable.append(warning)
                }
            } else {
                output.unreadable = page.unreadable
            }
            for entry in page.entries {
                if !append, let retained, output.entries.count >= 50, entry.position < retained {
                    output.cursor = output.entries.last?.cursor
                    return output
                }
                output.entries.append(entry)
            }
            output.cursor = page.next
            if append || page.next == nil || retained == nil || output.entries.last?.position == retained {
                return output
            }
            first = false
        }
    }
}

public enum RightKitHistoryError: Error, Sendable {
    case expired
    case invalidPage
}

import Foundation

/// Secret-free session projection shared by the app and its WidgetKit extension.
struct WidgetSession: Codable, Equatable, Hashable, Identifiable, Sendable {
    let id: String
    let title: String
    let project: String?
    let needsAttention: Bool
    let updatedAt: String?

    init(
        id: String,
        title: String,
        project: String? = nil,
        needsAttention: Bool = false,
        updatedAt: String? = nil
    ) {
        self.id = id
        self.title = title
        self.project = project
        self.needsAttention = needsAttention
        self.updatedAt = updatedAt
    }

    var displayTitle: String { title.isEmpty ? "Untitled session" : title }
}

/// Latest app-written overview. It intentionally contains no daemon URL or bearer token.
struct WidgetSnapshot: Codable, Equatable, Sendable {
    static let currentVersion = 1
    static let maxAge: TimeInterval = 60 * 60

    let version: Int
    let generatedAt: Date
    let runtimeLabel: String?
    let attentionCount: Int
    let recentSessions: [WidgetSession]

    static let empty = WidgetSnapshot(
        generatedAt: .distantPast,
        runtimeLabel: nil,
        attentionCount: 0,
        recentSessions: []
    )

    init(
        generatedAt: Date = Date(),
        runtimeLabel: String? = nil,
        attentionCount: Int = 0,
        recentSessions: [WidgetSession] = []
    ) {
        version = Self.currentVersion
        self.generatedAt = generatedAt
        self.runtimeLabel = runtimeLabel
        self.attentionCount = max(0, attentionCount)
        self.recentSessions = Array(recentSessions.prefix(6))
    }

    var isEmpty: Bool { recentSessions.isEmpty && attentionCount == 0 }

    func isStale(at date: Date = Date(), maxAge: TimeInterval = Self.maxAge) -> Bool {
        generatedAt == .distantPast || date.timeIntervalSince(generatedAt) > maxAge
    }

    private enum CodingKeys: String, CodingKey {
        case version, generatedAt, runtimeLabel, attentionCount, recentSessions
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        version = (try? container.decode(Int.self, forKey: .version)) ?? Self.currentVersion
        generatedAt = (try? container.decode(Date.self, forKey: .generatedAt)) ?? .distantPast
        runtimeLabel = try? container.decodeIfPresent(String.self, forKey: .runtimeLabel)
        attentionCount = max(0, (try? container.decode(Int.self, forKey: .attentionCount)) ?? 0)
        recentSessions = Array((try? container.decode([WidgetSession].self, forKey: .recentSessions))?.prefix(6) ?? [])
    }
}

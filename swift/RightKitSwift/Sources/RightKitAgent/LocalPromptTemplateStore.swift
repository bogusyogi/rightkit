import Foundation

/// product atom 20: reusable prompt templates and slash commands, insertable into the
/// composer (`ComposerBarPort`). Templates are user-authored text snippets keyed by
/// a `/name` slash trigger; the command palette (`CommandPaletteStore`) also surfaces them as
/// results so the same catalog powers typed slash commands and palette search.
///
/// Persistence is local (`UserDefaults`-backed JSON). Optional iCloud sync
/// (`PromptTemplateCloudSync`, off by default) merges by `updatedAt`, last writer wins.
public struct LocalPromptTemplate: Identifiable, Codable, Hashable, Sendable {
    public let id: UUID
    /// Slash trigger without the leading `/`, e.g. "explain". Lowercase, no whitespace.
    public var command: String
    /// Human-readable label shown in the palette and picker.
    public var title: String
    /// The literal text inserted into the composer draft when chosen.
    public var body: String
    /// Version stamp for sync conflict resolution; the store bumps it on every edit.
    public var updatedAt: Date

    public init(id: UUID = UUID(), command: String, title: String, body: String, updatedAt: Date = Date()) {
        self.id = id
        self.command = Self.normalizeCommand(command)
        self.title = title
        self.body = body
        self.updatedAt = updatedAt
    }

    /// JSON saved before sync existed has no `updatedAt`; it decodes as the epoch so any
    /// real remote edit wins over an unedited legacy copy.
    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(UUID.self, forKey: .id)
        command = try container.decode(String.self, forKey: .command)
        title = try container.decode(String.self, forKey: .title)
        body = try container.decode(String.self, forKey: .body)
        updatedAt = try container.decodeIfPresent(Date.self, forKey: .updatedAt)
            ?? Date(timeIntervalSince1970: 0)
    }

    public static func normalizeCommand(_ raw: String) -> String {
        raw.lowercased()
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .filter { !$0.isWhitespace }
    }
}

/// Observable store for the user's prompt template catalog. `@Observable` per house style;
/// views read `templates` directly rather than through a Combine publisher.
@MainActor
@Observable
public final class LocalPromptTemplateStore {
    public private(set) var templates: [LocalPromptTemplate]

    /// Deleted template ids and when they were deleted. Kept so sync can tell a delete
    /// from "never had it" and a stale copy on another device cannot resurrect the row.
    public private(set) var tombstones: [UUID: Date]

    private let defaultsKey = "app.coderight.CodeRight.promptTemplates"
    private let tombstonesKey = "app.coderight.CodeRight.promptTemplateTombstones"
    private let userDefaults: UserDefaults
    private let now: @Sendable () -> Date

    public init(userDefaults: UserDefaults = .standard, now: @escaping @Sendable () -> Date = { Date() }) {
        self.userDefaults = userDefaults
        self.now = now
        if let data = userDefaults.data(forKey: defaultsKey),
           let decoded = try? JSONDecoder().decode([LocalPromptTemplate].self, from: data) {
            self.templates = decoded
        } else {
            self.templates = Self.seedTemplates
        }
        if let data = userDefaults.data(forKey: tombstonesKey),
           let decoded = try? JSONDecoder().decode([String: Date].self, from: data) {
            self.tombstones = Dictionary(uniqueKeysWithValues: decoded.compactMap { key, date in
                UUID(uuidString: key).map { ($0, date) }
            })
        } else {
            self.tombstones = [:]
        }
    }

    /// Default catalog shown on first launch so the palette and `/` trigger are never empty.
    /// Fixed ids and an epoch stamp so every device seeds the same rows and sync converges
    /// instead of duplicating them.
    public static let seedTemplates: [LocalPromptTemplate] = [
        LocalPromptTemplate(
            id: UUID(uuidString: "5EED0001-0000-4000-8000-000000000001")!,
            command: "explain",
            title: "Explain this code",
            body: "Explain what this code does and why, in plain language.",
            updatedAt: Date(timeIntervalSince1970: 0)
        ),
        LocalPromptTemplate(
            id: UUID(uuidString: "5EED0001-0000-4000-8000-000000000002")!,
            command: "tests",
            title: "Write tests",
            body: "Write focused unit tests for the change I just described.",
            updatedAt: Date(timeIntervalSince1970: 0)
        ),
        LocalPromptTemplate(
            id: UUID(uuidString: "5EED0001-0000-4000-8000-000000000003")!,
            command: "review",
            title: "Review my diff",
            body: "Review the current diff for correctness and simplification opportunities.",
            updatedAt: Date(timeIntervalSince1970: 0)
        ),
    ]

    /// Templates whose command or title matches `query` (case-insensitive substring).
    /// Empty query returns the full catalog, most-recently-added first.
    public func matching(_ query: String) -> [LocalPromptTemplate] {
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard !trimmed.isEmpty else { return templates }
        return templates.filter {
            $0.command.contains(trimmed) || $0.title.lowercased().contains(trimmed)
        }
    }

    /// Resolves a typed slash command like "/explain" to its template body, or nil if the
    /// text isn't a recognized trigger. Callers use this to intercept a composer submit.
    public func resolveSlashCommand(_ text: String) -> LocalPromptTemplate? {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed.hasPrefix("/") else { return nil }
        let command = LocalPromptTemplate.normalizeCommand(String(trimmed.dropFirst()))
        guard !command.isEmpty else { return nil }
        return templates.first { $0.command == command }
    }

    @discardableResult
    public func add(command: String, title: String, body: String) -> LocalPromptTemplate? {
        let normalized = LocalPromptTemplate.normalizeCommand(command)
        guard !normalized.isEmpty, !title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              !templates.contains(where: { $0.command == normalized })
        else {
            return nil
        }
        let template = LocalPromptTemplate(command: normalized, title: title, body: body, updatedAt: now())
        templates.insert(template, at: 0)
        persist()
        return template
    }

    public func remove(_ id: LocalPromptTemplate.ID) {
        templates.removeAll { $0.id == id }
        tombstones[id] = now()
        persist()
    }

    public func update(_ template: LocalPromptTemplate) {
        guard let index = templates.firstIndex(where: { $0.id == template.id }) else { return }
        var stamped = template
        // An edit is always newer than the copy it replaces, even when this device's clock is
        // behind the one that wrote that copy; otherwise the edit would never sync out.
        stamped.updatedAt = max(now(), templates[index].updatedAt.addingTimeInterval(0.001))
        templates[index] = stamped
        persist()
    }

    /// Merge a sync page: drop deleted ids (recording them as tombstones), then upsert remote
    /// rows unless a tombstone or a newer local edit already covers them. Called by
    /// `PromptTemplateCloudSync`; does not re-stamp `updatedAt`.
    func applyRemote(upserts: [LocalPromptTemplate], deletedIDs: Set<UUID>) {
        for id in deletedIDs {
            templates.removeAll { $0.id == id }
            tombstones[id] = tombstones[id] ?? now()
        }
        for remote in upserts where tombstones[remote.id] == nil {
            if let index = templates.firstIndex(where: { $0.id == remote.id }) {
                guard templates[index].updatedAt < remote.updatedAt else { continue }
                templates[index] = remote
            } else {
                templates.append(remote)
            }
        }
        persist()
    }

    private func persist() {
        if let data = try? JSONEncoder().encode(templates) {
            userDefaults.set(data, forKey: defaultsKey)
        }
        if let data = try? JSONEncoder().encode(Dictionary(uniqueKeysWithValues: tombstones.map { ($0.key.uuidString, $0.value) })) {
            userDefaults.set(data, forKey: tombstonesKey)
        }
    }
}

import SwiftUI
import Observation

/// Mirrors `coderight_skills::registry_catalog::SkillCatalogEntry` field for
/// field. Deliberately excludes the manifest's markdown `body` — a catalog
/// row is metadata for browsing, not the skill's instructions, matching the
/// Rust side's `SkillCatalogEntry` shape (which has no `body` field either).
public struct SkillCatalogEntry: Identifiable, Codable, Equatable, Sendable {
    public enum Category: String, Codable, Sendable {
        case tool
        case behavioral
    }

    public enum Lifecycle: String, Codable, Sendable {
        case proposed
        case quarantined
        case benchmarked
        case approved
        case active
        case retired
    }

    public enum SourceScope: String, Codable, Sendable {
        case bundled
        case user
        case workspaceCompatibility = "workspace_compatibility"
        case workspace
    }

    public var id: String { name }
    public var name: String
    public var description: String
    public var category: Category
    public var lifecycle: Lifecycle
    public var isActive: Bool
    public var scope: SourceScope
    public var tags: [String]
    public var version: String?
    public var author: String?
    public var sourcePath: String

    public init(
        name: String,
        description: String,
        category: Category,
        lifecycle: Lifecycle,
        isActive: Bool,
        scope: SourceScope,
        tags: [String],
        version: String?,
        author: String?,
        sourcePath: String
    ) {
        self.name = name
        self.description = description
        self.category = category
        self.lifecycle = lifecycle
        self.isActive = isActive
        self.scope = scope
        self.tags = tags
        self.version = version
        self.author = author
        self.sourcePath = sourcePath
    }
}

/// Catalog metadata comes from shared engine discovery over app-owned roots.
public protocol SkillCatalogSource: Sendable {
    func fetchCatalog() async throws -> [SkillCatalogEntry]
}

public struct LocalSkillCatalogSource: SkillCatalogSource {
    public let roots: [String]
    public init(roots: [String] = []) { self.roots = roots }
    public func fetchCatalog() async throws -> [SkillCatalogEntry] {
        let entries = try await LocalRuntimeBridge.listSkillsCatalog(
            roots: roots.map { .init(scope: "workspace", path: $0) })
        return try JSONDecoder().decode([SkillCatalogEntry].self,
            from: JSONSerialization.data(withJSONObject: entries))
    }
}

@MainActor
@Observable
public final class SkillsCatalogStore {
    public private(set) var entries: [SkillCatalogEntry] = []
    public private(set) var isLoading = false
    public private(set) var loadError: String?
    public var searchQuery: String = ""

    private let source: SkillCatalogSource

    public init(source: SkillCatalogSource = LocalSkillCatalogSource()) {
        self.source = source
    }

    public func load() async {
        isLoading = true
        loadError = nil
        defer { isLoading = false }
        do {
            entries = try await source.fetchCatalog()
        } catch {
            loadError = (error as? LocalizedError)?.errorDescription ?? error.localizedDescription
        }
    }

    /// Filters `entries` by the current `searchQuery` over name, description,
    /// and tags — mirrors `coderight_skills::registry_catalog::search_catalog`.
    public var filteredEntries: [SkillCatalogEntry] {
        let needle = searchQuery.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard !needle.isEmpty else { return entries }
        return entries.filter { entry in
            entry.name.lowercased().contains(needle)
                || entry.description.lowercased().contains(needle)
                || entry.tags.contains { $0.lowercased().contains(needle) }
        }
    }
}

/// Browse-only catalog of available skills (ENG-105). Lists metadata —
/// name, description, category, lifecycle, tags, source — for every
/// discovered skill; installing a new one is a separate flow gated by the
/// daemon's `coderight_skills::registry_install::validate_for_install`
/// pipeline, which this view does not perform directly (see gap note).
@MainActor
public struct SkillsCatalogView: View {
    @State private var store: SkillsCatalogStore

    public init(store: SkillsCatalogStore = SkillsCatalogStore()) {
        _store = State(initialValue: store)
    }

    public var body: some View {
        List {
            if store.isLoading {
                HStack {
                    Spacer()
                    ProgressView()
                    Spacer()
                }
            } else if let loadError = store.loadError {
                ContentUnavailableView(
                    "Couldn't load skills",
                    systemImage: "exclamationmark.triangle",
                    description: Text(loadError)
                )
            } else if store.filteredEntries.isEmpty {
                ContentUnavailableView(
                    "No skills found",
                    systemImage: "puzzlepiece.extension",
                    description: Text(
                        store.searchQuery.isEmpty
                            ? "No skills are installed yet."
                            : "No skills match \"\(store.searchQuery)\"."
                    )
                )
            } else {
                ForEach(store.filteredEntries) { entry in
                    SkillCatalogRow(entry: entry)
                }
            }
        }
        .searchable(text: $store.searchQuery, prompt: "Search skills")
        .scrollContentBackground(.hidden).background(Theme.background)
        .navigationTitle("Skills")
        .task { await store.load() }
        .refreshable { await store.load() }
    }
}

@MainActor
private struct SkillCatalogRow: View {
    let entry: SkillCatalogEntry

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text(entry.name)
                    .font(.body.weight(.medium))
                Spacer()
                if !entry.isActive {
                    Text(entry.lifecycle.rawValue.capitalized)
                        .font(.caption2.weight(.semibold))
                        .padding(.horizontal, 6)
                        .padding(.vertical, 2)
                        .background(Color.secondary.opacity(0.15), in: Capsule())
                }
            }
            Text(entry.description)
                .font(.subheadline)
                .foregroundStyle(.secondary)
            HStack(spacing: 8) {
                Label(entry.category.rawValue.capitalized, systemImage: categoryIcon)
                Label(scopeLabel, systemImage: "tray.2")
                if let version = entry.version {
                    Text("v\(version)")
                }
            }
            .font(.caption)
            .foregroundStyle(.tertiary)
            if !entry.tags.isEmpty {
                Text(entry.tags.joined(separator: ", "))
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
            }
        }
        .padding(.vertical, 2)
    }

    private var categoryIcon: String {
        entry.category == .behavioral ? "wand.and.stars" : "hammer"
    }

    private var scopeLabel: String {
        switch entry.scope {
        case .bundled: return "Bundled"
        case .user: return "User"
        case .workspaceCompatibility: return "Workspace (compat)"
        case .workspace: return "Workspace"
        }
    }
}

import Foundation

/// APP-14: command palette — a superset of slash-command prompt insertion. A palette result
/// can insert text, run an in-app action, or navigate; providers are pluggable so new result
/// kinds (sessions, files, settings, prompt templates) register without the palette itself
/// knowing their internals.
public enum CommandPaletteResultKind: Sendable {
    case insertPrompt(body: String)
    case action(id: String)
    case navigate(routeID: String)
}

public struct CommandPaletteResult: Identifiable, Sendable {
    public let id: String
    public let title: String
    public let subtitle: String?
    public let providerID: String
    public let kind: CommandPaletteResultKind

    public init(
        id: String,
        title: String,
        subtitle: String? = nil,
        providerID: String,
        kind: CommandPaletteResultKind
    ) {
        self.id = id
        self.title = title
        self.subtitle = subtitle
        self.providerID = providerID
        self.kind = kind
    }
}

/// A pluggable source of palette results. Providers are queried independently and their
/// results merged by the store; a slow or empty provider never blocks the others.
public protocol CommandPaletteProvider: Sendable {
    /// Stable identifier used to group/attribute results and for allowlist checks.
    var id: String { get }
    /// Returns matches for `query` (already trimmed; may be empty for a "recent/default" list).
    @MainActor func results(for query: String) -> [CommandPaletteResult]
}

/// Adapts `LocalPromptTemplateStore` (product atom 20) as a palette provider so slash-command
/// templates surface as first-class palette results alongside other providers.
@MainActor
public struct PromptTemplatePaletteProvider: CommandPaletteProvider {
    public let id = "prompt-templates"
    private let store: LocalPromptTemplateStore

    public init(store: LocalPromptTemplateStore) {
        self.store = store
    }

    public func results(for query: String) -> [CommandPaletteResult] {
        store.matching(query).map { template in
            CommandPaletteResult(
                id: "prompt-\(template.id.uuidString)",
                title: "/\(template.command) — \(template.title)",
                subtitle: template.body,
                providerID: id,
                kind: .insertPrompt(body: template.body)
            )
        }
    }
}

/// Observable palette state: holds the registered providers and the merged, query-filtered
/// result list. `@Observable` per house style.
@MainActor
@Observable
public final class CommandPaletteStore {
    public private(set) var providers: [CommandPaletteProvider]
    public var query: String = ""

    public init(providers: [CommandPaletteProvider] = []) {
        self.providers = providers
    }

    public func register(_ provider: CommandPaletteProvider) {
        guard !providers.contains(where: { $0.id == provider.id }) else { return }
        providers.append(provider)
    }

    /// Merged results across every registered provider, in provider-registration order.
    /// Providers are independent: one throwing away all results does not affect another.
    public var results: [CommandPaletteResult] {
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        return providers.flatMap { $0.results(for: trimmed) }
    }
}

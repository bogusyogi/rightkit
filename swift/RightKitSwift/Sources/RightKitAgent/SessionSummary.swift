import Foundation

/// A row from `GET /sessions`. The daemon returns a superset of fields; the client
/// decodes only what the list and header render, tolerating everything else.
public struct SessionSummary: Codable, Identifiable, Equatable, Sendable {
    public let id: String
    public let title: String
    public let model: String
    public var workspaceRoot: String?
    public var executionBackend: String?
    public var updatedAt: String?
    /// Optional session kind. `"external-claude"` marks a Claude Code chat running on the
    /// computer outside CodeRight; absent for ordinary sessions.
    public var driverKind: String?
    /// Optional external-session state; present only alongside `driverKind == "external-claude"`.
    public var external: ExternalClaudeSession?

    /// True for a Claude Code chat already running outside CodeRight. Such sessions cannot be
    /// deleted and do not accept interrupt, steer, approval, answer or plan actions.
    public var isExternalClaude: Bool { driverKind == "external-claude" }

    public init(
        id: String, title: String, model: String,
        workspaceRoot: String? = nil, executionBackend: String? = nil, updatedAt: String? = nil,
        driverKind: String? = nil, external: ExternalClaudeSession? = nil
    ) {
        self.id = id
        self.title = title
        self.model = model
        self.workspaceRoot = workspaceRoot
        self.executionBackend = executionBackend
        self.updatedAt = updatedAt
        self.driverKind = driverKind
        self.external = external
    }

    enum CodingKeys: String, CodingKey {
        case id, title, model, workspaceRoot, executionBackend, updatedAt, driverKind, external
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        title = (try? c.decode(String.self, forKey: .title)) ?? id
        model = (try? c.decode(String.self, forKey: .model)) ?? ""
        workspaceRoot = try? c.decodeIfPresent(String.self, forKey: .workspaceRoot)
        executionBackend = try? c.decodeIfPresent(String.self, forKey: .executionBackend)
        updatedAt = try? c.decodeIfPresent(String.self, forKey: .updatedAt)
        driverKind = try? c.decodeIfPresent(String.self, forKey: .driverKind)
        external = try? c.decodeIfPresent(ExternalClaudeSession.self, forKey: .external)
    }
}

/// The `external` object on a `GET /sessions` row for an external Claude session. Decoded
/// leniently: an unrecognised status falls back to `.idle`, so a newer daemon never breaks the list.
public struct ExternalClaudeSession: Codable, Equatable, Sendable {
    public enum Status: String, Codable, Equatable, Sendable {
        case busy, idle, waiting
    }

    public let host: String
    public let account: String?
    public let status: Status
    public let inputAvailable: Bool

    public init(host: String, account: String? = nil, status: Status = .idle, inputAvailable: Bool = false) {
        self.host = host
        self.account = account
        self.status = status
        self.inputAvailable = inputAvailable
    }

    enum CodingKeys: String, CodingKey {
        case host, account, status, inputAvailable
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        host = (try? c.decode(String.self, forKey: .host)) ?? ""
        account = try? c.decodeIfPresent(String.self, forKey: .account)
        let raw = (try? c.decode(String.self, forKey: .status)) ?? ""
        status = Status(rawValue: raw) ?? .idle
        inputAvailable = (try? c.decode(Bool.self, forKey: .inputAvailable)) ?? false
    }
}

/// Orchestration settings subset: the model list + current primary model, from
/// `GET /settings/orchestration`.
public struct OrchestrationSettings: Decodable, Sendable {
    public let models: [String]
    public let primaryModel: String?

    enum CodingKeys: String, CodingKey {
        case models
        case primaryOrchestratorModelId
    }

    private struct ModelEntry: Decodable { let id: String? }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let entries = (try? c.decode([ModelEntry].self, forKey: .models)) ?? []
        models = entries.compactMap { $0.id }.filter { !$0.isEmpty }
        primaryModel = try? c.decodeIfPresent(String.self, forKey: .primaryOrchestratorModelId)
    }

    public init(models: [String], primaryModel: String?) {
        self.models = models
        self.primaryModel = primaryModel
    }
}

/// Token/cost usage, accumulated from `usageUpdated` events.
public struct UsageState: Equatable, Sendable {
    public var promptTokens: Int
    public var completionTokens: Int
    public var cacheReadTokens: Int
    public var cacheCreationTokens: Int
    public var costUsd: Double

    public static let empty = UsageState(
        promptTokens: 0, completionTokens: 0, cacheReadTokens: 0, cacheCreationTokens: 0, costUsd: 0
    )

    public init(promptTokens: Int, completionTokens: Int, cacheReadTokens: Int, cacheCreationTokens: Int, costUsd: Double) {
        self.promptTokens = promptTokens
        self.completionTokens = completionTokens
        self.cacheReadTokens = cacheReadTokens
        self.cacheCreationTokens = cacheCreationTokens
        self.costUsd = costUsd
    }
}

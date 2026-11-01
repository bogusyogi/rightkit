import Foundation

/// Lifecycle state rendered by a compact tool card. Status includes a message for diagnosis;
/// the view also supplies an icon and text so status is never conveyed by color alone.
public enum ToolActivityStatus: Equatable, Codable, Sendable {
    case running
    case succeeded
    case failed(String)

    public var label: String {
        switch self {
        case .running: return "Running"
        case .succeeded: return "Succeeded"
        case .failed: return "Failed"
        }
    }
}

public enum ToolActivityTransition: Equatable, Sendable {
    case started(name: String, input: String)
    case succeeded(output: String)
    case failed(message: String, output: String)
    /// A local sub-agent (`spawn_subagent`) or MCP call reported an interim
    /// progress update. Applying this never changes `status` — a running
    /// spawn stays `.running` until its own `succeeded`/`failed` transition —
    /// it only refreshes the surfaced progress/budget snapshot so the card
    /// updates in place instead of the activity being dropped from the feed.
    case progressed(SubagentProgress)
}

/// A point-in-time snapshot of one `spawn_subagent` child's bounded
/// execution, surfaced so the transcript never silently swallows local
/// delegation the way it previously did (product atom 15). All fields are
/// plain text/numbers — nothing here is interpreted as markup or executable.
public struct SubagentProgress: Equatable, Codable, Sendable {
    /// The role requested for the child (e.g. "designer", "researcher").
    public var role: String
    /// LLM/tool iterations the child has used so far, when the daemon reports it.
    public var iterationsUsed: Int?
    /// The `maxTurns` ceiling passed to `spawn_subagent`, if any was set.
    public var maxIterations: Int?
    /// Spend so far, in millionths of a US dollar, mirroring the engine's
    /// `SubagentSpawnOptions.budget_microusd` unit to avoid float drift.
    public var spentMicroUsd: UInt64?
    /// The per-child ceiling passed as `SubagentSpawnOptions.budget_microusd`.
    public var budgetMicroUsd: UInt64?
    /// Optional child agent id, present once the daemon has assigned one
    /// (e.g. for `continue_agent` follow-ups).
    public var childAgentId: String?

    public init(
        role: String,
        iterationsUsed: Int? = nil,
        maxIterations: Int? = nil,
        spentMicroUsd: UInt64? = nil,
        budgetMicroUsd: UInt64? = nil,
        childAgentId: String? = nil
    ) {
        self.role = role
        self.iterationsUsed = iterationsUsed
        self.maxIterations = maxIterations
        self.spentMicroUsd = spentMicroUsd
        self.budgetMicroUsd = budgetMicroUsd
        self.childAgentId = childAgentId
    }

    /// "$0.0031" style rendering, or nil when no spend is known yet.
    public var spentDisplay: String? {
        guard let spentMicroUsd else { return nil }
        return String(format: "$%.4f", Double(spentMicroUsd) / 1_000_000)
    }

    public var budgetDisplay: String? {
        guard let budgetMicroUsd else { return nil }
        return String(format: "$%.4f", Double(budgetMicroUsd) / 1_000_000)
    }

    public var iterationsDisplay: String? {
        guard let iterationsUsed else { return nil }
        if let maxIterations { return "\(iterationsUsed)/\(maxIterations)" }
        return "\(iterationsUsed)"
    }
}

/// A small, Sendable projection of one daemon tool call. It intentionally stores only text;
/// daemon payloads are untrusted and must never be interpreted as markup or executable input.
public struct ToolActivity: Equatable, Codable, Sendable, Identifiable {
    public let id: String
    public var name: String
    public var input: String
    public var output: String
    public var status: ToolActivityStatus
    /// Present only for `spawn_subagent` (and, once wired, MCP) activities.
    /// `nil` for every other tool — this never gates whether the activity
    /// renders, only whether the extra progress/budget row does.
    public var subagentProgress: SubagentProgress?

    public init(
        id: String,
        name: String,
        input: String = "",
        output: String = "",
        status: ToolActivityStatus = .running,
        subagentProgress: SubagentProgress? = nil
    ) {
        self.id = id
        self.name = name
        self.input = input
        self.output = output
        self.status = status
        self.subagentProgress = subagentProgress
    }

    public mutating func apply(_ transition: ToolActivityTransition) {
        switch transition {
        case .started(let name, let input):
            self.name = name
            self.input = input
            self.output = ""
            self.status = .running
        case .succeeded(let output):
            self.output = output
            self.status = .succeeded
        case .failed(let message, let output):
            self.output = output
            self.status = .failed(message)
        case .progressed(let progress):
            self.subagentProgress = progress
        }
    }

    /// True for the tool names this activity should never be filtered out
    /// for (product atom 15): local subagent spawning and its MCP siblings
    /// must always reach the transcript like any other tool call.
    public var isSubagentOrMcp: Bool {
        Self.subagentAndMcpToolNames.contains(name)
    }

    public static let subagentAndMcpToolNames: Set<String> = [
        "spawn_subagent",
        "list_mcp_tools",
        "mcp_call",
        "list_mcp_resources",
        "read_mcp_resource"
    ]

    /// Finds a useful one-line input summary without assuming a particular tool schema.
    public var inputSummary: String? {
        guard !input.isEmpty else { return nil }
        if let data = input.data(using: .utf8),
           let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any] {
            for key in ["command", "file_path", "path", "query", "url"] {
                if let value = object[key] as? String, !value.isEmpty { return value }
            }
        }
        let flat = input.replacingOccurrences(of: "\n", with: " ")
        return flat.isEmpty ? nil : flat
    }
}

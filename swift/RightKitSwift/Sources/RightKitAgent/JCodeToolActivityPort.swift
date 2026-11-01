import Foundation

/*
 Provenance (exact donor revision):
 - 1jehuang/jcode @ 10c26eb12a2e8eb14c505a6dc954e164cb62fe18
 - ios/Sources/Kit/SessionReducer.swift:8-37, 253-285

 The donor's TranscriptEntry.ToolCall shape and tool_start/tool_input/tool_exec/
 tool_done transitions are retained here as an isolated CodeRight projection.
 CodeRight adapters supply daemon events and attach the resulting calls to their
 transcript rows; this file has no dependency on Kit or CodeRight's shared
 transcript/networking types.
 */

/// A point-in-time `spawn_subagent` progress/budget snapshot, mirrored from
/// `Models/ToolActivity.SubagentProgress` for this donor-shaped port. Kept
/// as a separate type (rather than importing the app model) so this file
/// stays free of any dependency on CodeRight's shared transcript types, per
/// the provenance note above.
public struct SubagentProgressPort: Equatable, Codable, Sendable {
    public var role: String
    public var iterationsUsed: Int?
    public var maxIterations: Int?
    public var spentMicroUsd: UInt64?
    public var budgetMicroUsd: UInt64?
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
}

/// Namespaced, donor-shaped primitives for tool activity ports.
public enum ToolInteractionsPort {
    /// Status used while a tool input is still arriving, while it is executing,
    /// or after the daemon reports a terminal result.
    public enum ToolCallStatus: Equatable, Codable, Sendable {
        case streamingInput
        case running
        case succeeded
        case failed(String)

        public var label: String {
            switch self {
            case .streamingInput: return "Preparing"
            case .running: return "Running"
            case .succeeded: return "Succeeded"
            case .failed: return "Failed"
            }
        }

        public var isTerminal: Bool {
            switch self {
            case .streamingInput, .running: return false
            case .succeeded, .failed: return true
            }
        }
    }

    /// One tool call as it appears inside a streamed assistant turn.
    public struct ToolCall: Equatable, Codable, Sendable, Identifiable {
        public var id: String
        public var name: String
        public var input: String
        public var output: String
        public var status: ToolCallStatus
        /// Interim `spawn_subagent` progress/budget snapshot. Populated by
        /// `.progress` lifecycle events; `nil` for every non-subagent tool
        /// and for subagent calls before the daemon has reported anything.
        public var subagentProgress: SubagentProgressPort?

        public init(
            id: String,
            name: String,
            input: String = "",
            output: String = "",
            status: ToolCallStatus = .streamingInput,
            subagentProgress: SubagentProgressPort? = nil
        ) {
            self.id = id
            self.name = name
            self.input = input
            self.output = output
            self.status = status
            self.subagentProgress = subagentProgress
        }

        /// Tool names that must always surface here — local sub-agent spawns
        /// and the MCP quartet were previously filtered out of the transcript
        /// entirely (product atom 15/38); this port applies no such filter,
        /// and this flag documents that contract for callers that build any
        /// filtering of their own on top.
        public static let alwaysSurfacedToolNames: Set<String> = [
            "spawn_subagent",
            "list_mcp_tools",
            "mcp_call",
            "list_mcp_resources",
            "read_mcp_resource"
        ]

        /// Finds the same useful one-line summary used by the donor card. Tool
        /// arguments remain inert text; no value is executed or interpreted.
        public var inputSummary: String? {
            guard !input.isEmpty else { return nil }
            if let data = input.data(using: .utf8),
               let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any] {
                for key in ["command", "file_path", "path", "query", "url"] {
                    if let value = object[key] as? String, !value.isEmpty {
                        return value
                    }
                }
            }
            let flat = input.replacingOccurrences(of: "\n", with: " ")
            return flat.isEmpty ? nil : flat
        }
    }

    /// Donor-shaped event subset. `input` intentionally has no id because the
    /// jcode wire event appends each delta to the latest call in the turn.
    public enum ToolLifecycleEvent: Equatable, Sendable {
        case start(id: String, name: String)
        case input(delta: String)
        case execute(id: String, name: String)
        case done(id: String, name: String, output: String, error: String?)
        /// Interim `spawn_subagent` progress/budget update. Never terminal —
        /// the call's status is unaffected; only `subagentProgress` refreshes.
        case progress(id: String, name: String, progress: SubagentProgressPort)
    }

    /// Pure lifecycle projection for one transcript turn.
    ///
    /// This preserves jcode's recovery behavior: execution and completion can
    /// create a call when a client joined after `tool_start`, and input deltas
    /// target the latest call because the donor wire event carries no id.
    public struct ToolLifecycleState: Equatable, Sendable {
        public private(set) var calls: [ToolCall]

        public init(calls: [ToolCall] = []) {
            self.calls = calls
        }

        public mutating func apply(_ event: ToolLifecycleEvent) {
            switch event {
            case .start(let id, let name):
                calls.append(ToolCall(id: id, name: name))

            case .input(let delta):
                guard !calls.isEmpty else { return }
                calls[calls.count - 1].input += delta

            case .execute(let id, let name):
                if let index = calls.firstIndex(where: { $0.id == id }) {
                    calls[index].status = .running
                } else {
                    calls.append(ToolCall(id: id, name: name, status: .running))
                }

            case .done(let id, let name, let output, let error):
                let status = error.map(ToolCallStatus.failed) ?? .succeeded
                if let index = calls.firstIndex(where: { $0.id == id }) {
                    calls[index].output = output
                    calls[index].status = status
                } else {
                    calls.append(
                        ToolCall(id: id, name: name, output: output, status: status)
                    )
                }

            case .progress(let id, let name, let progress):
                if let index = calls.firstIndex(where: { $0.id == id }) {
                    calls[index].subagentProgress = progress
                } else {
                    calls.append(
                        ToolCall(id: id, name: name, status: .running, subagentProgress: progress)
                    )
                }
            }
        }

        public mutating func reset() {
            calls.removeAll(keepingCapacity: false)
        }
    }
}

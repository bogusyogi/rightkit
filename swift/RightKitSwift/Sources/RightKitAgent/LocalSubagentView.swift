#if os(iOS)
import SwiftUI

/// Renders one local sub-agent or MCP tool activity in the transcript
/// (product atom 15/38). Previously this class of tool call was filtered
/// out of the transcript entirely; this view — and `ToolActivity`'s refusal
/// to distinguish `spawn_subagent`/MCP calls from any other tool — is what
/// makes that spawning and its budget/progress visible again.
///
/// Nothing here calls out to the daemon or reads a credential: it is a pure
/// projection of a `ToolActivity` value supplied by the caller.
@MainActor
struct LocalSubagentView: View {
    let activity: ToolActivity

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                Image(systemName: symbolName)
                    .foregroundStyle(iconColor)
                Text(title)
                    .font(.subheadline.weight(.semibold))
                Spacer()
                statusBadge
            }

            if let summary = activity.inputSummary {
                Text(summary)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }

            if let progress = activity.subagentProgress {
                progressRow(progress)
            }

            if case .failed(let message) = activity.status, !message.isEmpty {
                Text(message)
                    .font(.caption)
                    .foregroundStyle(.red)
            }
        }
        .padding(10)
        .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 10))
        .accessibilityElement(children: .combine)
    }

    @ViewBuilder
    private func progressRow(_ progress: SubagentProgress) -> some View {
        HStack(spacing: 12) {
            Label(progress.role, systemImage: "person.badge.clock")
            if let iterations = progress.iterationsDisplay {
                Label(iterations, systemImage: "arrow.triangle.2.circlepath")
            }
            if let spent = progress.spentDisplay {
                let budgetSuffix = progress.budgetDisplay.map { " / \($0)" } ?? ""
                Label(spent + budgetSuffix, systemImage: "dollarsign.circle")
            } else if let budget = progress.budgetDisplay {
                Label("budget \(budget)", systemImage: "dollarsign.circle")
            }
        }
        .font(.caption)
        .foregroundStyle(.secondary)
        .labelStyle(.titleAndIcon)
    }

    private var statusBadge: some View {
        HStack(spacing: 4) {
            statusIcon
            Text(activity.status.label)
        }
        .font(.caption.weight(.medium))
        .foregroundStyle(statusColor)
    }

    @ViewBuilder
    private var statusIcon: some View {
        switch activity.status {
        case .running:
            ProgressView().controlSize(.mini)
        case .succeeded:
            Image(systemName: "checkmark.circle.fill")
        case .failed:
            Image(systemName: "xmark.octagon.fill")
        }
    }

    private var statusColor: Color {
        switch activity.status {
        case .running: return .secondary
        case .succeeded: return .green
        case .failed: return .red
        }
    }

    private var symbolName: String {
        activity.name == "spawn_subagent" ? "person.2.wave.2" : "puzzlepiece.extension"
    }

    private var iconColor: Color {
        activity.name == "spawn_subagent" ? .accentColor : .secondary
    }

    private var title: String {
        switch activity.name {
        case "spawn_subagent":
            return activity.subagentProgress.map { "Sub-agent · \($0.role)" } ?? "Sub-agent"
        case "list_mcp_tools": return "MCP · list tools"
        case "mcp_call": return "MCP · call"
        case "list_mcp_resources": return "MCP · list resources"
        case "read_mcp_resource": return "MCP · read resource"
        default: return activity.name
        }
    }
}

/// A scrollable feed of every subagent/MCP activity in a session, in the
/// order they were reported. Intended to sit alongside (not replace) the
/// main transcript renderer, so a session with heavy delegation has a place
/// to review sub-agent budgets without scrolling the whole conversation.
@MainActor
struct LocalSubagentFeedView: View {
    let activities: [ToolActivity]

    private var relevant: [ToolActivity] {
        activities.filter(\.isSubagentOrMcp)
    }

    var body: some View {
        Group {
            if relevant.isEmpty {
                ContentUnavailableView(
                    "No sub-agent activity",
                    systemImage: "person.2.wave.2",
                    description: Text("Sub-agent spawns and MCP calls for this session will appear here.")
                )
            } else {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 10) {
                        ForEach(relevant) { activity in
                            LocalSubagentView(activity: activity)
                        }
                    }
                    .padding()
                }
            }
        }
        .navigationTitle("Sub-agents")
    }
}
#endif

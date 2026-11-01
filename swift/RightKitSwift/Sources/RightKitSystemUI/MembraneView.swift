import RightKitHub
import SwiftUI

// Membrane's view: port of the markup half of `hub/src/views/Membrane.tsx`. The page scrolls as a
// whole: status with Restart Membrane (confirmed), graph freshness, then recent memory, context
// delivered and logs, which load only when asked.

struct MembraneView: View {
    @State private var model: MembraneModel
    private let initialLoad: Bool

    init(model: MembraneModel, initialLoad: Bool) {
        _model = State(initialValue: model)
        self.initialLoad = initialLoad
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                ACText(
                    "Membrane keeps per-project repository memory and a freshness map for Pulse. Status refreshes every 5 seconds while this page is open.",
                    .caption,
                    ink: .secondary
                )
                statusCard
                freshnessCard
                memoryCard
                contextCard
                logsCard
            }
            .padding(.horizontal, 18)
            .padding(.vertical, 14)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .task { await model.run(initialLoad: initialLoad) }
    }

    // MARK: Status

    private var statusCard: some View {
        HubCard(padding: 12) {
            VStack(alignment: .leading, spacing: 8) {
                ACText("Status", .label)
                if let error = model.statusError {
                    ACNoticeText("Status could not be loaded: \(error)", tone: .bad)
                }
                if model.status == nil && model.statusError == nil {
                    ACText("Loading status…", .caption, ink: .secondary)
                }
                restartControls
                if let error = model.restartError {
                    ACNoticeText("Restart failed: \(error)", tone: .bad)
                }
                if let status = model.status {
                    statusRows(status)
                }
            }
        }
    }

    @ViewBuilder private var restartControls: some View {
        HStack(spacing: 8) {
            if model.confirmRestart {
                ACText("Restart Membrane? Its current process stops and starts again.", .caption, ink: .secondary, lines: 2)
                ACButton(model.restarting ? "Restarting…" : "Confirm restart", enabled: !model.restarting) {
                    Task { await model.restart() }
                }
                ACButton("Cancel", enabled: !model.restarting) {
                    model.confirmRestart = false
                }
            } else {
                ACButton("Restart Membrane") {
                    model.confirmRestart = true
                }
            }
        }
    }

    private func statusRows(_ status: MembraneStatus) -> some View {
        let process = "\(status.running ? "Running" : "Not running"), pid \(membraneNumberText(status.pid)), up \(membraneUptime(status.uptimeSecs))"
        let health = status.healthError.map { "\(status.health): \($0)" } ?? status.health
        return VStack(alignment: .leading, spacing: 6) {
            kv("State") {
                ACBadge(status.state, tone: stateTone(status.state))
            }
            kv("Process") {
                ACText(process, .caption)
            }
            kv("Model") {
                ACText(membraneModelText(status.model), .caption)
            }
            kv("Availability") {
                if let availability = status.availability {
                    HStack(alignment: .firstTextBaseline, spacing: 6) {
                        ACBadge(availabilityLabel(availability.state), tone: availability.state == "ok" ? .ok : .warn)
                        ACText(availability.reason, .caption, ink: availability.state == "fault" ? .bad : .primary)
                    }
                } else {
                    ACText("None", .caption)
                }
            }
            kv("Health") {
                ACText(health, .caption)
            }
            kv("Last error") {
                ACText(membraneString(status.lastError), .caption)
            }
        }
    }

    // MARK: Freshness

    private var freshnessCard: some View {
        HubCard(padding: 12) {
            VStack(alignment: .leading, spacing: 8) {
                ACText("Freshness", .label)
                if let error = model.freshnessError {
                    ACNoticeText("Freshness could not be loaded: \(error)", tone: .bad)
                }
                if let freshness = model.freshness {
                    freshnessRows(freshness)
                }
            }
        }
    }

    private func freshnessRows(_ freshness: MembraneFreshness) -> some View {
        let reason = freshness.reason ?? ""
        return VStack(alignment: .leading, spacing: 8) {
            VStack(alignment: .leading, spacing: 6) {
                kv("Watcher") {
                    ACText(membraneText(freshness.watcherState), .caption)
                }
                kv("Coverage") {
                    ACText(membraneText(freshness.coverage), .caption)
                }
                kv("Freshness") {
                    ACText(membraneText(freshness.freshness), .caption)
                }
            }
            if !reason.isEmpty {
                ACText(reason, .caption, ink: .secondary)
            }
            if freshness.projects.isEmpty && reason.isEmpty {
                ACText("No projects indexed yet.", .caption, ink: .secondary)
            }
            ForEach(Array(freshness.projects.enumerated()), id: \.offset) { _, project in
                VStack(alignment: .leading, spacing: 2) {
                    ACText(membraneText(project.root), .caption)
                    ACText(projectDetail(project), .caption, ink: .secondary)
                    ACText("last \(membraneText(project.lastEvent))", .caption, ink: .secondary)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    // MARK: Memory

    private var memoryCard: some View {
        HubCard(padding: 12) {
            VStack(alignment: .leading, spacing: 8) {
                HStack(spacing: 8) {
                    ACText("Recent memory", .label)
                    Spacer()
                    ACButton(memoryButton, enabled: !model.memoryLoading) {
                        Task { await model.loadMemory() }
                    }
                }
                if let error = model.memoryError {
                    ACNoticeText("Memory could not be loaded: \(error)", tone: .bad)
                }
                if let memory = model.memory {
                    memoryRows(memory)
                } else if model.memoryError == nil {
                    ACText("Memory is read on demand. Secrets are redacted.", .caption, ink: .secondary)
                }
            }
        }
    }

    private var memoryButton: String {
        if model.memoryLoading { return "Loading…" }
        return model.memory == nil ? "Load memory" : "Refresh memory"
    }

    @ViewBuilder private func memoryRows(_ memory: MembraneMemory) -> some View {
        if !memory.available {
            ACText(memory.reason ?? "Memory is not available.", .caption, ink: .secondary)
        } else if memory.notes.isEmpty {
            ACText("No memory notes yet.", .caption, ink: .secondary)
        } else {
            ForEach(Array(memory.notes.enumerated()), id: \.offset) { _, note in
                VStack(alignment: .leading, spacing: 2) {
                    ACText(note.title ?? "Untitled note", .body)
                    ACText(noteDetail(note), .caption, ink: .secondary)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    // MARK: Context

    private var contextCard: some View {
        HubCard(padding: 12) {
            VStack(alignment: .leading, spacing: 8) {
                HStack(spacing: 8) {
                    ACText("Context delivered", .label)
                    Spacer()
                    ACButton(contextButton, enabled: !model.contextLoading) {
                        Task { await model.loadContext() }
                    }
                }
                if let error = model.contextError {
                    ACNoticeText("Context could not be loaded: \(error)", tone: .bad)
                }
                if let context = model.context {
                    contextRows(context)
                } else if model.contextError == nil {
                    ACText("Context receipts are read on demand. Only counts and sources are shown.", .caption, ink: .secondary)
                }
            }
        }
    }

    private var contextButton: String {
        if model.contextLoading { return "Loading…" }
        return model.context == nil ? "Load context" : "Refresh context"
    }

    @ViewBuilder private func contextRows(_ context: MembraneContext) -> some View {
        if !context.available {
            ACText(context.reason ?? "Context receipts are not available.", .caption, ink: .secondary)
        } else {
            VStack(alignment: .leading, spacing: 6) {
                kv("Receipts") {
                    ACText("\(context.receipts) across \(context.sessions) session\(context.sessions == 1 ? "" : "s")", .caption)
                }
                kv("By state") {
                    ACText(membraneCounts(context.byState), .caption)
                }
                kv("By source") {
                    ACText(membraneCounts(context.byEvent), .caption)
                }
                kv("Last") {
                    ACText(lastReceipt(context.last), .caption)
                }
            }
        }
    }

    // MARK: Logs

    private var logsCard: some View {
        HubCard(padding: 12) {
            VStack(alignment: .leading, spacing: 8) {
                HStack(spacing: 8) {
                    ACText("Recent logs", .label)
                    Spacer()
                    ACButton(logsButton, enabled: !model.logsLoading) {
                        Task { await model.loadLogs() }
                    }
                }
                if let error = model.logsError {
                    ACNoticeText("Logs could not be loaded: \(error)", tone: .bad)
                }
                if let logs = model.logs {
                    logsBody(logs)
                } else if model.logsError == nil {
                    ACText("Logs are read on demand.", .caption, ink: .secondary)
                }
            }
        }
    }

    private var logsButton: String {
        if model.logsLoading { return "Loading…" }
        return model.logs == nil ? "Load logs" : "Refresh logs"
    }

    @ViewBuilder private func logsBody(_ logs: MembraneLogs) -> some View {
        if !logs.available {
            ACText("No Membrane log yet.", .caption, ink: .secondary)
        } else {
            let plural = logs.count == 1 ? "" : "s"
            let cut = logs.truncated ? ", showing the last \(MembraneModel.logLines)" : ""
            ACText("\(logs.count) line\(plural)\(cut). Secrets are redacted.", .caption, ink: .secondary)
            ACNotice {
                ScrollView {
                    ACText(logs.lines.isEmpty ? "The log is empty." : logs.lines.joined(separator: "\n"), .small, ink: .secondary)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .frame(maxHeight: 320)
            }
        }
    }

    // MARK: Helpers

    /// A label beside its value, the page's two-column key list.
    private func kv<Value: View>(_ label: String, @ViewBuilder _ value: () -> Value) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 12) {
            ACText(label, .caption, ink: .secondary)
                .frame(width: 110, alignment: .leading)
            value()
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private func stateTone(_ state: String) -> HubTone {
        if state == "running" { return .ok }
        if state == "starting" || state == "stopped" || state == "not installed" || state.hasPrefix("latched") {
            return .warn
        }
        return .neutral
    }

    private func availabilityLabel(_ state: String) -> String {
        switch state {
        case "ok": return "OK"
        case "temporary": return "Temporary"
        case "fault": return "Fault"
        default: return state
        }
    }

    private func projectDetail(_ project: MembraneProject) -> String {
        var complete = false
        if case .bool(true)? = project.complete { complete = true }
        return "generation \(membraneText(project.generationId)), \(complete ? "complete" : "incomplete"), event \(membraneText(project.eventSequence))"
    }

    private func noteDetail(_ note: MembraneNote) -> String {
        let when = note.ts.map { ", \(membraneNumber($0))" } ?? ""
        return "\(membraneString(note.repository))\(when), id \(membraneText(note.id))"
    }

    private func lastReceipt(_ last: MembraneContextLast?) -> String {
        guard let last else { return "None" }
        return "\(membraneText(last.event)) (\(membraneText(last.harness))), \(membraneText(last.state)), \(membraneNumberText(last.ts))"
    }
}

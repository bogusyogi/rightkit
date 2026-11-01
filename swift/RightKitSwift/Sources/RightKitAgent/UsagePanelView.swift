import Foundation
import Observation
import SwiftUI

/// One recorded usage snapshot for a session/provider pair, captured whenever
/// `UsageFooterView` observes a `UsageState` change. This is the mobile
/// equivalent of the desktop's per-session cost ledger (APP-13).
///
/// Gap: the daemon protocol event (`applyUsage` in
/// `Views/SessionViewModelEvents.swift`) does not currently carry a provider
/// label, and neither `UsageFooterView`'s existing call site
/// (`Views/TranscriptView.swift`) nor `LocalSessionView.swift` are in this
/// lane's file allowlist, so this ledger cannot yet be fed a real provider
/// name or a stable historical session id from the actual event stream — it
/// records what is reachable today (the live `UsageState` plus whatever
/// caller-supplied labels are passed in, defaulting to "Current session" /
/// "Unknown provider"). Wiring real per-provider, cross-session persistence
/// needs `sessionId`/`providerLabel` threaded from `SessionViewModelEvents`
/// through `LocalSessionView` into `UsageFooterView` — out of scope here.
public struct UsageSnapshot: Identifiable, Equatable, Sendable {
    public let id: String
    public let sessionId: String
    public let sessionTitle: String
    public let providerLabel: String
    public let usage: UsageState
    public let recordedAt: Date

    public init(
        sessionId: String,
        sessionTitle: String,
        providerLabel: String,
        usage: UsageState,
        recordedAt: Date = Date()
    ) {
        self.id = "\(sessionId)-\(recordedAt.timeIntervalSinceReferenceDate)"
        self.sessionId = sessionId
        self.sessionTitle = sessionTitle
        self.providerLabel = providerLabel
        self.usage = usage
        self.recordedAt = recordedAt
    }

    public var totalTokens: Int {
        usage.promptTokens + usage.completionTokens + usage.cacheReadTokens + usage.cacheCreationTokens
    }
}

/// In-memory, process-lifetime ledger of usage snapshots. Not persisted to
/// disk: the goal is "show real numbers for what happened this run" rather
/// than a durable spend history, since the latter needs the daemon/event-side
/// wiring described in `UsageSnapshot`'s doc comment.
@MainActor
@Observable
public final class UsageLedger {
    public static let shared = UsageLedger()

    public private(set) var snapshots: [UsageSnapshot] = []

    private var lastRecorded: [String: UsageState] = [:]

    init() {}

    /// Records the latest usage for a session/provider only when it actually
    /// changed, so repeated `onChange` firings from a footer view don't spam
    /// the ledger with duplicate rows.
    public func record(sessionId: String, sessionTitle: String, providerLabel: String, usage: UsageState) {
        guard usage != .empty else { return }
        guard lastRecorded[sessionId] != usage else { return }
        lastRecorded[sessionId] = usage
        snapshots.append(
            UsageSnapshot(
                sessionId: sessionId, sessionTitle: sessionTitle, providerLabel: providerLabel, usage: usage
            )
        )
    }

    /// Latest snapshot per session id, most-recently-updated first.
    public var perSession: [UsageSnapshot] {
        var latestBySession: [String: UsageSnapshot] = [:]
        for snapshot in snapshots {
            latestBySession[snapshot.sessionId] = snapshot
        }
        return latestBySession.values.sorted { $0.recordedAt > $1.recordedAt }
    }

    /// Latest-per-session snapshots summed by provider label.
    public var perProvider: [(provider: String, totalCostUsd: Double, totalTokens: Int)] {
        var totals: [String: (cost: Double, tokens: Int)] = [:]
        for snapshot in perSession {
            var entry = totals[snapshot.providerLabel] ?? (cost: 0, tokens: 0)
            entry.cost += snapshot.usage.costUsd
            entry.tokens += snapshot.totalTokens
            totals[snapshot.providerLabel] = entry
        }
        return totals
            .map { (provider: $0.key, totalCostUsd: $0.value.cost, totalTokens: $0.value.tokens) }
            .sorted { $0.totalCostUsd > $1.totalCostUsd }
    }

    public var grandTotalCostUsd: Double {
        perSession.reduce(0) { $0 + $1.usage.costUsd }
    }

    public var grandTotalTokens: Int {
        perSession.reduce(0) { $0 + $1.totalTokens }
    }

    func reset() {
        snapshots.removeAll()
        lastRecorded.removeAll()
    }
}

/// Usage & cost dashboard (APP-13): per-provider totals, per-session
/// breakdown, and a simple bar chart — hand-drawn shapes, no external chart
/// dependency, per the lane brief.
struct UsagePanelView: View {
    let ledger: UsageLedger

    var body: some View {
        List {
            Section("Total this run") {
                HStack {
                    VStack(alignment: .leading) {
                        Text("Spend").font(.caption).foregroundStyle(Theme.muted)
                        Text(String(format: "$%.4f", ledger.grandTotalCostUsd))
                            .font(.title3.monospacedDigit())
                    }
                    Spacer()
                    VStack(alignment: .trailing) {
                        Text("Tokens").font(.caption).foregroundStyle(Theme.muted)
                        Text("\(ledger.grandTotalTokens)")
                            .font(.title3.monospacedDigit())
                    }
                }
                .padding(.vertical, 4)
            }

            if !ledger.perProvider.isEmpty {
                Section("By provider") {
                    UsageChartView(
                        bars: ledger.perProvider.map { .init(label: $0.provider, value: $0.totalCostUsd) }
                    )
                    .frame(height: 120)
                    .padding(.vertical, 4)
                    ForEach(ledger.perProvider, id: \.provider) { entry in
                        HStack {
                            Text(entry.provider).foregroundStyle(Theme.text)
                            Spacer()
                            Text(String(format: "$%.4f", entry.totalCostUsd))
                                .foregroundStyle(Theme.muted)
                                .monospacedDigit()
                        }
                    }
                }
            }

            if ledger.perSession.isEmpty {
                Section {
                    Text("No usage recorded yet in this run.")
                        .foregroundStyle(Theme.muted)
                }
            } else {
                Section("By session") {
                    ForEach(ledger.perSession) { snapshot in
                        VStack(alignment: .leading, spacing: 4) {
                            HStack {
                                Text(snapshot.sessionTitle).foregroundStyle(Theme.text)
                                Spacer()
                                Text(String(format: "$%.4f", snapshot.usage.costUsd))
                                    .foregroundStyle(Theme.muted)
                                    .monospacedDigit()
                            }
                            Text("\(snapshot.providerLabel) · \(snapshot.totalTokens) tok")
                                .font(.caption)
                                .foregroundStyle(Theme.muted)
                        }
                    }
                }
            }
        }
        .scrollContentBackground(.hidden)
        .background(Theme.background)
        .navigationTitle("Usage")
        .navigationBarTitleDisplayMode(.inline)
    }
}

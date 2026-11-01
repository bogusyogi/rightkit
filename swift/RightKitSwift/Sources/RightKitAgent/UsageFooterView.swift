import SwiftUI

/// Port of `../coderight-web/src/components/UsagePanel.tsx`, expanded into a
/// tappable entry point for the full usage & cost dashboard (APP-13,
/// `Usage/UsagePanelView.swift`).
///
/// `sessionId`/`sessionTitle`/`providerLabel` are optional with defaults so
/// the existing call site (`Views/TranscriptView.swift`,
/// `UsageFooterView(usage: vm.usage)`, outside this lane's allowlist) keeps
/// compiling unchanged. Without real values from the event stream, snapshots
/// are recorded under a single "Current session" / "Unknown provider"
/// bucket — see the gap note on `UsageSnapshot`.
struct UsageFooterView: View {
    let usage: UsageState
    var sessionId: String = "current"
    var sessionTitle: String = "Current session"
    var providerLabel: String = "Unknown provider"

    @State private var showPanel = false

    private var totalTokens: Int {
        usage.promptTokens + usage.completionTokens + usage.cacheReadTokens + usage.cacheCreationTokens
    }

    var body: some View {
        Button {
            showPanel = true
        } label: {
            HStack(spacing: 12) {
                Text(String(format: "$%.4f", usage.costUsd))
                Text("\(totalTokens) tok")
                    .opacity(0.65)
                Spacer()
                Image(systemName: "chart.bar")
                    .opacity(0.65)
            }
            .font(.caption2)
            .foregroundStyle(Theme.muted)
            .padding(.horizontal, 12)
            .padding(.vertical, 6)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Theme.surface)
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("usage-footer")
        .onChange(of: usage) { _, newValue in
            UsageLedger.shared.record(
                sessionId: sessionId, sessionTitle: sessionTitle, providerLabel: providerLabel, usage: newValue
            )
        }
        .onAppear {
            UsageLedger.shared.record(
                sessionId: sessionId, sessionTitle: sessionTitle, providerLabel: providerLabel, usage: usage
            )
        }
        .sheet(isPresented: $showPanel) {
            NavigationStack {
                UsagePanelView(ledger: UsageLedger.shared)
                    .toolbar {
                        ToolbarItem(placement: .cancellationAction) {
                            Button("Close") { showPanel = false }
                        }
                    }
            }
        }
    }
}

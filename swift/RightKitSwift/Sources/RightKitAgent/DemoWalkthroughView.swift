import SwiftUI

/// No-network scripted walkthrough of pairing → session → approval (APP-09).
///
/// Entirely fixture-backed (`DemoFixtures`) — no `DaemonClienting`, `LocalRuntime`, or
/// networking type is touched. Steps advance with a "Next" button so it doubles as an
/// onboarding tour and a support/sales aid that works with the device in airplane mode.
///
/// Integration note (left for the owning lane/orchestrator): this view is not wired into
/// onboarding or Settings. The natural hook is a "View Demo" row in
/// `apps/coderight-ios/CodeRight/LocalRuntime/HomeView.swift`, alongside the existing
/// `NavigationLink("Providers & workspaces") { LocalSetupView() }` at line 123 — add a sibling
/// `NavigationLink("View Demo") { DemoWalkthroughView() }` there, or present it as a sheet
/// from the app's first-run onboarding screen if one exists.
public struct DemoWalkthroughView: View {
    private enum Step: Int, CaseIterable {
        case pairing, session, approval, done

        var title: String {
            switch self {
            case .pairing: return "Pair a Computer"
            case .session: return "Start a Session"
            case .approval: return "Review & Approve"
            case .done: return "Done"
            }
        }
    }

    @State private var step: Step = .pairing

    public init() {}

    public var body: some View {
        VStack(spacing: 20) {
            Text(step.title)
                .font(.title2.weight(.semibold))
                .foregroundStyle(Theme.text)

            content

            Spacer()

            HStack {
                if step != .pairing {
                    Button("Back") { back() }
                        .foregroundStyle(Theme.muted)
                }
                Spacer()
                Button(step == .done ? "Restart" : "Next") { advance() }
                    .buttonStyle(.borderedProminent)
                    .tint(Theme.accent)
                    .foregroundStyle(Theme.onAccent)
            }
        }
        .padding()
        .background(Theme.background)
        .navigationTitle("Demo")
    }

    @ViewBuilder
    private var content: some View {
        switch step {
        case .pairing:
            VStack(alignment: .leading, spacing: 4) {
                Text(DemoFixtures.daemon.label)
                    .font(.body.weight(.medium))
                    .foregroundStyle(Theme.text)
                Text(DemoFixtures.daemon.origin)
                    .font(.caption)
                    .foregroundStyle(Theme.muted)
            }
            .padding()
            .background(Theme.surface)
            .clipShape(RoundedRectangle(cornerRadius: 12))
        case .session:
            List(DemoFixtures.transcript.filter { $0.role != .approval }) { message in
                Text(message.text).foregroundStyle(Theme.text)
            }
            .frame(maxHeight: 220)
            .listStyle(.plain)
        case .approval:
            if let approval = DemoFixtures.transcript.first(where: { $0.role == .approval }) {
                VStack(alignment: .leading, spacing: 8) {
                    Text(approval.text).foregroundStyle(Theme.text)
                    if let diff = approval.diff {
                        Text(diff)
                            .font(.system(.caption, design: .monospaced))
                            .foregroundStyle(Theme.muted)
                    }
                }
                .padding()
                .background(Theme.surface)
                .clipShape(RoundedRectangle(cornerRadius: 12))
            }
        case .done:
            Text("That's the full loop: pair a computer, start a session, review and approve a change — all from your phone.")
                .foregroundStyle(Theme.muted)
        }
    }

    private func advance() {
        let all = Step.allCases
        guard let index = all.firstIndex(of: step) else { return }
        step = all[index] == .done ? .pairing : all[min(index + 1, all.count - 1)]
    }

    private func back() {
        let all = Step.allCases
        guard let index = all.firstIndex(of: step), index > 0 else { return }
        step = all[index - 1]
    }
}

import SwiftUI

/// Blocking response card shared by tool approvals, questions, and plans.
/// The local state locks controls immediately; the parent may replace it with a resolved or
/// failed state when the daemon emits its response.
struct ApprovalInteractionCard: View {
    let state: ApprovalInteractionState
    let onSubmit: (ApprovalInteractionAction) -> Void
    @State private var localState: ApprovalInteractionState
    @State private var answer = ""

    init(
        state: ApprovalInteractionState,
        onSubmit: @escaping (ApprovalInteractionAction) -> Void
    ) {
        self.state = state
        self.onSubmit = onSubmit
        _localState = State(initialValue: state)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 9) {
            HStack(spacing: 7) {
                Image(systemName: iconName)
                    .foregroundStyle(iconColor)
                    .accessibilityHidden(true)
                Text(state.title)
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(Theme.text)
                if let risk = state.risk, !risk.isEmpty {
                    Text(risk)
                        .font(.caption.weight(.medium))
                        .foregroundStyle(Theme.muted)
                }
                Spacer(minLength: 4)
                Text(localState.status.label)
                    .font(.caption.weight(.medium))
                    .foregroundStyle(statusColor)
            }
            .accessibilityElement(children: .combine)
            .accessibilityLabel("\(state.kind.label): \(state.title)")
            .accessibilityValue(localState.status.label)

            if let effectTarget = state.effectTarget, !effectTarget.isEmpty {
                // ENG-28: show the exact effect (literal path/command) this
                // approval authorizes, distinct from any freeform detail, so
                // approving cannot be misread as authorizing a different target.
                Label {
                    Text(effectTarget)
                        .font(.system(.caption, design: .monospaced))
                        .foregroundStyle(Theme.text)
                        .textSelection(.enabled)
                } icon: {
                    Image(systemName: "target")
                        .foregroundStyle(Theme.accent2)
                }
                .accessibilityElement(children: .combine)
                .accessibilityLabel("Exact target: \(effectTarget)")
            }

            if !state.detail.isEmpty {
                Text(state.detail)
                    .font(.system(.caption, design: .monospaced))
                    .foregroundStyle(Theme.text)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
            }

            if !state.questions.isEmpty {
                VStack(alignment: .leading, spacing: 5) {
                    ForEach(state.questions) { question in
                        VStack(alignment: .leading, spacing: 2) {
                            if let header = question.header, !header.isEmpty {
                                Text(header)
                                    .font(.caption.weight(.semibold))
                                    .foregroundStyle(Theme.muted)
                            }
                            Text(question.question)
                                .font(.subheadline)
                                .foregroundStyle(Theme.text)
                        }
                    }
                }
            }

            if case .failed(let message) = localState.status {
                Label(message, systemImage: "exclamationmark.triangle.fill")
                    .font(.caption)
                    .foregroundStyle(Theme.danger)
                    .accessibilityLabel("Response failed: \(message)")
            }
            if localState.status == .stale {
                Label("This request is no longer active", systemImage: "clock.badge.xmark")
                    .font(.caption)
                    .foregroundStyle(Theme.muted)
            }

            controls
        }
        .padding(10)
        .background(Theme.roleTint(.approval), in: RoundedRectangle(cornerRadius: 10, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .stroke(Theme.accent2.opacity(0.55), lineWidth: 1)
        )
        .onChange(of: state) { _, updated in
            localState = updated
            if updated.status != .pending { answer = "" }
        }
    }

    @ViewBuilder
    private var controls: some View {
        switch state.kind {
        case .question:
            HStack(spacing: 8) {
                TextField("Your answer", text: $answer, axis: .vertical)
                    .textFieldStyle(.roundedBorder)
                    .lineLimit(1 ... 4)
                    .disabled(!localState.canSubmit)
                    .accessibilityLabel("Answer")
                Button("Send") { submit(.answer(answer)) }
                    .buttonStyle(.borderedProminent)
                    .tint(Theme.accent)
                    .disabled(!localState.canSubmit || answer.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    .accessibilityLabel("Send answer")
            }
        case .toolApproval:
            HStack(spacing: 8) {
                Button("Approve") { submit(.approve) }
                    .buttonStyle(.borderedProminent)
                    .tint(Theme.accent2)
                    .accessibilityLabel("Approve tool")
                Button("Skip", role: .cancel) { submit(.decline) }
                    .buttonStyle(.bordered)
                    .accessibilityLabel("Skip tool")
            }
            .disabled(!localState.canSubmit)
        case .plan:
            HStack(spacing: 8) {
                Button("Approve") { submit(.approve) }
                    .buttonStyle(.borderedProminent)
                    .tint(Theme.accent2)
                    .accessibilityLabel("Approve plan")
                Button("Reject", role: .destructive) { submit(.decline) }
                    .buttonStyle(.bordered)
                    .accessibilityLabel("Reject plan")
            }
            .disabled(!localState.canSubmit)
        }
    }

    private func submit(_ action: ApprovalInteractionAction) {
        guard localState.submit(action) else { return }
        onSubmit(action)
    }

    private var iconName: String {
        switch state.kind {
        case .toolApproval: return "hand.raised.fill"
        case .question: return "questionmark.circle.fill"
        case .plan: return "list.bullet.clipboard.fill"
        }
    }

    private var iconColor: Color {
        localState.status == .stale ? Theme.muted : Theme.accent2
    }

    private var statusColor: Color {
        switch localState.status {
        case .failed: return Theme.danger
        case .resolved(let approved): return approved ? Theme.accent2 : Theme.danger
        case .pending, .submitting, .stale: return Theme.muted
        }
    }
}

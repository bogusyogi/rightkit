import SwiftUI

/*
 Provenance (exact donor revision):
 - mjmkk/opencodex @ 1d34372bb51372b7cd61b716021654c9019dfb94
 - codex-worker-ios/Sources/CodexWorker/Features/ApprovalFeature/ApprovalSheetView.swift:1-122

 The donor's command/file approval presentation, decline-reason field, and
 approve/session-approve/decline/cancel actions are retained. CodeRight adds
 question and plan panels to the same response seam, while Theme and all
 networking remain owned by the host app.
 */

public extension CodeRightToolInteractionsPort {
    /// Blocking response panel for command/file approvals, structured
    /// questions, and plan review. The parent owns transport and feeds result
    /// actions back through `ApprovalReducer`.
    struct ApprovalSheetView: View {
        public let state: ApprovalState
        public let onSubmit: (ApprovalResponseRequest) -> Void

        @State private var localState: ApprovalState
        @State private var answers: [String]

        public init(
            state: ApprovalState,
            onSubmit: @escaping (ApprovalResponseRequest) -> Void
        ) {
            self.state = state
            self.onSubmit = onSubmit
            _localState = State(initialValue: state)
            _answers = State(initialValue: Self.initialAnswers(for: state))
        }

        public var body: some View {
            Group {
                if let interaction = localState.current {
                    panel(for: interaction)
                }
            }
            .onChange(of: state) { _, updated in
                localState = updated
                answers = Self.initialAnswers(for: updated)
            }
        }

        @ViewBuilder
        private func panel(for interaction: PendingInteraction) -> some View {
            VStack(alignment: .leading, spacing: 12) {
                header(for: interaction)

                switch interaction {
                case .approval(let approval):
                    approvalDetails(approval)
                case .question(let question):
                    questionDetails(question)
                case .plan(let plan):
                    planDetails(plan)
                }

                if let errorMessage = localState.errorMessage, !errorMessage.isEmpty {
                    Label(errorMessage, systemImage: "exclamationmark.triangle.fill")
                        .font(.caption)
                        .foregroundStyle(Theme.danger)
                        .fixedSize(horizontal: false, vertical: true)
                        .accessibilityLabel("Response failed: \(errorMessage)")
                }

                controls(for: interaction)
            }
            .padding(14)
            .background(Theme.surface)
            .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: 14, style: .continuous)
                    .stroke(Theme.accent2.opacity(0.55), lineWidth: 1)
            )
            .padding(.horizontal, 12)
            .padding(.bottom, 10)
        }

        private func header(for interaction: PendingInteraction) -> some View {
            HStack(spacing: 8) {
                Image(systemName: iconName(for: interaction))
                    .foregroundStyle(Theme.accent2)
                    .accessibilityHidden(true)
                Text(title(for: interaction))
                    .font(.headline)
                    .foregroundStyle(Theme.text)

                if case .approval(let approval) = interaction {
                    Text(approval.riskLevel.label)
                        .font(.caption.weight(.medium))
                        .foregroundStyle(riskColor(approval.riskLevel))
                        .padding(.horizontal, 8)
                        .padding(.vertical, 4)
                        .background(riskColor(approval.riskLevel).opacity(0.16))
                        .clipShape(Capsule())
                        .accessibilityLabel(approval.riskLevel.label)
                }

                Spacer(minLength: 4)
                Text(localState.status.label)
                    .font(.caption.weight(.medium))
                    .foregroundStyle(statusColor)
                    .multilineTextAlignment(.trailing)
            }
            .accessibilityElement(children: .combine)
            .accessibilityLabel("\(interaction.kind.label): \(title(for: interaction))")
            .accessibilityValue(localState.status.label)
        }

        @ViewBuilder
        private func approvalDetails(_ approval: Approval) -> some View {
            if !approval.formattedCommand.isEmpty {
                Text(approval.formattedCommand)
                    .font(.system(.footnote, design: .monospaced))
                    .foregroundStyle(Theme.text)
                    .padding(10)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(Theme.background)
                    .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
                    .textSelection(.enabled)
                    .accessibilityLabel("Command: \(approval.formattedCommand)")
            }

            if let cwd = approval.cwd?.trimmingCharacters(in: .whitespacesAndNewlines),
               !cwd.isEmpty {
                Text("cwd: \(cwd)")
                    .font(.caption)
                    .foregroundStyle(Theme.muted)
                    .lineLimit(1)
                    .textSelection(.enabled)
            }

            if let reason = approval.reason?.trimmingCharacters(in: .whitespacesAndNewlines),
               !reason.isEmpty {
                Text(reason)
                    .font(.caption)
                    .foregroundStyle(Theme.muted)
                    .fixedSize(horizontal: false, vertical: true)
            }

            if approval.grantRoot == true {
                Label("Requests root access", systemImage: "lock.open.fill")
                    .font(.caption)
                    .foregroundStyle(Theme.danger)
            }

            VStack(alignment: .leading, spacing: 6) {
                Text("Decline reason (optional)")
                    .font(.caption)
                    .foregroundStyle(Theme.muted)
                TextField(
                    "Why should this request be declined?",
                    text: Binding(
                        get: { localState.declineReasonInput },
                        set: { localState.declineReasonInput = $0 }
                    ),
                    axis: .vertical
                )
                .textFieldStyle(.roundedBorder)
                .lineLimit(2 ... 4)
                .disabled(!localState.canSubmit)
                .accessibilityLabel("Decline reason")
            }
        }

        @ViewBuilder
        private func questionDetails(_ request: QuestionRequest) -> some View {
            if request.questions.isEmpty {
                answerField(label: "Answer", index: 0)
            } else {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(Array(request.questions.enumerated()), id: \.element.id) { index, item in
                        VStack(alignment: .leading, spacing: 4) {
                            if let header = item.header, !header.isEmpty {
                                Text(header)
                                    .font(.caption.weight(.semibold))
                                    .foregroundStyle(Theme.muted)
                            }
                            Text(item.question)
                                .font(.subheadline)
                                .foregroundStyle(Theme.text)
                                .fixedSize(horizontal: false, vertical: true)
                            answerField(label: "Answer", index: index)
                        }
                    }
                }
            }
        }

        private func answerField(label: String, index: Int) -> some View {
            TextField(label, text: answerBinding(at: index), axis: .vertical)
                .textFieldStyle(.roundedBorder)
                .lineLimit(1 ... 4)
                .disabled(!localState.canSubmit)
                .accessibilityLabel(label)
        }

        private func planDetails(_ request: PlanRequest) -> some View {
            Text(request.detail)
                .font(.system(.footnote, design: .monospaced))
                .foregroundStyle(Theme.text)
                .fixedSize(horizontal: false, vertical: true)
                .textSelection(.enabled)
        }

        @ViewBuilder
        private func controls(for interaction: PendingInteraction) -> some View {
            switch interaction {
            case .approval:
                HStack(spacing: 8) {
                    Button("Decline") {
                        submit(.submitTapped(.decline))
                    }
                    .buttonStyle(.bordered)
                    .accessibilityLabel("Decline approval")

                    Button("Approve for session") {
                        submit(.submitTapped(.acceptForSession))
                    }
                    .buttonStyle(.bordered)
                    .accessibilityLabel("Approve for session")

                    Button("Approve") {
                        submit(.submitTapped(.accept))
                    }
                    .buttonStyle(.borderedProminent)
                    .tint(Theme.accent)
                    .accessibilityLabel("Approve request")
                }
                .disabled(!localState.canSubmit)

                Button("Cancel task", role: .destructive) {
                    submit(.submitTapped(.cancel))
                }
                .font(.footnote)
                .disabled(!localState.canSubmit)
                .accessibilityLabel("Cancel task")

            case .question:
                HStack {
                    Spacer(minLength: 0)
                    Button("Send answers") {
                        submit(.submitQuestion(answers))
                    }
                    .buttonStyle(.borderedProminent)
                    .tint(Theme.accent)
                    .disabled(!localState.canSubmit || !hasAnswer)
                    .accessibilityLabel("Send answers")
                }

            case .plan:
                HStack(spacing: 8) {
                    Button("Reject", role: .destructive) {
                        submit(.submitPlan(false))
                    }
                    .buttonStyle(.bordered)
                    .accessibilityLabel("Reject plan")

                    Button("Approve") {
                        submit(.submitPlan(true))
                    }
                    .buttonStyle(.borderedProminent)
                    .tint(Theme.accent)
                    .accessibilityLabel("Approve plan")
                }
                .disabled(!localState.canSubmit)
            }
        }

        private func submit(_ action: ApprovalAction) {
            guard let request = ApprovalReducer.reduce(&localState, action) else { return }
            onSubmit(request)
        }

        private var hasAnswer: Bool {
            answers.contains {
                !$0.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            }
        }

        private func answerBinding(at index: Int) -> Binding<String> {
            Binding(
                get: { answers.indices.contains(index) ? answers[index] : "" },
                set: { value in
                    if answers.indices.contains(index) {
                        answers[index] = value
                    } else if index == answers.count {
                        answers.append(value)
                    }
                }
            )
        }

        private static func initialAnswers(for state: ApprovalState) -> [String] {
            guard let request = state.currentQuestion else { return [] }
            return Array(repeating: "", count: max(request.questions.count, 1))
        }

        private func title(for interaction: PendingInteraction) -> String {
            switch interaction {
            case .approval(let approval): return approval.kind.title
            case .question: return "Answer required"
            case .plan: return "Plan approval"
            }
        }

        private func iconName(for interaction: PendingInteraction) -> String {
            switch interaction {
            case .approval(let approval): return approval.kind.iconName
            case .question: return "questionmark.circle.fill"
            case .plan: return "list.bullet.clipboard.fill"
            }
        }

        private func riskColor(_ risk: RiskLevel) -> Color {
            switch risk {
            case .low: return Theme.accent2
            case .medium: return Theme.accent
            case .high: return Theme.danger
            }
        }

        private var statusColor: Color {
            switch localState.status {
            case .failed: return Theme.danger
            case .resolved(let approved): return approved ? Theme.accent2 : Theme.danger
            case .pending, .submitting, .submitted, .stale: return Theme.muted
            }
        }
    }
}

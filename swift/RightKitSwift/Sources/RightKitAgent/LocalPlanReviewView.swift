import SwiftUI

/// Plan-approval round-trip for the `exit_plan_mode` tool (Claude-Code
/// `ExitPlanMode` parity): the driver presents the plan text and blocks until
/// the operator approves (session leaves Plan mode) or rejects.
///
/// ENG-28 exact-effect binding: this view shows the literal `plan` string the
/// engine's `PlanExit::present` callback received for *this* call — never a
/// cached or previously-approved plan — so approving cannot be read as
/// approving different, later-substituted plan content. Callers must
/// construct a fresh `LocalPlanReviewView` per plan-exit callback invocation
/// rather than reusing one across calls.
@MainActor
struct LocalPlanReviewView: View {
    let plan: String
    let onApprove: () -> Void
    let onReject: () -> Void

    @State private var isSubmitting = false

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    Label("Review before leaving Plan mode", systemImage: "list.bullet.clipboard.fill")
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(Theme.text)

                    if plan.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                        Text("The agent did not include any plan content.")
                            .font(.footnote)
                            .foregroundStyle(Theme.muted)
                    } else {
                        MarkdownText(raw: plan)
                            .textSelection(.enabled)
                    }
                }
                .padding(16)
            }
            .background(Theme.background)
            .navigationTitle("Plan approval")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Reject", role: .destructive) { submit(onReject) }
                        .disabled(isSubmitting)
                        .accessibilityLabel("Reject plan")
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Approve") { submit(onApprove) }
                        .disabled(isSubmitting)
                        .accessibilityLabel("Approve plan")
                }
            }
        }
    }

    /// Locks the controls on first tap so a slow daemon round trip cannot be
    /// raced into a double submission (approve-then-reject or vice versa).
    private func submit(_ action: () -> Void) {
        guard !isSubmitting else { return }
        isSubmitting = true
        action()
    }
}

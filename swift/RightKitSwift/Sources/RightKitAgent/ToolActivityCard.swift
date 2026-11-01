import SwiftUI

/// Compact, expandable projection of one tool lifecycle.
struct ToolActivityCard: View {
    let activity: ToolActivity
    @State private var expanded = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Button {
                withAnimation(.easeInOut(duration: 0.15)) { expanded.toggle() }
            } label: {
                HStack(spacing: 8) {
                    statusIcon
                        .frame(width: 18, height: 18)
                    Text(activity.name)
                        .font(.system(.subheadline, design: .monospaced).weight(.medium))
                        .foregroundStyle(Theme.text)
                        .lineLimit(1)
                    if !expanded, let summary = activity.inputSummary {
                        Text(summary)
                            .font(.system(.caption, design: .monospaced))
                            .foregroundStyle(Theme.muted)
                            .lineLimit(1)
                    }
                    Spacer(minLength: 8)
                    Text(activity.status.label)
                        .font(.caption.weight(.medium))
                        .foregroundStyle(Theme.muted)
                    Image(systemName: "chevron.down")
                        .font(.caption2.weight(.semibold))
                        .rotationEffect(.degrees(expanded ? 180 : 0))
                        .accessibilityHidden(true)
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Tool \(activity.name)")
            .accessibilityValue(activity.status.label)
            .accessibilityHint(expanded ? "Collapses details" : "Expands details")

            if expanded {
                if !activity.input.isEmpty { codeBlock(label: "Input", text: activity.input) }
                if !activity.output.isEmpty { codeBlock(label: "Output", text: activity.output) }
                if case .failed(let message) = activity.status {
                    Label(message, systemImage: "xmark.circle")
                        .font(.caption)
                        .foregroundStyle(Theme.danger)
                        .accessibilityLabel("Tool failure: \(message)")
                }
            }
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 8)
        .background(Theme.roleTint(.tool), in: RoundedRectangle(cornerRadius: 10, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .stroke(borderColor, lineWidth: 1)
        )
    }

    @ViewBuilder
    private var statusIcon: some View {
        switch activity.status {
        case .running:
            Image(systemName: "arrow.triangle.2.circlepath")
                .font(.caption.weight(.semibold))
                .foregroundStyle(Theme.accent2)
                .accessibilityLabel("Running")
        case .succeeded:
            Image(systemName: "checkmark.circle.fill")
                .font(.caption)
                .foregroundStyle(Theme.accent2)
                .accessibilityLabel("Succeeded")
        case .failed:
            Image(systemName: "xmark.circle.fill")
                .font(.caption)
                .foregroundStyle(Theme.danger)
                .accessibilityLabel("Failed")
        }
    }

    private var borderColor: Color {
        switch activity.status {
        case .running: return Theme.accent2.opacity(0.45)
        case .succeeded: return Theme.border
        case .failed: return Theme.danger.opacity(0.6)
        }
    }

    private func codeBlock(label: String, text: String) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(label)
                .font(.caption2.weight(.semibold))
                .foregroundStyle(Theme.muted)
            ScrollView(.horizontal, showsIndicators: false) {
                Text(String(text.prefix(4000)))
                    .font(.system(.caption, design: .monospaced))
                    .foregroundStyle(Theme.text)
                    .textSelection(.enabled)
            }
        }
        .padding(8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.background, in: RoundedRectangle(cornerRadius: 7, style: .continuous))
    }
}

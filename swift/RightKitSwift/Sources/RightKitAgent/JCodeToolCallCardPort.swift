import SwiftUI

/*
 Provenance (exact donor revision):
 - 1jehuang/jcode @ 10c26eb12a2e8eb14c505a6dc954e164cb62fe18
 - ios/Sources/JCodeMobile/Views/ToolCallCard.swift:1-111

 This is a CodeRight-native transplant of the donor's compact/collapsible
 tool card. Theme tokens and the call type are supplied by CodeRight; no
 JCodeMobile view or Theme type is linked.
 */

public extension CodeRightToolInteractionsPort {
    /// Compact, expandable tool lifecycle card. The call id belongs to the
    /// parent row, so SwiftUI preserves expanded state across live updates.
    struct ToolCallCard: View {
        public let call: ToolCall
        @State private var expanded = false

        public init(call: ToolCall) {
            self.call = call
        }

        public var body: some View {
            VStack(alignment: .leading, spacing: 10) {
                Button {
                    withAnimation(.easeInOut(duration: 0.15)) {
                        expanded.toggle()
                    }
                } label: {
                    HStack(spacing: 8) {
                        statusIcon
                            .frame(width: 16, height: 16)
                        Text(call.name)
                            .font(.system(size: 13, weight: .medium, design: .monospaced))
                            .foregroundStyle(Theme.text)
                            .lineLimit(1)
                        if !expanded, let summary = call.inputSummary {
                            Text(summary)
                                .font(.system(size: 11, design: .monospaced))
                                .foregroundStyle(Theme.muted)
                                .lineLimit(1)
                        }
                        Spacer(minLength: 8)
                        Image(systemName: "chevron.down")
                            .font(.caption2.weight(.semibold))
                            .foregroundStyle(Theme.muted)
                            .rotationEffect(.degrees(expanded ? 180 : 0))
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Tool \(call.name)")
                .accessibilityValue(call.status.label)
                .accessibilityHint(expanded ? "Collapses details" : "Expands input and output")

                if expanded {
                    if !call.input.isEmpty {
                        codeBlock(call.input)
                    }
                    if !call.output.isEmpty {
                        codeBlock(String(call.output.prefix(2000)))
                    }
                    if case let .failed(message) = call.status {
                        Text(message)
                            .font(.system(size: 12, design: .monospaced))
                            .foregroundStyle(Theme.danger)
                            .accessibilityLabel("Tool failure: \(message)")
                    }
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 10)
            .background(Theme.surface)
            .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: 12, style: .continuous)
                    .stroke(accent, lineWidth: 1)
            )
        }

        /// Border tint hints at status without making color the only signal.
        private var accent: Color {
            switch call.status {
            case .streamingInput, .running: return Theme.accent2.opacity(0.3)
            case .failed: return Theme.danger.opacity(0.3)
            case .succeeded: return Theme.border
            }
        }

        @ViewBuilder
        private var statusIcon: some View {
            switch call.status {
            case .streamingInput, .running:
                ProgressView()
                    .controlSize(.mini)
                    .tint(Theme.accent2)
                    .accessibilityLabel(call.status.label)
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

        private func codeBlock(_ text: String) -> some View {
            ScrollView(.horizontal, showsIndicators: false) {
                Text(text)
                    .font(.system(size: 11, design: .monospaced))
                    .foregroundStyle(Theme.text)
                    .padding(10)
                    .textSelection(.enabled)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Theme.background)
            .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
        }
    }
}

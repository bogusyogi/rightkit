import RightKitUI
import SwiftUI

enum ChatChrome {
    static let horizontalInset: CGFloat = 20
    static let selectorHeight: CGFloat = 48
    static let composerRadius: CGFloat = 22
    static let composerInset: CGFloat = 4
    static let composerActionSize: CGFloat = 44
    static let composerInputMinHeight: CGFloat = 40
}

struct ChatSelectorRow: View {
    let icon: String
    let value: String
    var title: String? = nil
    var dimmed = false

    var body: some View {
        HStack(spacing: 8) {
            VStack(alignment: .leading, spacing: 4) {
                if let title {
                    Text(title)
                        .font(Theme.body(size: 11, weight: .medium, relativeTo: .caption))
                        .foregroundStyle(Theme.muted)
                }
                HStack(spacing: 6) {
                    Image(systemName: icon)
                        .font(.system(size: 13))
                        .accessibilityHidden(true)
                    Text(value)
                        .font(Theme.body(size: 13, weight: .medium, relativeTo: .subheadline))
                        .lineLimit(1)
                }
                .foregroundStyle(dimmed ? Theme.muted : Theme.text)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            Image(systemName: "chevron.down")
                .font(.system(size: 9, weight: .semibold))
                .foregroundStyle(Theme.muted)
                .accessibilityHidden(true)
        }
        .frame(minHeight: 58)
        .contentShape(Rectangle())
    }
}

struct ChatCircleButton: View {
    let systemName: String
    let accessibilityLabel: String
    var filled = false
    var destructive = false
    var disabled = false
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: systemName)
                .font(.system(size: 18, weight: .semibold))
                .foregroundStyle(foreground)
                .frame(width: ChatChrome.composerActionSize, height: ChatChrome.composerActionSize)
                .background(background, in: Circle())
                .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .disabled(disabled)
        .accessibilityLabel(accessibilityLabel)
    }

    private var foreground: Color {
        if disabled { return Theme.muted }
        if destructive { return Theme.danger }
        return filled ? Theme.onAccent : Theme.text
    }

    private var background: Color {
        if destructive { return Theme.danger.opacity(0.14) }
        if filled { return Theme.accent }
        if disabled { return Theme.raised.opacity(0.7) }
        return Color.clear
    }
}

extension View {
    func chatComposerSurface() -> some View {
        padding(.horizontal, ChatChrome.composerInset)
            .padding(.vertical, 5)
            .modifier(ChatComposerMaterial())
    }
}

private struct ChatComposerMaterial: ViewModifier {
    @Environment(\.codeRightReduceTransparency) private var reduceTransparency

    /// Glass vs. opaque branching (including the increased-contrast fallback) is RightKitUI's;
    /// CodeRight supplies the radius, the user's reduce-transparency override and its own colors.
    func body(content: Content) -> some View {
        content.rightKitAdaptiveGlass(
            minimumCornerRadius: ChatChrome.composerRadius,
            fallbackBorder: Theme.muted,
            opaqueBackground: Theme.surface,
            opaqueBorder: Theme.muted,
            forceOpaque: reduceTransparency
        )
    }
}

#if os(macOS)
import RightKitHub
import SwiftUI

// Small building blocks for the Storage views, drawn only from controls SwiftRight covers.

/// A button in the page's weights: normal (a flat tinted fill), outline (a bordered fill on the page colour), primary and ghost.
struct StorageButton: View {
    enum Weight { case normal, outline, primary, ghost }

    let title: String
    var symbol: String?
    var weight: Weight = .normal
    /// Dims and disables a normal button (a custom-drawn button does not dim by itself).
    var enabled = true
    let action: () -> Void
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var label: some View {
        HStack(spacing: 4) {
            if let symbol { Image(systemName: symbol) }
            Text(verbatim: title)
                .lineLimit(1)
        }
    }

    var body: some View {
        // A button keeps its width on Windows' wider font; the text and bars beside it give way first.
        switch weight {
        case .normal, .outline:
            Button(action: action) {
                HubLayered {
                    RoundedRectangle(cornerRadius: 7)
                        .fill(weight == .outline ? theme.palette(scheme).plane.color : theme.palette(scheme).fill2.color)
                    RoundedRectangle(cornerRadius: 7)
                        .stroke(weight == .outline ? theme.palette(scheme).line.color : Color.clear, lineWidth: 1)
                    label
                        .font(theme.type.label.font)
                        .foregroundStyle(theme.palette(scheme).ink.color)
                        .padding(.horizontal, 10)
                        .padding(.vertical, 5)
                }
            }
            .buttonStyle(.plain)
            .disabled(!enabled)
            .opacity(enabled ? 1 : 0.5)
            .layoutPriority(1)
        case .primary:
            Button(action: action) { label.font(theme.type.caption.font) }
                .buttonStyle(.borderedProminent).layoutPriority(1)
        case .ghost:
            Button(action: action) { label.font(theme.type.caption.font) }
                .buttonStyle(.borderless).layoutPriority(1)
        }
    }
}

/// The page's empty state: a tinted round mark holding the symbol, a bold title, an optional line and
/// an optional button, centred.
struct StorageEmpty: View {
    let symbol: String
    let title: String
    var message = ""
    var actionTitle: String?
    var action: (@MainActor () -> Void)?
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        VStack(spacing: 8) {
            ZStack {
                RoundedRectangle(cornerRadius: 18).fill(palette.fill2.color)
                Image(systemName: symbol)
                    .font(Font.system(size: 17))
                    .foregroundStyle(palette.accent.color)
            }
            .frame(width: 36, height: 36)
            Text(verbatim: title)
                .font(Font.system(size: 14, weight: .semibold))
                .foregroundStyle(palette.ink.color)
            if !message.isEmpty {
                Text(verbatim: message)
                    .font(theme.type.caption.font)
                    .foregroundStyle(palette.ink2.color)
            }
            if let actionTitle, let action {
                StorageButton(title: actionTitle) { action() }
            }
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 24)
    }
}

/// A one-line notice with an optional action: the page's `st-notice`.
struct StorageNotice: View {
    let text: String
    var tone: HubTone = .neutral
    var actionTitle: String?
    var actionDisabled = false
    var action: (() -> Void)?
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    private var fill: HubColor {
        switch tone {
        case .warn: return palette.warnSoft
        case .bad: return palette.badSoft
        case .ok: return palette.okSoft
        default: return palette.fill2
        }
    }

    private var ink: HubColor {
        switch tone {
        case .warn: return palette.warn
        case .bad: return palette.bad
        default: return palette.ink
        }
    }

    var body: some View {
        HubLayered {
            RoundedRectangle(cornerRadius: theme.radii.item).fill(fill.color)
            RoundedRectangle(cornerRadius: theme.radii.item).stroke(palette.hairline.color, lineWidth: 1)
            HStack(spacing: theme.spacing.row) {
                Text(verbatim: text)
                    .font(theme.type.caption.font)
                    .foregroundStyle(ink.color)
                    .frame(maxWidth: .infinity, alignment: .leading)
                if let actionTitle, let action {
                    StorageButton(title: actionTitle, weight: .ghost, action: action)
                        .disabled(actionDisabled)
                }
            }
            .padding(.horizontal, theme.spacing.row)
            .padding(.vertical, theme.spacing.xs + 2)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

/// A small status pill: Safe, Review, OK, Warning, Keep.
struct StorageBadge: View {
    let text: String
    let tone: HubTone
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    private var fill: HubColor {
        switch tone {
        case .ok: return palette.okSoft
        case .warn: return palette.warnSoft
        case .bad: return palette.badSoft
        case .accent: return palette.accentSoft
        case .neutral: return palette.fill2
        }
    }

    var body: some View {
        HubLayered {
            RoundedRectangle(cornerRadius: theme.radii.pill).fill(fill.color)
            Text(verbatim: text)
                .font(theme.type.micro.font)
                .foregroundStyle(tone.color(in: palette).color)
                .lineLimit(1)
                .padding(.horizontal, 7)
                .padding(.vertical, 1)
        }
        // A badge keeps its width on Windows' wider font; the title beside it gives way first.
        .layoutPriority(1)
    }
}

/// A bar filled to `fraction` in any colour, on the section's track.
struct StorageFractionBar: View {
    let fraction: Double
    let color: Color
    var height: CGFloat = 4
    /// What the bar measures and its value, for screen readers.
    var label = "Share"
    var value = ""
    /// The track behind the fill; nil for the theme's `fill3`. The page's volume and row bars sit on `plane`.
    var track: Color?
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        let clamped = fraction.isFinite ? min(1, max(0, fraction)) : 0
        StorageFractionLayout(fraction: clamped) {
            RoundedRectangle(cornerRadius: theme.radii.pill).fill(track ?? theme.palette(scheme).fill3.color)
            RoundedRectangle(cornerRadius: theme.radii.pill).fill(color)
        }
        .frame(height: height)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(label)
        .accessibilityValue(value)
    }
}

/// Places the first subview over the full bounds and the second over the leading `fraction` of the width.
private struct StorageFractionLayout: Layout {
    var fraction: Double

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        CGSize(width: proposal.width ?? 100, height: proposal.height ?? 4)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        guard subviews.count >= 2 else { return }
        subviews[0].place(at: bounds.origin, anchor: .topLeading, proposal: ProposedViewSize(width: bounds.width, height: bounds.height))
        subviews[1].place(at: bounds.origin, anchor: .topLeading, proposal: ProposedViewSize(width: bounds.width * fraction, height: bounds.height))
    }
}

/// One proportional bar split into segments, one per item, each as wide as its share. It replaces the
/// treemap: the same sizes, in one row.
struct StorageStrip: View {
    struct Segment: Identifiable {
        let id: String
        let weight: Double
        let color: Color
        /// Written in the segment when it is wide enough, as the page's treemap does.
        var label = ""
    }

    let segments: [Segment]
    var height: CGFloat = 22
    @Environment(\.hubTheme) private var theme

    var body: some View {
        let total = max(segments.reduce(0) { $0 + $1.weight }, 1)
        StorageWeightedLayout(weights: segments.map(\.weight)) {
            ForEach(segments) { segment in
                ZStack(alignment: .topLeading) {
                    RoundedRectangle(cornerRadius: 2).fill(segment.color).opacity(0.88)
                    if !segment.label.isEmpty && segment.weight / total >= 0.12 {
                        Text(verbatim: segment.label)
                            .font(Font.system(size: 11))
                            .foregroundStyle(Color.white)
                            .lineLimit(1)
                            .padding(.horizontal, 4)
                            .padding(.top, 2)
                    }
                }
            }
        }
        .frame(height: height)
    }
}

private struct StorageWeightedLayout: Layout {
    var weights: [Double]

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        CGSize(width: proposal.width ?? 200, height: proposal.height ?? 22)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        let total = weights.reduce(0, +)
        guard total > 0 else { return }
        var x = bounds.minX
        for (index, subview) in subviews.enumerated() where index < weights.count {
            let width = bounds.width * weights[index] / total
            subview.place(
                at: CGPoint(x: x, y: bounds.minY),
                anchor: .topLeading,
                proposal: ProposedViewSize(width: max(width - 1, 0.5), height: bounds.height)
            )
            x += width
        }
    }
}

/// The four workspaces as a row of buttons on a tinted track, the chosen one raised (the page's
/// `SegmentedControl`).
struct StorageTabs: View {
    let panes: [StorageModel.Pane]
    let selection: StorageModel.Pane
    let choose: (StorageModel.Pane) -> Void
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        HubLayered {
            RoundedRectangle(cornerRadius: theme.radii.item).fill(palette.fill2.color)
            HStack(spacing: 2) {
                ForEach(panes, id: \.self) { pane in
                    let chosen = pane == selection
                    Button {
                        choose(pane)
                    } label: {
                        HubLayered {
                            RoundedRectangle(cornerRadius: theme.radii.control)
                                .fill(chosen ? palette.elevated.color : Color.clear)
                            RoundedRectangle(cornerRadius: theme.radii.control)
                                .stroke(chosen ? palette.line.color : Color.clear, lineWidth: 1)
                            Text(verbatim: pane.title)
                                .font(theme.type.label.font)
                                .foregroundStyle(chosen ? palette.ink.color : palette.ink2.color)
                                .lineLimit(1)
                                .padding(.horizontal, 12)
                                .padding(.vertical, 5)
                        }
                    }
                    .buttonStyle(.plain)
                    .accessibilityAddTraits(chosen ? .isSelected : [])
                    // Longest title first, so none is cut when the row only just fits (Fit.swift).
                    .layoutPriority(Fit.segment(pane.title))
                }
            }
            .padding(2)
        }
        // The whole control keeps its width on Windows' wider font.
        .layoutPriority(2)
    }
}

/// The page's panel: a tinted fill with a hairline border, content top to bottom.
struct StorageCard<Content: View>: View {
    private let content: Content
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    init(@ViewBuilder content: () -> Content) {
        self.content = content()
    }

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        HubLayered {
            RoundedRectangle(cornerRadius: theme.radii.card).fill(palette.fill2.color)
            RoundedRectangle(cornerRadius: theme.radii.card).stroke(palette.hairline.color, lineWidth: 1)
            VStack(alignment: .leading, spacing: 0) {
                content
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 10)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

/// The page's native select, as a button: the chosen value and a chevron, named by what it chooses
/// (so a screen reader says "Kind, All kinds"). Pressing it opens the choices under the row.
struct StorageSelect: View {
    let label: String
    let value: String
    var open = false
    var enabled = true
    var minWidth: CGFloat = 128
    var maxWidth: CGFloat?
    let action: () -> Void
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        Button(action: action) {
            HubLayered {
                RoundedRectangle(cornerRadius: theme.radii.item).fill(open ? palette.fill2.color : palette.plane.color)
                RoundedRectangle(cornerRadius: theme.radii.item).stroke(palette.line.color, lineWidth: 1)
                HStack(spacing: 8) {
                    Text(verbatim: value)
                        .font(Font.system(size: 13, weight: .semibold))
                        .foregroundStyle(palette.ink.color)
                        .lineLimit(1)
                        // Served before the chevron, or an equal share cuts it at half the select (Fit.swift).
                        .layoutPriority(Fit.priority(windows: 1))
                    Spacer(minLength: 4)
                    Image(systemName: open ? "chevron.up" : "chevron.down")
                        .font(Font.system(size: 10))
                        .foregroundStyle(palette.ink2.color)
                }
                .padding(.horizontal, 10)
                .padding(.vertical, 6)
                .frame(minWidth: minWidth, maxWidth: maxWidth, alignment: .leading)
            }
        }
        .buttonStyle(.plain)
        .disabled(!enabled)
        .opacity(enabled ? 1 : 0.5)
        .accessibilityLabel(label)
        .accessibilityValue(value)
    }
}

/// One choice of a `StorageChoicePanel`.
struct StorageChoice: Identifiable {
    let id: String
    let title: String
    var selected = false
    var enabled = true
    /// A small heading shown above this choice ("Network").
    var heading: String?
    let action: () -> Void
}

/// The choices of a select, open under its row: a bordered column, the chosen one ticked.
struct StorageChoicePanel: View {
    let choices: [StorageChoice]
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        HubLayered {
            RoundedRectangle(cornerRadius: theme.radii.item).fill(palette.plane.color)
            RoundedRectangle(cornerRadius: theme.radii.item).stroke(palette.line.color, lineWidth: 1)
            VStack(alignment: .leading, spacing: 0) {
                ForEach(choices) { choice in
                    if let heading = choice.heading {
                        Text(verbatim: heading)
                            .font(theme.type.small.font)
                            .foregroundStyle(palette.ink3.color)
                            .padding(.top, 6)
                    }
                    Button {
                        choice.action()
                    } label: {
                        HubLayered {
                            HubHitArea()
                            HStack(spacing: theme.spacing.sm) {
                                Text(verbatim: choice.title)
                                    .font(theme.type.body.font)
                                    .foregroundStyle(palette.ink.color)
                                    .lineLimit(1)
                                Spacer()
                                Image(systemName: "checkmark")
                                    .foregroundStyle(palette.accent.color)
                                    .opacity(choice.selected ? 1 : 0)
                            }
                            .padding(.vertical, theme.spacing.xs + 2)
                            .frame(maxWidth: .infinity, alignment: .leading)
                        }
                    }
                    .buttonStyle(.plain)
                    .disabled(!choice.enabled)
                    .opacity(choice.enabled ? 1 : 0.5)
                    .accessibilityAddTraits(choice.selected ? .isSelected : [])
                }
            }
            .padding(.horizontal, theme.spacing.md)
            .padding(.vertical, theme.spacing.xs)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

/// The page's search box: a tinted rounded field with the placeholder drawn under the text. The
/// field itself has an empty prompt and the accessibility `label`, so a screen reader (and UI
/// automation) finds it by name; a field whose label equals its prompt reports no name on the Mac.
struct StorageSearchField: View {
    let placeholder: String
    let label: String
    let text: Binding<String>
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        HubLayered {
            RoundedRectangle(cornerRadius: theme.radii.field).fill(palette.fill2.color)
            RoundedRectangle(cornerRadius: theme.radii.field).stroke(palette.hairline.color, lineWidth: 1)
            HStack(spacing: 6) {
                HubLayered {
                    Text(verbatim: placeholder)
                        .font(theme.type.body.font)
                        .foregroundStyle(palette.ink3.color)
                        .lineLimit(1)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .opacity(text.wrappedValue.isEmpty ? 1 : 0)
                    TextField("", text: text)
                        .textFieldStyle(.plain)
                        .font(theme.type.body.font)
                        .foregroundStyle(palette.ink.color)
                        .accessibilityLabel(label)
                }
                if !text.wrappedValue.isEmpty {
                    Button {
                        text.wrappedValue = ""
                    } label: {
                        Image(systemName: "xmark.circle.fill")
                            .foregroundStyle(palette.ink3.color)
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Clear search")
                }
            }
            .padding(.horizontal, 10)
            .frame(height: 30)
        }
    }
}

/// A label with its value on the right: one fact of the inspector.
struct StorageFact: View {
    let label: String
    let value: String
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        let palette = theme.palette(scheme)
        HStack(alignment: .top, spacing: theme.spacing.md) {
            Text(verbatim: label)
                .font(theme.type.caption.font)
                .foregroundStyle(palette.ink2.color)
            Spacer()
            Text(verbatim: value)
                .font(theme.type.caption.font)
                .foregroundStyle(palette.ink.color)
        }
    }
}
#endif

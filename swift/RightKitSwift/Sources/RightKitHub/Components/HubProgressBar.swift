import SwiftUI

/// A horizontal progress bar: a track `RoundedRectangle` and a fill `RoundedRectangle` whose width
/// follows the fraction. A bar, not a ring (ProgressView and `Circle().trim` are not covered by SwiftRight).
public struct HubProgressBar: View {
    private let fraction: Double
    private let tone: HubTone
    private let height: CGFloat
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    /// - Parameters:
    ///   - value: Progress from 0 to 1; values outside are clamped.
    ///   - tone: The fill colour.
    ///   - height: Bar thickness in points.
    public init(value: Double, tone: HubTone = .accent, height: CGFloat = 6) {
        self.fraction = value.isFinite ? min(1, max(0, value)) : 0
        self.tone = tone
        self.height = height
    }

    private var palette: HubPalette { theme.palette(scheme) }

    public var body: some View {
        HubFractionLayout(fraction: fraction) {
            RoundedRectangle(cornerRadius: theme.radii.pill).fill(palette.fill3.color)
            RoundedRectangle(cornerRadius: theme.radii.pill).fill(tone.color(in: palette).color)
        }
        .frame(height: height)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Progress")
        .accessibilityValue("\(Int((fraction * 100).rounded())) percent")
    }
}

/// Places the first subview over the full bounds and the second over the leading `fraction` of the
/// width, so the fill's width is set by layout and no clip or trim is needed.
private struct HubFractionLayout: Layout {
    var fraction: Double

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        CGSize(width: proposal.width ?? 100, height: proposal.height ?? 6)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        guard subviews.count >= 2 else { return }
        subviews[0].place(
            at: bounds.origin,
            anchor: .topLeading,
            proposal: ProposedViewSize(width: bounds.width, height: bounds.height)
        )
        subviews[1].place(
            at: bounds.origin,
            anchor: .topLeading,
            proposal: ProposedViewSize(width: bounds.width * fraction, height: bounds.height)
        )
    }
}

import SwiftUI

/// Layers views on top of each other and sizes the stack by its LAST child only.
///
/// This replaces `.background`, which SwiftRight does not cover. Put the shapes first and the real
/// content last: the shapes are then proposed exactly the content's size, so a `RoundedRectangle`
/// fill behind a card does not grow to fill its parent the way it would in a plain ZStack.
///
/// Built on SwiftUI's `Layout` protocol (`swiftui.custom-layout`, delivered in SwiftRight).
public struct HubLayered<Content: View>: View {
    private let content: Content

    public init(@ViewBuilder content: () -> Content) {
        self.content = content()
    }

    public var body: some View {
        HubLayeredLayout {
            content
        }
    }
}

private struct HubLayeredLayout: Layout {
    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        guard let last = subviews.last else { return .zero }
        return last.sizeThatFits(proposal)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        let size = ProposedViewSize(width: bounds.width, height: bounds.height)
        for subview in subviews {
            subview.place(at: bounds.origin, anchor: .topLeading, proposal: size)
        }
    }
}

/// A near-invisible rectangle that makes the whole area of a plain button hit-testable.
/// Plain-style buttons only respond over drawn content, so a row with a Spacer needs one under it.
/// Used in place of `.contentShape`, which SwiftRight does not cover. Put it first in a `HubLayered`.
public struct HubHitArea: View {
    public init() {}

    public var body: some View {
        Rectangle().fill(Color.black.opacity(0.001))
    }
}

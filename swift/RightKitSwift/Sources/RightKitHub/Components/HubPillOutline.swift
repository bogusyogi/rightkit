import SwiftUI

/// A pill (or any rounded shape) with a hairline outline, drawn from fills only.
///
/// SwiftRight draws a stroked pill-radius `RoundedRectangle` with tick artifacts, so the outline is an
/// outer fill in the line colour and an inner fill inset by the line width: the same picture as a
/// stroke, from the fills SwiftRight proves. Put it first in a `HubLayered`, under the label.
///
/// - `line`: the outline colour.
/// - `base`: an opaque colour matching what the pill sits on (the card or the page). It hides the
///   outer fill inside the outline, so a translucent `fill` over it looks as it does over the page.
/// - `fill`: the pill's own fill, drawn over `base`; `Color.clear` for an outline only.
public struct HubPillOutline: View {
    private let line: Color
    private let base: Color
    private let fill: Color
    private let lineWidth: CGFloat
    private let radius: CGFloat

    public init(line: Color, base: Color, fill: Color = Color.clear, lineWidth: CGFloat = 1, radius: CGFloat = 99) {
        self.line = line
        self.base = base
        self.fill = fill
        self.lineWidth = lineWidth
        self.radius = radius
    }

    public var body: some View {
        HubLayered {
            RoundedRectangle(cornerRadius: radius).fill(line)
            RoundedRectangle(cornerRadius: max(radius - lineWidth, 0)).fill(base).padding(lineWidth)
            RoundedRectangle(cornerRadius: max(radius - lineWidth, 0)).fill(fill).padding(lineWidth)
        }
    }
}

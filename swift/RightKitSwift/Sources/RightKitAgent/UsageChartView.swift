import SwiftUI

/// One bar in a `UsageChartView`.
struct UsageChartBar: Identifiable {
    let label: String
    let value: Double

    var id: String { label }
}

/// A minimal hand-drawn horizontal bar chart — no Charts framework
/// dependency, no third-party package. Deliberately simple: this is a cost
/// dashboard, not a data-viz surface, and the values involved (dollars,
/// tokens) read more clearly as ranked bars with labels than as a
/// zoomable/pannable chart.
struct UsageChartView: View {
    let bars: [UsageChartBar]

    private var maxValue: Double {
        max(bars.map(\.value).max() ?? 0, 0.0001)
    }

    var body: some View {
        GeometryReader { proxy in
            VStack(alignment: .leading, spacing: 8) {
                ForEach(bars) { bar in
                    HStack(spacing: 8) {
                        Text(bar.label)
                            .font(.caption2)
                            .foregroundStyle(Theme.muted)
                            .frame(width: 76, alignment: .leading)
                            .lineLimit(1)

                        RoundedRectangle(cornerRadius: 3)
                            .fill(Theme.accent)
                            .frame(
                                width: max(4, proxy.size.width * 0.6 * CGFloat(bar.value / maxValue)),
                                height: 14
                            )

                        Text(String(format: "$%.4f", bar.value))
                            .font(.caption2.monospacedDigit())
                            .foregroundStyle(Theme.muted)
                    }
                }
            }
        }
    }
}

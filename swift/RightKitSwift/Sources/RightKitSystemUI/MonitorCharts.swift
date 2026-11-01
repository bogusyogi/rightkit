import Foundation
import RightKitHub
import SwiftUI

// Monitor's history charts: the look of `hub/src/components/Chart.tsx` (`AreaChart`) from the controls
// the hub may use. Each chart is a row of narrow columns: a 1.5 pt line at the value with a 14 percent
// fill under it for the first series, and a line only (dashed by skipping every third column) for the
// others, over three grid lines, with the scale on the left and the time under the plot.

enum MonChartMetrics {
    /// Columns across the plot, and the width of each.
    static let bins = 60
    static let binWidth: CGFloat = 2
    static var plotWidth: CGFloat { CGFloat(bins) * binWidth }
    /// The scale's width and its gap to the plot. Three cards share one row (#415), so the plot is
    /// narrow and short: 120 x 56 pt, with a 36 pt scale.
    static let axisWidth: CGFloat = 36
    static let axisGap: CGFloat = 4
    static let plotHeight: CGFloat = 56
    static let line: CGFloat = 1.5
}

/// One series of a chart: a value per column (nil is a gap).
struct MonLineSpec: Identifiable {
    let id: Int
    let label: String
    let color: Color
    let values: [Double?]
    let dashed: Bool
}

enum MonChartRules {
    /// The page's usage colour: below 70% ok, 70 to 90% warn, 90 and up bad.
    static func level(_ percent: Double) -> MonLevel {
        if percent >= 90 { return .bad }
        if percent >= 70 { return .warn }
        return .ok
    }

    /// The top of an automatic scale: 1, 2, 5 or 10 times a power of ten.
    static func niceMax(_ value: Double) -> Double {
        if value <= 0 { return 1 }
        var power = 1.0
        while power * 10 <= value { power *= 10 }
        while power > value { power /= 10 }
        let fraction = value / power
        return (fraction <= 1 ? 1 : fraction <= 2 ? 2 : fraction <= 5 ? 5 : 10) * power
    }

    /// "6:27 AM": the clock time of a sample, in the viewer's own format.
    static func timeText(_ ms: Double) -> String {
        let formatter = DateFormatter()
        formatter.setLocalizedDateFormatFromTemplate("jmm")
        return formatter.string(from: Date(timeIntervalSince1970: ms / 1000))
    }

    /// What a screen reader gets for a chart: the latest value of each series and the range seen.
    static func summary(_ label: String, lines: [MonLineSpec], minutes: Int, format: (Double) -> String) -> String {
        let all = lines.flatMap { $0.values.compactMap { $0 } }
        guard let low = all.min(), let high = all.max() else { return "\(label): no data yet" }
        var parts: [String] = []
        for spec in lines {
            guard let found = spec.values.last(where: { $0 != nil }), let value = found else { continue }
            parts.append((lines.count > 1 ? "\(spec.label) " : "") + format(value))
        }
        return "\(label): now \(parts.joined(separator: ", ")); over the last \(minutes) minutes from \(format(low)) to \(format(high))"
    }
}

/// One column of a chart: its height in points (nil for a gap) and whether its line is drawn (a
/// dashed series leaves every third column out).
private struct MonColumn: Identifiable {
    let id: Int
    let height: CGFloat?
    let lineOn: Bool
}

/// A chart of one to three series on a shared scale, with its scale and time labels.
struct MonTimeChart: View {
    let lines: [MonLineSpec]
    let top: Double
    let topLabel: String
    let bottomLabel: String
    let summary: String
    let leftLabel: String
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(alignment: .top, spacing: MonChartMetrics.axisGap) {
                VStack(alignment: .trailing, spacing: 0) {
                    Text(verbatim: topLabel)
                        .font(Font.system(size: 11))
                        .foregroundStyle(palette.ink3.color)
                        .lineLimit(1)
                    Spacer(minLength: 0)
                    Text(verbatim: bottomLabel)
                        .font(Font.system(size: 11))
                        .foregroundStyle(palette.ink3.color)
                        .lineLimit(1)
                }
                .frame(width: MonChartMetrics.axisWidth, height: MonChartMetrics.plotHeight, alignment: .topTrailing)
                plot
            }
            HStack(spacing: 0) {
                Text(verbatim: leftLabel)
                    .lineLimit(1)
                Spacer(minLength: 0)
                Text(verbatim: "now")
                    .lineLimit(1)
            }
            .font(Font.system(size: 11))
            .foregroundStyle(palette.ink3.color)
            .padding(.leading, MonChartMetrics.axisWidth + MonChartMetrics.axisGap)
            .frame(width: MonChartMetrics.axisWidth + MonChartMetrics.axisGap + MonChartMetrics.plotWidth)
        }
    }

    private var plot: some View {
        let width = MonChartMetrics.plotWidth
        let height = MonChartMetrics.plotHeight
        return HubLayered {
            grid
            ZStack(alignment: .topLeading) {
                ForEach(lines) { spec in
                    layer(spec)
                }
            }
            .frame(width: width, height: height)
            Color.clear
                .frame(width: width, height: height)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(summary)
    }

    /// Lines at the top, the middle and the base of the scale.
    private var grid: some View {
        VStack(spacing: 0) {
            Rectangle().fill(palette.hairline.color).frame(height: 1)
            Spacer(minLength: 0)
            Rectangle().fill(palette.hairline.color).frame(height: 1)
            Spacer(minLength: 0)
            Rectangle().fill(palette.ink3.color.opacity(0.5)).frame(height: 1)
        }
        .frame(width: MonChartMetrics.plotWidth, height: MonChartMetrics.plotHeight)
    }

    private func columns(_ spec: MonLineSpec) -> [MonColumn] {
        let height = MonChartMetrics.plotHeight
        return spec.values.enumerated().map { entry in
            let fraction = entry.element.map { top > 0 ? min(max($0 / top, 0), 1) : 0 }
            return MonColumn(
                id: entry.offset,
                height: fraction.map { CGFloat($0) * height },
                lineOn: !spec.dashed || entry.offset % 3 != 2)
        }
    }

    /// One series: the first also fills under its line.
    private func layer(_ spec: MonLineSpec) -> some View {
        let fills = spec.id == 0
        return HStack(alignment: .bottom, spacing: 0) {
            ForEach(columns(spec)) { column in
                VStack(spacing: 0) {
                    Spacer(minLength: 0)
                    if let height = column.height {
                        Rectangle()
                            .fill(column.lineOn ? spec.color : Color.clear)
                            .frame(height: MonChartMetrics.line)
                        Rectangle()
                            .fill(fills ? spec.color.opacity(0.14) : Color.clear)
                            .frame(height: max(0, height - MonChartMetrics.line))
                    }
                }
                .frame(width: MonChartMetrics.binWidth)
            }
        }
        .frame(width: MonChartMetrics.plotWidth, height: MonChartMetrics.plotHeight, alignment: .bottomLeading)
    }
}

/// The key under a chart: a short line in each series' colour, dashed where the series is.
struct MonLegend: View {
    let lines: [MonLineSpec]
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        HStack(spacing: 10) {
            ForEach(lines) { spec in
                HStack(spacing: 5) {
                    if spec.dashed {
                        HStack(spacing: 2) {
                            Rectangle().fill(spec.color).frame(width: 5, height: 2)
                            Rectangle().fill(spec.color).frame(width: 5, height: 2)
                        }
                    } else {
                        Rectangle().fill(spec.color).frame(width: 12, height: 2)
                    }
                    Text(verbatim: spec.label)
                        .lineLimit(1)
                }
            }
            Spacer(minLength: 0)
        }
        .font(Font.system(size: 11))
        .foregroundStyle(palette.ink2.color)
    }
}

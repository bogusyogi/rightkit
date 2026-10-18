import SwiftUI

/// One measurement. A nil value is a gap: the sampler ran and had nothing to report.
public struct HubSample: Sendable, Equatable {
    public let time: Date
    public let value: Double?

    public init(time: Date = Date(), value: Double?) {
        self.time = time
        self.value = value
    }
}

/// A bounded series of samples, newest last. Appending past `capacity` drops the oldest sample.
public struct HubSeries: Sendable, Equatable {
    /// The number of slots the series fills. At least 1.
    public let capacity: Int
    /// The samples held, oldest first. Never more than `capacity`.
    public private(set) var samples: [HubSample]

    public init(capacity: Int, samples: [HubSample] = []) {
        self.capacity = max(1, capacity)
        self.samples = Array(samples.suffix(max(1, capacity)))
    }

    /// Adds a sample, dropping the oldest when the series is full.
    public mutating func append(_ sample: HubSample) {
        samples.append(sample)
        if samples.count > capacity {
            samples.removeFirst(samples.count - capacity)
        }
    }

    /// Adds a value, or nil for a gap, stamped with `time`.
    public mutating func append(_ value: Double?, at time: Date = Date()) {
        append(HubSample(time: time, value: value))
    }

    /// The newest sample's value; nil when there is none or the newest is a gap.
    public var latest: Double? { samples.last?.value }

    /// Exactly `capacity` values, oldest first. Slots not yet filled are nil at the front.
    public func slotValues() -> [Double?] {
        let missing = capacity - samples.count
        return Array(repeating: nil, count: max(0, missing)) + samples.map { $0.value }
    }

    /// The values as at most `maxBars` bars. When the slots outnumber the bars, adjacent slots are
    /// averaged into buckets; a bucket with no values is a gap (nil).
    public func buckets(maxBars: Int) -> [Double?] {
        let slots = slotValues()
        let bars = min(capacity, max(1, maxBars))
        if bars == capacity { return slots }
        var out: [Double?] = []
        out.reserveCapacity(bars)
        for index in 0..<bars {
            let lower = index * capacity / bars
            let upper = (index + 1) * capacity / bars
            var total = 0.0
            var count = 0
            for slot in slots[lower..<upper] {
                if let value = slot {
                    total += value
                    count += 1
                }
            }
            out.append(count == 0 ? nil : total / Double(count))
        }
        return out
    }
}

/// A bar chart of a `HubSeries`, drawn with Rectangles in an HStack and sized by frame heights only
/// (no Path, Canvas or Swift Charts). Newest is on the right.
///
/// - A nil value is a gap, drawn as an empty slot of the same width.
/// - Below 1 pt per bar, adjacent samples are averaged into buckets; a bucket with no values is a gap.
/// - The accessibility value reads "no data" for gaps.
///
/// SwiftRight proves no way to measure a view, so the chart's size is given, not discovered.
public struct HubBarChart: View {
    private let series: HubSeries
    private let range: ClosedRange<Double>?
    private let label: String
    private let format: (Double) -> String
    private let width: CGFloat
    private let height: CGFloat
    private let showsCaption: Bool
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    /// - Parameters:
    ///   - series: The samples to draw.
    ///   - range: The value range the full bar height maps to. Nil uses 0 (or the lowest value if
    ///     negative) up to the highest value.
    ///   - label: What the chart shows, for the caption and for accessibility.
    ///   - format: Formats one value for the caption and the accessibility value.
    ///   - width: Drawn width in points.
    ///   - height: Drawn height of the bars in points.
    ///   - showsCaption: Draws the label and the latest value under the bars.
    public init(
        series: HubSeries,
        range: ClosedRange<Double>? = nil,
        label: String,
        format: @escaping (Double) -> String,
        width: CGFloat = 240,
        height: CGFloat = 48,
        showsCaption: Bool = true
    ) {
        self.series = series
        self.range = range
        self.label = label
        self.format = format
        self.width = width
        self.height = height
        self.showsCaption = showsCaption
    }

    private var palette: HubPalette { theme.palette(scheme) }

    /// One drawn slot. `fraction` is nil for a gap.
    struct Bar: Identifiable {
        let id: Int
        let fraction: Double?
    }

    /// The slots to draw: at least 1 pt wide each.
    func bars() -> [Bar] {
        let values = series.buckets(maxBars: Int(width.rounded(.down)))
        let present = values.compactMap { $0 }
        let low = range?.lowerBound ?? min(0, present.min() ?? 0)
        var high = range?.upperBound ?? (present.max() ?? 0)
        if high <= low { high = low + 1 }
        var out: [Bar] = []
        out.reserveCapacity(values.count)
        for (index, value) in values.enumerated() {
            if let value {
                out.append(Bar(id: index, fraction: min(1, max(0, (value - low) / (high - low)))))
            } else {
                out.append(Bar(id: index, fraction: nil))
            }
        }
        return out
    }

    /// The text a screen reader gets for a series: the latest, lowest and highest values, and how
    /// many samples are gaps. "no data" when nothing was measured.
    public static func accessibilityValue(for series: HubSeries, format: (Double) -> String) -> String {
        let present = series.samples.compactMap { $0.value }
        guard let low = present.min(), let high = present.max() else { return "no data" }
        let latest = series.latest.map(format) ?? "no data"
        var text = "latest \(latest), low \(format(low)), high \(format(high))"
        let gaps = series.samples.count - present.count
        if gaps > 0 { text += ", \(gaps) gaps with no data" }
        return text
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: theme.spacing.xs) {
            HStack(alignment: .bottom, spacing: 0) {
                ForEach(bars()) { bar in
                    slot(for: bar)
                }
            }
            .frame(width: width, height: height)
            if showsCaption {
                HStack {
                    Text(verbatim: label)
                        .font(theme.type.small.font)
                        .foregroundStyle(palette.ink2.color)
                        .lineLimit(1)
                    Spacer()
                    Text(verbatim: series.latest.map(format) ?? "no data")
                        .font(theme.type.small.font)
                        .foregroundStyle(palette.ink.color)
                        .lineLimit(1)
                }
                .frame(width: width)
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(label)
        .accessibilityValue(Self.accessibilityValue(for: series, format: format))
    }

    private func slot(for bar: Bar) -> some View {
        let barHeight: CGFloat
        if let fraction = bar.fraction {
            let raw = CGFloat(fraction) * height
            barHeight = fraction > 0 ? max(1, raw) : 0
        } else {
            barHeight = 0
        }
        let color = bar.fraction == nil ? Color.clear : palette.accent.color
        return VStack(spacing: 0) {
            Spacer(minLength: 0)
            Rectangle()
                .fill(color)
                .frame(height: barHeight)
        }
    }
}

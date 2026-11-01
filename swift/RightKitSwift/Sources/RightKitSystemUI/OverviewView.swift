#if os(macOS)
import Foundation
import RightKitHub
import SwiftUI

// Overview's view: port of the markup half of `hub/src/views/Overview.tsx` and its `.ov-*` CSS in
// `hub/src/styles.css`. A status strip, then a 2 x 3 grid of cards. The page scrolls as a whole in a
// window smaller than the default 900 x 600.

/// Sizes the page cannot measure: SwiftRight proves no way to read a view's size, so the sparklines
/// are drawn at the width the default window gives a card column (pane 679 pt, 18 pt page padding,
/// 10 pt gutter, 14 pt card padding, 18 pt column gap: 135 pt).
private enum OvMetrics {
    static let sparkWidth: CGFloat = 135
    static let sparkHeight: CGFloat = 22
    static let sparkTallHeight: CGFloat = 32
    static let lineLabelWidth: CGFloat = 124
    static let lineGap: CGFloat = 12
}

struct OverviewView: View {
    @State private var model: OverviewModel
    private let initialLoad: Bool
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme
    /// Opens another section; the shell sets it (`HubNavigateAction`).
    @Environment(\.hubNavigate) private var navigate

    init(model: OverviewModel, initialLoad: Bool) {
        _model = State(initialValue: model)
        self.initialLoad = initialLoad
    }

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        VStack(spacing: 0) {
            if let message = model.liveError {
                HubBanner("Live readings unavailable: \(message)")
            }
            ScrollView {
                VStack(alignment: .leading, spacing: 10) {
                    if model.permissionsMissing > 0 {
                        attention
                    }
                    hero
                    grid
                }
                .padding(.horizontal, 18)
                .padding(.top, 12)
                .padding(.bottom, 14)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .task { await model.run(initialLoad: initialLoad) }
        .task { await model.followNotch() }
    }

    // MARK: Status strip

    private var attention: some View {
        let missing = model.permissionsMissing
        return Button {
            navigate("permissions")
        } label: {
            HubLayered {
                RoundedRectangle(cornerRadius: 8).fill(palette.warnSoft.color)
                HStack(spacing: 8) {
                    Image(systemName: "checkmark.shield")
                    Text(verbatim: missing == 1 ? "1 permission needs approval" : "\(missing) permissions need approval")
                        // Served before the chevron, or an equal share cuts it (Fit.swift).
                        .layoutPriority(Fit.priority(windows: 1))
                    Spacer(minLength: 0)
                    Image(systemName: "chevron.right")
                }
                .font(Font.system(size: 12, weight: .semibold))
                .foregroundStyle(palette.warn.color)
                .padding(.horizontal, 10)
                .padding(.vertical, 8)
            }
        }
        .buttonStyle(.plain)
    }

    private var hero: some View {
        let headline = model.headline
        let tone = headline.level.tone
        let mark: Color = {
            switch headline.level {
            case .ok: return palette.okSoft.color
            case .warn: return palette.warnSoft.color
            case .bad: return palette.badSoft.color
            }
        }()
        return OvSurface(horizontal: 12, vertical: 8) {
            HStack(spacing: 12) {
                HubLayered {
                    RoundedRectangle(cornerRadius: 9).fill(mark)
                    Image(systemName: headline.level.symbol)
                        .font(Font.system(size: 18, weight: .regular))
                        .foregroundStyle(tone.color(in: palette).color)
                        .frame(width: 32, height: 32)
                }
                .frame(width: 32, height: 32)
                VStack(alignment: .leading, spacing: 1) {
                    Text(verbatim: headline.title)
                        .font(Font.system(size: 15, weight: .semibold))
                        .foregroundStyle(palette.ink.color)
                        .lineLimit(1)
                    Text(verbatim: headline.reason)
                        .font(theme.type.caption.font)
                        .foregroundStyle(palette.ink2.color)
                        .lineLimit(1)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                // The buttons keep their width on Windows' wider font; the text beside them gives way.
                HStack(spacing: 6) {
                    ForEach(headline.actions, id: \.label) { action in
                        OvActionButton(label: action.label, primary: action.label == headline.actions.first?.label) {
                            navigate(action.section)
                        }
                    }
                }
                .layoutPriority(2)
            }
            .frame(minHeight: 32)
        }
    }

    // MARK: Grid

    private var grid: some View {
        VStack(spacing: 10) {
            HStack(alignment: .top, spacing: 10) {
                performanceCard
                networkCard
            }
            HStack(alignment: .top, spacing: 10) {
                usageCard
                storageCard
            }
            HStack(alignment: .top, spacing: 10) {
                cleanupCard
                appsCard
            }
        }
    }

    private func card<Content: View>(
        symbol: String, title: String, go: String, section: String, @ViewBuilder content: () -> Content
    ) -> some View {
        OvCard(symbol: symbol, title: title, go: go, onOpen: { navigate(section) }, content: content)
    }

    private func spark(_ label: String, series: HubSeries, range: ClosedRange<Double>?, height: CGFloat, format: @escaping (Double) -> String) -> OvSpark {
        OvSpark(series: series, range: range, label: label, height: height, format: format)
    }

    // Performance: CPU and memory, each with a bar and the last ten minutes.
    private var performanceCard: some View {
        let cpu = model.cpu
        let memUsed = model.memUsed
        let memParts = MonFormat.splitBytes(memUsed)
        let hasTrend = model.recent.count > 1
        let percentFormat: (Double) -> String = { "\(Int($0.rounded()))%" }
        let memNote: String? = memUsed == nil
            ? nil
            : "of \(MonFormat.bytes(model.memTotal)) · \(MonFormat.percent(model.memFraction))\(model.pressure.map { " · \($0)" } ?? "")"
        return card(symbol: "gauge.with.dots.needle.33percent", title: "Performance", go: "Monitor", section: "monitor") {
            HStack(alignment: .top, spacing: 18) {
                OvMeter(
                    label: "CPU",
                    value: cpu.map { "\(Int($0.rounded()))" } ?? "—",
                    unit: cpu == nil ? nil : "%",
                    fraction: (cpu ?? 0) / 100,
                    level: model.cpuLevel,
                    measured: cpu != nil,
                    note: nil,
                    spark: hasTrend
                        ? spark("CPU", series: model.series { $0.cpu }, range: 0...100, height: OvMetrics.sparkHeight, format: percentFormat)
                        : nil)
                OvMeter(
                    label: "Memory",
                    value: memUsed == nil ? "—" : memParts.number,
                    unit: memUsed == nil ? nil : memParts.unit,
                    fraction: model.memFraction,
                    level: model.memLevel,
                    measured: memUsed != nil,
                    note: memNote,
                    spark: hasTrend
                        ? spark("Memory", series: model.series { $0.memPercent }, range: 0...100, height: OvMetrics.sparkHeight, format: percentFormat)
                        : nil)
            }
        }
    }

    // Network and swap: each direction's rate and trend, then swap in use.
    private var networkCard: some View {
        let swapText: String = {
            guard let total = model.swapTotal else { return "—" }
            return total == 0 ? "None" : MonFormat.bytes(model.swapUsed)
        }()
        let swapInUse = (model.swapTotal ?? 0) > 0
        let rate: (Double) -> String = { "\(MonFormat.bytes($0))/s" }
        func trend(_ label: String, _ pick: @escaping (MonSample) -> Double?) -> OvSpark? {
            guard model.recent.contains(where: { pick($0) != nil }) else { return nil }
            let series = model.series(pick)
            let top = max(100_000, series.samples.compactMap { $0.value }.max() ?? 0)
            return spark(label, series: series, range: 0...top, height: OvMetrics.sparkTallHeight, format: rate)
        }
        return card(symbol: "arrow.up.arrow.down", title: "Network & swap", go: "Monitor", section: "monitor") {
            if let network = model.network {
                HStack(alignment: .top, spacing: 18) {
                    OvFlow(label: "Down", symbol: "arrow.down", value: network.down, spark: trend("Network down") { $0.netDown })
                    OvFlow(label: "Up", symbol: "arrow.up", value: network.up, spark: trend("Network up") { $0.netUp })
                }
            } else {
                OvSub(text: "Network not available")
            }
            HStack(spacing: 10) {
                HStack(spacing: 4) {
                    Text(verbatim: "Swap")
                        .font(Font.system(size: 11.5))
                        .foregroundStyle(palette.ink2.color)
                    Text(verbatim: swapText)
                        .font(Font.system(size: 11.5, weight: .semibold))
                        .foregroundStyle(palette.ink.color)
                    if swapInUse {
                        Text(verbatim: "in use")
                            .font(Font.system(size: 11.5))
                            .foregroundStyle(palette.ink2.color)
                    }
                }
                Spacer(minLength: 0)
                if let kind = model.network?.kind {
                    Text(verbatim: kind)
                        .font(Font.system(size: 11.5))
                        .foregroundStyle(palette.ink2.color)
                }
            }
            .frame(minHeight: 22)
        }
    }

    // AI usage: the five-hour and weekly windows of each connected account.
    private var usageCard: some View {
        card(symbol: "person.crop.circle", title: "AI usage", go: "Accounts", section: "accounts") {
            if model.notchDown {
                OvSub(text: "Notch not running")
            } else if !model.notchRead {
                OvSub(text: "Reading…")
            } else if model.connectedAccounts.isEmpty {
                OvSub(text: "No accounts shown")
            } else {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(model.connectedAccounts) { account in
                        OvAccountUsage(account: account)
                    }
                }
            }
        }
    }

    // Storage: each local drive with its free space.
    private var storageCard: some View {
        card(symbol: "internaldrive", title: "Storage", go: "Storage", section: "storage") {
            if model.volumes == nil {
                OvSub(text: "Reading…")
            } else if model.drives.isEmpty {
                OvSub(text: "No drives found")
            } else {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(model.drives, id: \.mountPoint) { drive in
                        OvDriveLine(volume: drive)
                    }
                }
            }
        }
    }

    // Cleanup: what the last saved scan says can go. Nothing here starts a scan.
    private var cleanupCard: some View {
        let go: String = {
            if case .value = model.cleanup { return "Review" }
            return "Scan"
        }()
        return card(symbol: "sparkles", title: "Cleanup", go: go, section: "cleanup") {
            switch model.cleanup {
            case .reading:
                OvHeadlineLine(big: "Reading…", sub: nil, button: nil)
            case .missing:
                OvHeadlineLine(big: "Not scanned", sub: nil, button: OvButtonSpec(label: "Scan for clutter") { navigate("cleanup") })
                OvFootText(text: "Look for what can go")
            case .value(let report):
                OvHeadlineLine(big: MonFormat.bytes(model.canGo), sub: "can be cleared", button: nil)
                OvFootText(text: "Last scan \(MonFormat.ago(report.scannedAt))")
            }
        }
    }

    // Apps: the saved inventory and how many updates the last check found.
    private var appsCard: some View {
        let available = model.updatesAvailable
        return card(symbol: "square.grid.2x2", title: "Apps", go: "Apps", section: "apps") {
            switch model.apps {
            case .reading:
                OvHeadlineLine(big: "Reading…", sub: nil, button: nil)
            case .missing:
                OvHeadlineLine(big: "Not listed", sub: nil, button: nil)
                OvFootText(text: "Open Apps to list what is installed")
            case .value(let cached):
                if cached.savedAt == nil {
                    OvHeadlineLine(big: "Not listed", sub: nil, button: nil)
                    OvFootText(text: "Open Apps to list what is installed")
                } else {
                    OvHeadlineLine(
                        big: "\(model.installedCount) apps", sub: "\(MonFormat.bytes(model.installedBytes)) installed", button: nil)
                }
            }
            if !model.updates.isReading {
                HStack {
                    if available > 0 {
                        OvPill(text: "\(available) update\(available == 1 ? "" : "s") available") { navigate("apps") }
                    } else {
                        OvFootText(
                            text: model.updates.value?.checkedAt == nil ? "Updates not checked yet" : "No updates found")
                    }
                    Spacer(minLength: 0)
                }
                .frame(minHeight: 22)
            }
        }
    }
}

// MARK: - Pieces

/// The rounded, hairline-bordered panel the hero and the cards sit in.
private struct OvSurface<Content: View>: View {
    private let horizontal: CGFloat
    private let vertical: CGFloat
    private let content: Content
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    init(horizontal: CGFloat, vertical: CGFloat, @ViewBuilder content: () -> Content) {
        self.horizontal = horizontal
        self.vertical = vertical
        self.content = content()
    }

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        HubLayered {
            RoundedRectangle(cornerRadius: theme.radii.card).fill(palette.fill2.color)
            RoundedRectangle(cornerRadius: theme.radii.card).stroke(palette.hairline.color, lineWidth: 1)
            content
                .padding(.horizontal, horizontal)
                .padding(.vertical, vertical)
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        }
    }
}

/// One card: a header button that opens the section, then the body centred in the spare height.
private struct OvCard<Content: View>: View {
    private let symbol: String
    private let title: String
    private let go: String
    private let onOpen: @MainActor () -> Void
    private let content: Content
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    init(symbol: String, title: String, go: String, onOpen: @escaping @MainActor () -> Void, @ViewBuilder content: () -> Content) {
        self.symbol = symbol
        self.title = title
        self.go = go
        self.onOpen = onOpen
        self.content = content()
    }

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        OvSurface(horizontal: 14, vertical: 12) {
            VStack(alignment: .leading, spacing: 8) {
                Button(action: onOpen) {
                    HubLayered {
                        HubHitArea()
                        HStack(spacing: 7) {
                            Image(systemName: symbol)
                                .foregroundStyle(palette.accent.color)
                            Text(verbatim: title)
                                .font(Font.system(size: 13, weight: .semibold))
                                .foregroundStyle(palette.ink.color)
                                .lineLimit(1)
                                .layoutPriority(2)
                            Spacer(minLength: 0)
                            Text(verbatim: go)
                                .font(Font.system(size: 12))
                                .foregroundStyle(palette.ink2.color)
                                .lineLimit(1)
                                .layoutPriority(1)
                            Image(systemName: "chevron.right")
                                .font(Font.system(size: 11))
                                .foregroundStyle(palette.ink2.color)
                        }
                    }
                    .frame(height: 18)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("\(title), open \(go)")
                VStack(alignment: .leading, spacing: 8) {
                    Spacer(minLength: 0)
                    content
                    Spacer(minLength: 0)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)
            }
        }
        .frame(minHeight: 138, maxHeight: .infinity)
        .frame(maxWidth: .infinity)
    }
}

/// A sparkline's data: drawn as bars (a line chart waits for Path).
private struct OvSpark {
    let series: HubSeries
    let range: ClosedRange<Double>?
    let label: String
    let height: CGFloat
    let format: (Double) -> String
}

/// The slot under a bar or a rate: the trend when there is one, else the same height of nothing, so
/// both columns of a card keep the same height.
private struct OvTrendSlot: View {
    let spark: OvSpark?
    let height: CGFloat

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let spark {
                HubBarChart(
                    series: spark.series,
                    range: spark.range,
                    label: spark.label,
                    format: spark.format,
                    width: OvMetrics.sparkWidth,
                    height: spark.height,
                    showsCaption: false)
            } else {
                Color.clear.frame(height: height)
            }
        }
        .frame(maxWidth: .infinity, minHeight: height, maxHeight: height, alignment: .leading)
    }
}

/// A level mark: a check, a warning or a cross in the level's colour, with the word for a screen reader.
private struct OvLevelMark: View {
    let level: MonLevel
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        Image(systemName: level.symbol)
            .font(Font.system(size: 14))
            .foregroundStyle(level.tone.color(in: theme.palette(scheme)).color)
            .accessibilityLabel(level.word)
    }
}

/// A usage bar in the level's colour, never narrower than 1%, on the page colour (`.ov-bar`).
private struct OvBar: View {
    let fraction: Double
    let level: MonLevel
    let label: String
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        let f = MonFormat.clamp(fraction)
        let palette = theme.palette(scheme)
        StorageFractionBar(
            fraction: max(f, 0.01),
            color: level.tone.color(in: palette).color,
            height: 6,
            label: label,
            value: "\(Int((f * 100).rounded())) percent",
            track: palette.plane.color
        )
    }
}

/// One measurement: label and value on one line, a bar, the last minutes, a note.
private struct OvMeter: View {
    let label: String
    let value: String
    let unit: String?
    let fraction: Double
    let level: MonLevel
    let measured: Bool
    let note: String?
    let spark: OvSpark?
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    private var noteText: String { note ?? (measured ? level.word : "") }

    private var noteColor: Color {
        if note == nil && level != .ok { return level.tone.color(in: palette).color }
        return palette.ink2.color
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(verbatim: label)
                    .font(theme.type.caption.font)
                    .foregroundStyle(palette.ink2.color)
                    .lineLimit(1)
                Spacer(minLength: 0)
                HStack(alignment: .firstTextBaseline, spacing: 2) {
                    if measured {
                        OvLevelMark(level: level)
                            .padding(.trailing, 5)
                    }
                    Text(verbatim: value)
                        .font(Font.system(size: 18, weight: .semibold))
                        .foregroundStyle(palette.ink.color)
                        .lineLimit(1)
                    if let unit {
                        Text(verbatim: unit)
                            .font(Font.system(size: 12))
                            .foregroundStyle(palette.ink2.color)
                            .lineLimit(1)
                    }
                }
                // The value keeps its width; the label gives way first ("Mem…", never "1… GB").
                .layoutPriority(1)
            }
            OvBar(fraction: fraction, level: level, label: label)
            OvTrendSlot(spark: spark, height: OvMetrics.sparkHeight)
            Text(verbatim: noteText)
                .font(Font.system(size: 11.5))
                .foregroundStyle(noteColor)
                .lineLimit(1)
                .frame(minHeight: 16, alignment: .leading)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// One direction of network traffic: label and rate on one line, its recent trend below.
private struct OvFlow: View {
    let label: String
    let symbol: String
    let value: Double
    let spark: OvSpark?
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        let parts = MonFormat.splitBytes(value)
        VStack(alignment: .leading, spacing: 4) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                HStack(spacing: 4) {
                    Image(systemName: symbol)
                        .font(Font.system(size: 12))
                    Text(verbatim: label)
                        .font(theme.type.caption.font)
                        .lineLimit(1)
                }
                .foregroundStyle(palette.ink2.color)
                Spacer(minLength: 0)
                HStack(alignment: .firstTextBaseline, spacing: 2) {
                    Text(verbatim: parts.number)
                        .font(Font.system(size: 18, weight: .semibold))
                        .foregroundStyle(palette.ink.color)
                        .lineLimit(1)
                    Text(verbatim: "\(parts.unit)/s")
                        .font(Font.system(size: 12))
                        .foregroundStyle(palette.ink2.color)
                        .lineLimit(1)
                }
                // The rate keeps its width; the label gives way first.
                .layoutPriority(1)
            }
            OvTrendSlot(spark: spark, height: OvMetrics.sparkTallHeight)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// A plain secondary line of text in a card.
private struct OvSub: View {
    let text: String
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        Text(verbatim: text)
            .font(Font.system(size: 12))
            .foregroundStyle(theme.palette(scheme).ink2.color)
            .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct OvFootText: View {
    let text: String

    var body: some View {
        HStack {
            OvSub(text: text)
        }
        .frame(minHeight: 22)
    }
}

/// An account's five-hour and weekly windows, one line each.
private struct OvAccountUsage: View {
    let account: MonAccount
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        let rows = OvLogic.pickLimits(account.limits ?? [])
        VStack(alignment: .leading, spacing: 8) {
            if rows.isEmpty {
                OvSub(text: "\(account.name) · no readings yet")
            } else {
                ForEach(Array(rows.enumerated()), id: \.offset) { entry in
                    line(entry.element)
                }
            }
        }
    }

    private func line(_ row: OvLimitRow) -> some View {
        let fraction = MonFormat.clamp(row.fraction)
        let level = MonLevel.usage(fraction)
        let name = "\(account.name) · \(row.label)"
        return HStack(spacing: OvMetrics.lineGap) {
            Text(verbatim: name)
                .font(Font.system(size: 12))
                .foregroundStyle(palette.ink2.color)
                .lineLimit(1)
                .frame(width: OvMetrics.lineLabelWidth, alignment: .leading)
            OvBar(fraction: fraction, level: level, label: name)
            HStack(alignment: .firstTextBaseline, spacing: 2) {
                OvLevelMark(level: level)
                    .padding(.trailing, 4)
                Text(verbatim: "\(Int((fraction * 100).rounded()))")
                    .font(Font.system(size: 15, weight: .semibold))
                    .foregroundStyle(palette.ink.color)
                    .lineLimit(1)
                Text(verbatim: "%")
                    .font(Font.system(size: 12))
                    .foregroundStyle(palette.ink2.color)
                    .lineLimit(1)
            }
            // Wide enough for the check, "100" and "%" in Segoe UI (about 10% wider than SF): the bar
            // beside it is the flexible part and shrinks first.
            .frame(width: 72, alignment: .trailing)
        }
    }
}

/// A drive: name, bar and free space on one line, then how full it is under the bar.
private struct OvDriveLine: View {
    let volume: OvVolume
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        let used = volume.used
        let level = MonLevel.usage(used)
        let note = "of \(MonFormat.bytes(volume.totalBytes)) · \(MonFormat.percent(used)) used"
            + (level == .ok ? "" : " · \(level.word)")
            + (volume.reclaimableNote.map { " · \($0)" } ?? "")
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: OvMetrics.lineGap) {
                HStack(spacing: 5) {
                    OvLevelMark(level: level)
                    Text(verbatim: volume.name)
                        .font(Font.system(size: 13, weight: .semibold))
                        .foregroundStyle(palette.ink.color)
                        .lineLimit(1)
                }
                .frame(width: OvMetrics.lineLabelWidth, alignment: .leading)
                OvBar(fraction: used, level: level, label: "\(volume.name) used")
                HStack(alignment: .firstTextBaseline, spacing: 3) {
                    Text(verbatim: MonFormat.bytes(volume.availableBytes))
                        .font(Font.system(size: 15, weight: .semibold))
                        .foregroundStyle(palette.ink.color)
                        .lineLimit(1)
                    Text(verbatim: "free")
                        .font(Font.system(size: 12))
                        .foregroundStyle(palette.ink2.color)
                        .lineLimit(1)
                }
                // Room for "1000 GB free" in Segoe UI; the bar is the flexible part.
                .frame(width: 104, alignment: .trailing)
            }
            Text(verbatim: note)
                .font(Font.system(size: 12))
                .foregroundStyle(palette.ink2.color)
                .lineLimit(1)
                .padding(.leading, OvMetrics.lineLabelWidth + OvMetrics.lineGap)
        }
    }
}

struct OvButtonSpec {
    let label: String
    let action: @MainActor () -> Void
}

/// The big number, what it means, and optionally a button: Cleanup and Apps.
private struct OvHeadlineLine: View {
    let big: String
    let sub: String?
    let button: OvButtonSpec?
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        HStack(spacing: 12) {
            Text(verbatim: big)
                .font(Font.system(size: 24, weight: .semibold))
                .foregroundStyle(palette.ink.color)
                .lineLimit(1)
                .layoutPriority(2)
            Spacer(minLength: 0)
            if let sub {
                Text(verbatim: sub)
                    .font(Font.system(size: 12))
                    .foregroundStyle(palette.ink2.color)
                    .lineLimit(1)
            }
            if let button {
                OvActionButton(label: button.label, primary: true, action: button.action)
                    .layoutPriority(2)
            }
        }
        .frame(minHeight: 30)
    }
}

/// A hero or card button: filled with the accent when primary, else a hairline-bordered fill.
private struct OvActionButton: View {
    let label: String
    let primary: Bool
    let action: @MainActor () -> Void
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        Button {
            action()
        } label: {
            HubLayered {
                RoundedRectangle(cornerRadius: 7).fill(primary ? palette.accent.color : palette.fill2.color)
                RoundedRectangle(cornerRadius: 7).stroke(primary ? palette.accent.color : palette.hairline.color, lineWidth: 1)
                Text(verbatim: label)
                    .font(Font.system(size: 12, weight: .semibold))
                    .foregroundStyle(primary ? palette.accentInk.color : palette.ink.color)
                    .lineLimit(1)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 6)
            }
        }
        .buttonStyle(.plain)
    }
}

/// "3 updates available": a small button with an accent dot.
private struct OvPill: View {
    let text: String
    let action: @MainActor () -> Void
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        Button {
            action()
        } label: {
            HubLayered {
                RoundedRectangle(cornerRadius: 6).fill(palette.plane.color)
                HStack(spacing: 6) {
                    RoundedRectangle(cornerRadius: 3.5)
                        .fill(palette.accent.color)
                        .frame(width: 7, height: 7)
                    Text(verbatim: text)
                        .font(Font.system(size: 12))
                        .foregroundStyle(palette.ink.color)
                        .lineLimit(1)
                }
                .padding(.horizontal, 9)
                .padding(.vertical, 3)
            }
        }
        .buttonStyle(.plain)
    }
}
#endif

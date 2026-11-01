#if os(macOS)
import Foundation
import RightKitHub
import SwiftUI

// Monitor's view: port of the markup half of `hub/src/views/Monitor.tsx` and `monitor.css`. The page
// scrolls as a whole: a memory-pressure notice, 30 minutes of history as bar charts, the Windows
// sensors, then the process table with Quit, Force Quit and Inspect.

/// Sizes the page cannot measure: SwiftRight proves no way to read a view's size, so charts and
/// columns are drawn at the widths the default 900 x 600 window gives them (pane 679 pt, 18 pt page
/// padding: 643 pt; three chart cards with 10 pt gutters and 12 pt padding: about 207 pt each). The columns are
/// the page's (`monitor.css`): CPU 56, Memory 84, Threads 64, Started 76, with a 12 pt gap.
private enum MonMetrics {
    static let historyWidth: CGFloat = 300
    static let gap: CGFloat = 10
    static let columnGap: CGFloat = 12
    static let cpuWidth: CGFloat = 56
    static let memoryWidth: CGFloat = 84
    static let threadsWidth: CGFloat = 64
    static let startedWidth: CGFloat = 76
}

struct MonitorView: View {
    @State private var model: MonitorModel
    private let initialLoad: Bool
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    init(model: MonitorModel, initialLoad: Bool) {
        _model = State(initialValue: model)
        self.initialLoad = initialLoad
    }

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        VStack(spacing: 0) {
            if model.status == nil {
                if let error = model.loadError {
                    HubErrorState(title: "Could not read the system", message: error) {
                        Task { await model.refreshProcesses() }
                    }
                } else {
                    HubLoadingState(message: "Reading…")
                }
            } else {
                if let error = model.loadError {
                    HubBanner("Live readings unavailable: \(error)")
                }
                page
            }
        }
        .task { await model.runProcesses(initialLoad: initialLoad) }
        .task { await model.runMetrics(initialLoad: initialLoad) }
        .task { await model.followNotch() }
        .task { await model.watchSensors() }
        .hubConfirm(
            "Force Quit \(model.pendingForce?.name ?? "")?",
            message: "This ends it immediately without letting it save. Unsaved work in it will be lost.",
            confirmLabel: "Force Quit",
            isPresented: Binding(get: { model.confirmForce }, set: { model.confirmForce = $0 })
        ) {
            Task { await model.forceConfirmed() }
        }
    }

    private var page: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 10) {
                notice
                charts
                if MonPlatform.isWindows || model.sensorCount > 0 {
                    sensors
                }
                toolbar
                sectionHeader
                list
                caption
            }
            .padding(.horizontal, 18)
            .padding(.vertical, 14)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    // MARK: Notice

    /// Memory pressure: only an elevated or critical reading earns a box; normal and unknown stay
    /// one calm line.
    private var notice: some View {
        let pressure = model.pressureText
        let level = pressure.map { MonLevel.pressure($0) } ?? .ok
        let calm = level == .ok
        let leaderText: String = {
            guard let leader = model.leader else { return "No process rows reported." }
            let detail = model.sort == .cpu
                ? "\(Int(leader.cpuUsagePercent.rounded()))% CPU"
                : "\(MonFormat.bytes(leader.memoryBytes)) memory"
            return "\(leader.name) · \(detail)"
        }()
        let heading = HStack(spacing: 4) {
            Text(verbatim: "Memory pressure:")
                .foregroundStyle(palette.ink.color)
            Text(verbatim: pressure ?? "not reported")
                .foregroundStyle(pressure == nil ? palette.ink.color : level.tone.color(in: palette).color)
        }
        .font(Font.system(size: calm ? 13 : 15, weight: .semibold))
        return VStack(alignment: .leading, spacing: 2) {
            if calm {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    heading
                    Text(verbatim: leaderText)
                        .font(Font.system(size: 12))
                        .foregroundStyle(palette.ink2.color)
                        .lineLimit(1)
                }
                .padding(.horizontal, 2)
                .padding(.vertical, 4)
            } else {
                HubLayered {
                    RoundedRectangle(cornerRadius: theme.radii.card).fill(palette.panel.color)
                    RoundedRectangle(cornerRadius: theme.radii.card)
                        .stroke(level.tone.color(in: palette).color, lineWidth: 1)
                    VStack(alignment: .leading, spacing: 2) {
                        heading
                        Text(verbatim: leaderText)
                            .font(Font.system(size: 12))
                            .foregroundStyle(palette.ink2.color)
                    }
                    .padding(.horizontal, 12)
                    .padding(.vertical, 10)
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
        }
    }

    // MARK: History

    private var charts: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 12) {
                Text(verbatim: "History, last \(model.minutes) minutes")
                    .font(Font.system(size: 15, weight: .semibold))
                    .foregroundStyle(palette.ink.color)
                    .lineLimit(1)
                    .layoutPriority(1)
                Spacer(minLength: 0)
                MonSegmented(
                    options: [5, 15, 30].map { MonOption(id: String($0), label: "\($0) min") },
                    selectedID: String(model.minutes)
                ) { id in
                    if let minutes = Int(id) { model.minutes = minutes }
                }
            }
            if let window = model.chartWindow(minutes: model.minutes) {
                chartGrid(window)
            } else {
                Text(verbatim: "Collecting readings…")
                    .font(Font.system(size: 12))
                    .foregroundStyle(palette.ink2.color)
                    .padding(.vertical, 8)
            }
        }
    }

    private func chartGrid(_ window: MonWindow) -> some View {
        let latest = model.latest
        let status = model.status
        let minutes = model.minutes
        let count = MonChartMetrics.bins
        let percent: (Double) -> String = { "\(Int($0.rounded()))%" }
        let rate: (Double) -> String = { "\(MonFormat.bytes($0))/s" }
        let size: (Double) -> String = { MonFormat.bytes($0) }

        let cpuNow = status?.cpu ?? latest?.cpu
        let swapTotal = status?.swapTotal ?? latest?.swapTotal
        let swapUsed = status?.swapUsed ?? latest?.swapUsed
        let swapText: String = {
            guard let swapTotal else { return "Swap unknown" }
            if swapTotal == 0 { return "No swap space" }
            return "Swap \(MonFormat.bytes(swapUsed)) of \(MonFormat.bytes(swapTotal))"
        }()
        let net = model.system?.network
        let downNow: String = latest?.netDown.map { "↓ \(rate($0))" } ?? net.map { "↓ \(rate($0.down))" } ?? "—"
        let upText: String = {
            if let up = latest?.netUp { return "↑ \(rate(up))" }
            if let net { return "↑ \(rate(net.up))\(net.kind.map { " · \($0)" } ?? "")" }
            return model.notchDown ? "Notch not running" : "No active interface"
        }()

        let cpuValues = window.bins(count: count) { $0.cpu }
        let memValues = window.bins(count: count) { $0.memUsed }
        let swapValues = window.bins(count: count) { $0.swapUsed }
        let downValues = window.bins(count: count) { $0.netDown }
        let upValues = window.bins(count: count) { $0.netUp }

        let cpuColor = MonChartRules.level(latest?.cpu ?? 0).tone.color(in: palette).color
        let memColor = MonChartRules.level(latest?.memPercent ?? 0).tone.color(in: palette).color
        let cpuLines = [MonLineSpec(id: 0, label: "CPU", color: cpuColor, values: cpuValues, dashed: false)]
        let memLines = [
            MonLineSpec(id: 0, label: "Memory", color: memColor, values: memValues, dashed: false),
            MonLineSpec(id: 1, label: "Swap", color: palette.ink2.color, values: swapValues, dashed: true),
        ]
        let netLines = [
            MonLineSpec(id: 0, label: "Down", color: palette.accent.color, values: downValues, dashed: false),
            MonLineSpec(id: 1, label: "Up", color: palette.ink2.color, values: upValues, dashed: true),
        ]

        let memTotal = latest?.memTotal ?? 0
        let memTop = memTotal > 0
            ? memTotal
            : MonChartRules.niceMax((memValues + swapValues).compactMap { $0 }.max() ?? 0)
        let netTop = MonChartRules.niceMax(
            max(100_000, (downValues + upValues).compactMap { $0 }.max() ?? 0))
        let leftLabel = window.short ? "since \(MonChartRules.timeText(window.start))" : "\(minutes) min ago"

        // One symmetric row of three equal, compact cards, so the process table stays in view (#415).
        return HStack(alignment: .top, spacing: MonMetrics.gap) {
            chartCard(
                title: "System CPU", now: cpuNow.map { "\(Int($0.rounded()))%" } ?? "—",
                extra: "All cores, 0–100%", legend: nil
            ) {
                MonTimeChart(
                    lines: cpuLines, top: 100, topLabel: percent(100), bottomLabel: percent(0),
                    summary: MonChartRules.summary("CPU", lines: cpuLines, minutes: minutes, format: percent),
                    leftLabel: leftLabel)
            }
            chartCard(
                title: "Memory used",
                now: latest.map { "\(MonFormat.bytes($0.memUsed)) / \(MonFormat.bytes($0.memTotal))" } ?? "—",
                extra: swapText, legend: memLines
            ) {
                MonTimeChart(
                    lines: memLines, top: memTop, topLabel: size(memTop), bottomLabel: size(0),
                    summary: MonChartRules.summary("Memory", lines: memLines, minutes: minutes, format: size),
                    leftLabel: leftLabel)
            }
            chartCard(title: "Network", now: downNow, extra: upText, legend: netLines) {
                MonTimeChart(
                    lines: netLines, top: netTop, topLabel: rate(netTop), bottomLabel: rate(0),
                    summary: MonChartRules.summary("Network", lines: netLines, minutes: minutes, format: rate),
                    leftLabel: leftLabel)
            }
        }
    }

    /// A chart card (`.monitor-chart-card`): the title, the latest reading and a note on one line, the
    /// chart, then its key.
    private func chartCard<Content: View>(
        title: String, now: String, extra: String?, legend: [MonLineSpec]?, @ViewBuilder content: () -> Content
    ) -> some View {
        HubLayered {
            RoundedRectangle(cornerRadius: theme.radii.card).fill(palette.panel.color)
            RoundedRectangle(cornerRadius: theme.radii.card).stroke(palette.hairline.color, lineWidth: 1)
            VStack(alignment: .leading, spacing: 6) {
                HStack(alignment: .firstTextBaseline, spacing: 10) {
                    Text(verbatim: title)
                        .font(Font.system(size: 12))
                        .foregroundStyle(palette.ink2.color)
                        .lineLimit(1)
                        .layoutPriority(2)
                    Spacer(minLength: 0)
                    Text(verbatim: now)
                        .font(Font.system(size: 12, weight: .semibold))
                        .foregroundStyle(palette.ink.color)
                        .lineLimit(1)
                        .layoutPriority(3)
                    if let extra {
                        Spacer(minLength: 0)
                        Text(verbatim: extra)
                            .font(Font.system(size: 12))
                            .foregroundStyle(palette.ink2.color)
                            .lineLimit(1)
                            // The note may truncate; the reading before it may not (Fit.swift).
                            .layoutPriority(Fit.priority(windows: 4))
                    }
                }
                content()
                if let legend {
                    MonLegend(lines: legend)
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 10)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .frame(maxWidth: .infinity, alignment: .topLeading)
    }

    // MARK: Sensors

    private var sensors: some View {
        let system = model.system
        let count = model.sensorCount
        let fans = system?.fans?.filter { $0.rpm != nil || $0.percent != nil }.count ?? 0
        let temperatures = system?.temperatures?.count ?? 0
        let summary: String = {
            if count == 0 { return "not available on this PC" }
            var parts: [String] = []
            if fans > 0 { parts.append("\(fans) fan\(fans == 1 ? "" : "s")") }
            if temperatures > 0 { parts.append("\(temperatures) temperature\(temperatures == 1 ? "" : "s")") }
            if system?.battery != nil { parts.append("battery") }
            return parts.joined(separator: " · ")
        }()
        let rows = sensorRows()
        return VStack(alignment: .leading, spacing: 4) {
            Button {
                model.sensorsOpen.toggle()
            } label: {
                HubLayered {
                    HubHitArea()
                    HStack(spacing: 6) {
                        Image(systemName: model.sensorsOpen ? "chevron.down" : "chevron.right")
                            .font(Font.system(size: 11))
                            .foregroundStyle(palette.ink3.color)
                            .frame(width: 14)
                        Text(verbatim: "Sensors")
                            .font(theme.type.body.font)
                            .foregroundStyle(palette.ink.color)
                        Text(verbatim: summary)
                            .font(Font.system(size: 11.5))
                            .foregroundStyle(palette.ink3.color)
                            .lineLimit(1)
                        Spacer(minLength: 0)
                    }
                }
                .frame(height: 28)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Sensors \(summary)")
            .accessibilityValue(model.sensorsOpen ? "expanded" : "collapsed")
            if model.sensorsOpen {
                ScrollView {
                    VStack(spacing: 0) {
                        ForEach(rows) { row in
                            HStack(alignment: .firstTextBaseline, spacing: 12) {
                                Text(verbatim: row.label)
                                    .font(Font.system(size: 12))
                                    .foregroundStyle(palette.ink.color)
                                    .lineLimit(1)
                                Spacer(minLength: 0)
                                Text(verbatim: row.value)
                                    .font(Font.system(size: 12))
                                    .foregroundStyle(palette.ink.color)
                                    .lineLimit(1)
                                    .layoutPriority(1)
                            }
                            .frame(height: 21)
                        }
                    }
                    .padding(.leading, 26)
                    .padding(.trailing, 4)
                }
                .frame(height: min(180, CGFloat(rows.count) * 21 + 6))
            }
        }
    }

    private func sensorRows() -> [MonSensorRow] {
        let system = model.system
        var rows: [(String, String)] = []
        if model.sensorCount == 0 { rows.append(("Sensors", "Not available on this PC")) }
        if let battery = system?.battery { rows.append(("Battery", MonRules.batteryText(battery))) }
        let temperatures = system?.temperatures ?? []
        if MonPlatform.isWindows && model.sensorCount > 0
            && !temperatures.contains(where: { MonSensorRow.isCpuTemperature($0.name) }) {
            rows.append(("CPU temperature", "Not available on this PC"))
        }
        for fan in system?.fans ?? [] {
            if let rpm = fan.rpm {
                rows.append((fan.name, "\(Int(rpm.rounded())) rpm"))
            } else if let percent = fan.percent {
                rows.append((fan.name, "\(Int(percent.rounded()))%"))
            }
        }
        for temperature in temperatures.sorted(by: { $0.name < $1.name }) {
            rows.append((temperature.name, "\(String(format: "%.1f", temperature.celsius)) °C"))
        }
        return rows.enumerated().map { MonSensorRow(id: $0.offset, label: $0.element.0, value: $0.element.1) }
    }

    // MARK: Table

    private var toolbar: some View {
        HStack(spacing: 8) {
            searchField
                .frame(maxWidth: .infinity)
            MonSegmented(
                options: MonSortKey.allCases.map { MonOption(id: $0.rawValue, label: $0.label) },
                selectedID: model.sort.rawValue
            ) { id in
                if let key = MonSortKey(rawValue: id) { model.chooseSort(key, fromHeader: false) }
            }
        }
    }

    /// The page's search box: a plain field with a hairline outline and no magnifier.
    private var searchField: some View {
        HubLayered {
            RoundedRectangle(cornerRadius: 7).fill(palette.fill2.color)
            RoundedRectangle(cornerRadius: 7).stroke(palette.hairline.color, lineWidth: 1)
            TextField("Search apps and processes", text: Binding(get: { model.query }, set: { model.query = $0 }))
                .textFieldStyle(.plain)
                .font(theme.type.body.font)
                .foregroundStyle(palette.ink.color)
                .accessibilityLabel("Search apps and processes")
                .padding(.horizontal, 10)
                .frame(height: 30)
        }
    }

    private var sectionHeader: some View {
        let trimmed = model.trimmedQuery
        let visibleCount = model.visible.count
        let text = trimmed.isEmpty
            ? "Apps and processes, sorted by \(model.sort.words), \(model.orderText)"
            : "\(visibleCount) matching “\(trimmed)”"
        return HStack(spacing: 12) {
            Text(verbatim: "Top consumers")
                .font(Font.system(size: 15, weight: .semibold))
                .foregroundStyle(palette.ink.color)
                .lineLimit(1)
                .layoutPriority(2)
            Spacer(minLength: 0)
            Text(verbatim: text)
                .font(Font.system(size: 11))
                .foregroundStyle(palette.ink3.color)
                .lineLimit(1)
            Spacer(minLength: 0)
            HStack(spacing: 6) {
                RoundedRectangle(cornerRadius: 3.5)
                    .fill(palette.warn.color)
                    .frame(width: 7, height: 7)
                Text(verbatim: "Above 90% CPU, or top 3 by memory")
                    .font(Font.system(size: 11))
                    .foregroundStyle(palette.ink3.color)
                    .lineLimit(1)
            }
        }
    }

    private var list: some View {
        let rows = model.visible
        let shown = model.showAll ? rows : Array(rows.prefix(MonitorModel.rowLimit))
        let top = model.topMemory
        let now = Date().timeIntervalSince1970
        return VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: MonMetrics.columnGap) {
                sortHead("Name", .name, end: false)
                    .frame(maxWidth: .infinity, alignment: .leading)
                sortHead("CPU %", .cpu, end: true)
                    .frame(width: MonMetrics.cpuWidth, alignment: .trailing)
                sortHead("Memory", .memory, end: true)
                    .frame(width: MonMetrics.memoryWidth, alignment: .trailing)
                sortHead("Threads", .threads, end: true)
                    .frame(width: MonMetrics.threadsWidth, alignment: .trailing)
                sortHead("Started", .started, end: true)
                    .frame(width: MonMetrics.startedWidth, alignment: .trailing)
            }
            .padding(.horizontal, 4)
            .padding(.vertical, 2)
            Divider()
            if rows.isEmpty {
                Text(verbatim: model.trimmedQuery.isEmpty
                    ? "No apps or processes to show yet."
                    : "No apps or processes match “\(model.trimmedQuery)”.")
                    .font(theme.type.body.font)
                    .foregroundStyle(palette.ink2.color)
                    .padding(.horizontal, 4)
                    .padding(.vertical, 16)
            }
            ForEach(shown) { process in
                MonRowView(model: model, process: process, isTop: top.contains(process.key), now: now)
            }
            if rows.count > shown.count {
                HStack(spacing: 10) {
                    Text(verbatim: "Showing the first \(shown.count) of \(rows.count). Search to narrow the list.")
                        .font(Font.system(size: 12))
                        .foregroundStyle(palette.ink2.color)
                    MonButton(label: "Show all \(rows.count)", style: .secondary) { model.showAll = true }
                    Spacer(minLength: 0)
                }
                .padding(.horizontal, 4)
                .padding(.vertical, 10)
            }
        }
    }

    /// One sortable column header: the arrow marks the current order.
    private func sortHead(_ label: String, _ column: MonSortKey, end: Bool) -> some View {
        let active = model.sort == column
        return Button {
            model.chooseSort(column, fromHeader: true)
        } label: {
            HStack(spacing: 3) {
                Text(verbatim: label)
                if active {
                    Image(systemName: model.dir == .asc ? "arrow.up" : "arrow.down")
                        .font(Font.system(size: 10))
                }
            }
            .font(Font.system(size: 11, weight: active ? .semibold : .regular))
            .foregroundStyle(active ? palette.ink.color : palette.ink2.color)
            .lineLimit(1)
            .padding(.vertical, 4)
        }
        .buttonStyle(.plain)
        .accessibilityLabel(label)
        .accessibilityValue(active ? (model.dir == .asc ? "ascending" : "descending") : "not sorted")
    }

    private var caption: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(verbatim: "CPU: 100% = one core. System CPU above is 0–100% across all cores.")
            Text(verbatim: MonPlatform.isWindows
                ? "Memory: what the process is using in RAM (working set)."
                : "Memory: what the process is responsible for, including compressed and swapped memory.")
        }
        .font(Font.system(size: 11.5))
        .foregroundStyle(palette.ink3.color)
    }
}

// MARK: - Process rows

private struct MonRowView: View {
    let model: MonitorModel
    let process: MonProcess
    let isTop: Bool
    let now: Double
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    private var reasons: [String] {
        var out: [String] = []
        if process.cpuUsagePercent > 90 { out.append("Above 90% CPU") }
        if isTop { out.append("Top 3 by memory") }
        return out
    }

    private func numberText(_ text: String, width: CGFloat, muted: Bool = false) -> some View {
        Text(verbatim: text)
            .font(Font.system(size: 12.5))
            .foregroundStyle(muted ? palette.ink3.color : palette.ink2.color)
            .lineLimit(1)
            .frame(width: width, alignment: .trailing)
    }

    var body: some View {
        let p = process
        let system = MonRules.isSystem(p)
        let isOpen = model.expanded == p.key
        let hasMembers = p.members.count > 1
        let busy = model.busy == p.key
        let isStuck = model.stuck.contains(p.key)
        let inspecting = model.inspect?.row.key == p.key
        let meta = "\(p.members.count) process\(p.members.count == 1 ? "" : "es") · \(system ? "Protected system process" : "Running")"
        return VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: MonMetrics.columnGap) {
                HStack(spacing: 6) {
                    if !reasons.isEmpty {
                        RoundedRectangle(cornerRadius: 3.5)
                            .fill(palette.warn.color)
                            .frame(width: 7, height: 7)
                            .accessibilityLabel(reasons.joined(separator: ", "))
                    }
                    if hasMembers {
                        Button {
                            model.expanded = isOpen ? nil : p.key
                        } label: {
                            HubLayered {
                                HubHitArea()
                                HStack(spacing: 6) {
                                    Image(systemName: isOpen ? "chevron.down" : "chevron.right")
                                        .font(Font.system(size: 11))
                                        .foregroundStyle(palette.ink3.color)
                                        .frame(width: 12)
                                    Text(verbatim: p.name)
                                        .font(theme.type.body.font)
                                        .foregroundStyle(palette.ink.color)
                                        .lineLimit(1)
                                }
                            }
                            .frame(height: 24)
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel(p.name)
                        .accessibilityValue(isOpen ? "expanded" : "collapsed")
                        .layoutPriority(2)
                    } else {
                        Text(verbatim: p.name)
                            .font(theme.type.body.font)
                            .foregroundStyle(palette.ink.color)
                            .lineLimit(1)
                            .layoutPriority(2)
                    }
                    if system {
                        // The page's pill is the plane colour on the plane: only its text shows.
                        Text(verbatim: "System")
                            .font(Font.system(size: 11))
                            .foregroundStyle(palette.ink2.color)
                            .lineLimit(1)
                            .padding(.horizontal, 7)
                            .padding(.vertical, 1)
                            .layoutPriority(2)
                    }
                    Text(verbatim: meta)
                        .font(Font.system(size: 11.5))
                        .foregroundStyle(palette.ink3.color)
                        .lineLimit(1)
                    Spacer(minLength: 0)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                numberText("\(Int(p.cpuUsagePercent.rounded()))%", width: MonMetrics.cpuWidth)
                numberText(MonFormat.bytes(p.memoryBytes), width: MonMetrics.memoryWidth)
                numberText(MonRules.threadsText(p.threads), width: MonMetrics.threadsWidth, muted: p.threads == nil)
                numberText(
                    MonRules.age(startedAt: p.startedAt, now: now), width: MonMetrics.startedWidth,
                    muted: p.startedAt == nil)
            }
            .padding(.horizontal, 4)
            .padding(.top, 6)
            HStack(spacing: 4) {
                Spacer(minLength: 0)
                MonButton(label: "Reveal", style: .ghost, enabled: p.executable != nil) {
                    Task { await model.reveal(p) }
                }
                MonButton(
                    label: "Inspect", style: .ghost, accessibilityLabel: "Inspect \(p.name)",
                    selected: inspecting
                ) {
                    model.toggleInspect(p)
                }
                if !system && p.canAct {
                    MonButton(label: "Quit", style: .secondary, enabled: !busy) {
                        Task { await model.quit(p) }
                    }
                    MonButton(label: "Force Quit", style: isStuck ? .danger : .ghostDanger, enabled: !busy) {
                        model.askForce(p)
                    }
                }
            }
            .padding(.horizontal, 4)
            .padding(.top, 4)
            .padding(.bottom, 6)
            if let note = model.notes[p.key] {
                Text(verbatim: note)
                    .font(Font.system(size: 11.5))
                    .foregroundStyle(palette.ink2.color)
                    .lineLimit(1)
                    .padding(.leading, 22)
                    .padding(.trailing, 4)
                    .padding(.bottom, 6)
            }
            if inspecting, let state = model.inspect {
                MonInspectPanel(model: model, state: state, samples: model.procHistory[p.historyKey] ?? [])
                    .padding(.bottom, 6)
            }
            if isOpen && hasMembers {
                VStack(spacing: 0) {
                    ForEach(p.members, id: \.identity.pid) { member in
                        HStack(spacing: MonMetrics.columnGap) {
                            Text(verbatim: member.name)
                                .font(Font.system(size: 12))
                                .foregroundStyle(palette.ink2.color)
                                .lineLimit(1)
                                .frame(maxWidth: .infinity, alignment: .leading)
                            numberText("\(Int(member.cpuUsagePercent.rounded()))%", width: MonMetrics.cpuWidth, muted: true)
                            numberText(MonFormat.bytes(member.memoryBytes), width: MonMetrics.memoryWidth)
                            numberText(MonRules.threadsText(member.threads), width: MonMetrics.threadsWidth, muted: true)
                            numberText(
                                MonRules.age(startedAt: member.startedAt, now: now), width: MonMetrics.startedWidth,
                                muted: true)
                        }
                        .padding(.leading, 26)
                        .padding(.trailing, 4)
                        .padding(.vertical, 3)
                    }
                }
                .padding(.bottom, 4)
            }
            Divider()
        }
    }
}

/// The Inspect panel under a row: listening ports and open files, read when asked and never
/// refreshed alone, then this page's own history of the process.
private struct MonInspectPanel: View {
    let model: MonitorModel
    let state: MonInspectState
    let samples: [MonProcSample]
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    private var unknownText: String {
        state.result == nil ? (state.loading ? "Reading…" : "Unknown") : "Unknown. The note below says why."
    }

    private func small(_ text: String) -> some View {
        Text(verbatim: text)
            .font(Font.system(size: 11.5))
            .foregroundStyle(palette.ink2.color)
    }

    private func heading(_ text: String) -> some View {
        Text(verbatim: text)
            .font(Font.system(size: 12, weight: .semibold))
            .foregroundStyle(palette.ink2.color)
    }

    var body: some View {
        let row = state.row
        let ports = state.result?.listening
        let files = state.result?.openFiles
        let subtitle = "Read when you asked. Not refreshed on its own."
            + (row.residentBytes.map { " · In RAM now: \(MonFormat.bytes($0))" } ?? "")
        return HubCard(padding: 10) {
            VStack(alignment: .leading, spacing: 8) {
                HStack(alignment: .top, spacing: 12) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(verbatim: row.name)
                            .font(Font.system(size: 13, weight: .semibold))
                            .foregroundStyle(palette.ink.color)
                            .lineLimit(1)
                        small(subtitle)
                    }
                    Spacer(minLength: 0)
                    MonButton(label: "Refresh", style: .secondary, enabled: !state.loading) {
                        model.refreshInspect()
                    }
                    MonButton(label: "Close", style: .ghost) {
                        model.closeInspect()
                    }
                    .keyboardShortcut(.cancelAction)
                }
                if let error = state.error {
                    Text(verbatim: error)
                        .font(Font.system(size: 11.5))
                        .foregroundStyle(palette.bad.color)
                }
                HStack(alignment: .top, spacing: 14) {
                    VStack(alignment: .leading, spacing: 4) {
                        heading("Listening ports")
                        if let ports {
                            if ports.isEmpty {
                                small("No listening ports.")
                            } else {
                                portList(ports)
                            }
                        } else {
                            small(unknownText)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .topLeading)
                    VStack(alignment: .leading, spacing: 4) {
                        heading("Open files")
                        if let files {
                            if files.isEmpty {
                                small("No open files listed.")
                            } else {
                                fileList(files)
                            }
                        } else {
                            small(unknownText)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .topLeading)
                }
                VStack(alignment: .leading, spacing: 4) {
                    heading("Network hosts")
                    if let connections = state.result?.connections {
                        if connections.isEmpty {
                            small("No established connections.")
                        } else {
                            connectionList(connections)
                        }
                    } else {
                        small(unknownText)
                    }
                }
                small("GPU: Unavailable. macOS has no public API that reports GPU use for one process, so Pulse shows no per-process GPU figure.")
                history
                if let notes = state.result?.notes, !notes.isEmpty {
                    VStack(alignment: .leading, spacing: 2) {
                        ForEach(Array(notes.enumerated()), id: \.offset) { entry in
                            small("• \(entry.element)")
                        }
                    }
                }
            }
        }
    }

    /// Rows are 16 pt with 2 pt between, so the list's height follows its length up to 220 pt and
    /// scrolls past that.
    private func listHeight(_ count: Int) -> CGFloat { min(220, CGFloat(count) * 18) }

    private func portList(_ ports: [MonPort]) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 2) {
                ForEach(Array(ports.enumerated()), id: \.offset) { entry in
                    HStack(spacing: 8) {
                        Text(verbatim: String(entry.element.port))
                            .foregroundStyle(palette.ink2.color)
                            .frame(width: 56, alignment: .trailing)
                        Text(verbatim: entry.element.proto.uppercased())
                            .foregroundStyle(palette.ink.color)
                            .frame(width: 48, alignment: .leading)
                        Text(verbatim: entry.element.family)
                            .foregroundStyle(palette.ink2.color)
                            .lineLimit(1)
                        Spacer(minLength: 0)
                    }
                    .font(Font.system(size: 12))
                    .frame(height: 16)
                }
            }
        }
        .frame(height: listHeight(ports.count))
        .accessibilityLabel("Listening ports")
    }

    /// Remote ends as the system reports them: addresses, not host names (nothing is looked up).
    private func connectionList(_ connections: [MonConnection]) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 2) {
                ForEach(Array(connections.enumerated()), id: \.offset) { entry in
                    HStack(spacing: 8) {
                        Text(verbatim: entry.element.remoteAddress)
                            .foregroundStyle(palette.ink.color)
                            .lineLimit(1)
                        Text(verbatim: String(entry.element.remotePort))
                            .foregroundStyle(palette.ink2.color)
                        Text(verbatim: entry.element.proto.uppercased())
                            .foregroundStyle(palette.ink2.color)
                        Spacer(minLength: 0)
                    }
                    .font(Font.system(size: 12))
                    .frame(height: 16)
                }
            }
        }
        .frame(height: listHeight(connections.count))
        .accessibilityLabel("Network hosts")
    }

    private func fileList(_ files: [String]) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 2) {
                ForEach(Array(files.enumerated()), id: \.offset) { entry in
                    Text(verbatim: MonRules.middleTruncate(entry.element))
                        .font(Font.system(size: 12))
                        .foregroundStyle(palette.ink.color)
                        .lineLimit(1)
                        .frame(height: 16, alignment: .leading)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .frame(height: listHeight(files.count))
        .accessibilityLabel("Open files")
    }

    /// CPU, memory and disk I/O bars for the process, from this page's polls. Nothing is kept after the page closes.
    private var history: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                heading("History")
                Spacer(minLength: 0)
                small("History since this page was opened")
            }
            if samples.count < 2 {
                small("Collecting…")
            } else {
                HStack(alignment: .top, spacing: 14) {
                    historyCell("CPU", values: samples.map { $0.cpu }) { "\(Int($0.rounded()))%" }
                    historyCell("Memory", values: samples.map { $0.memory }) { MonFormat.bytes($0) }
                }
                // No line until two readings have a rate; an older engine sends no disk_io, so none is drawn.
                let disk = samples.compactMap { $0.disk }
                if disk.count >= 2 {
                    historyCell(MonPlatform.isWindows ? "I/O" : "Disk I/O", values: disk) { "\(MonFormat.bytes($0))/s" }
                }
            }
        }
    }

    private func historyCell(_ title: String, values: [Double], format: @escaping (Double) -> String) -> some View {
        let series = HubSeries(
            capacity: MonitorModel.historySamples, samples: values.map { HubSample(value: $0) })
        return VStack(alignment: .leading, spacing: 4) {
            HStack {
                small(title)
                Spacer(minLength: 0)
                Text(verbatim: format(values.last ?? 0))
                    .font(Font.system(size: 12, weight: .semibold))
                    .foregroundStyle(palette.ink.color)
            }
            .frame(width: MonMetrics.historyWidth)
            HubBarChart(
                series: series, label: "\(title) over the last \(values.count) readings", format: format,
                width: MonMetrics.historyWidth, height: 32, showsCaption: false)
            small("Min \(format(values.min() ?? 0)) · Max \(format(values.max() ?? 0))")
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

// MARK: - Small controls

private struct MonSensorRow: Identifiable {
    let id: Int
    let label: String
    let value: String

    static func isCpuTemperature(_ name: String) -> Bool {
        let lower = name.lowercased()
        return lower.contains("cpu") || lower.contains("core") || lower.contains("package")
    }
}

private struct MonOption: Identifiable {
    let id: String
    let label: String
}

/// A short row of choices in one rounded track, the chosen one raised: a Picker replacement.
private struct MonSegmented: View {
    let options: [MonOption]
    let selectedID: String
    let onSelect: @MainActor (String) -> Void
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        HubLayered {
            RoundedRectangle(cornerRadius: 8).fill(palette.fill2.color)
            HStack(spacing: 2) {
                ForEach(options) { option in
                    let selected = option.id == selectedID
                    Button {
                        onSelect(option.id)
                    } label: {
                        HubLayered {
                            RoundedRectangle(cornerRadius: 6).fill(selected ? palette.plane.color : Color.clear)
                            RoundedRectangle(cornerRadius: 6)
                                .stroke(selected ? palette.hairline.color : Color.clear, lineWidth: 1)
                            Text(verbatim: option.label)
                                .font(Font.system(size: 12, weight: selected ? .semibold : .regular))
                                .foregroundStyle(selected ? palette.ink.color : palette.ink2.color)
                                .lineLimit(1)
                                .padding(.horizontal, 10)
                                .padding(.vertical, 5)
                        }
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel(option.label)
                    .accessibilityAddTraits(selected ? .isSelected : [])
                    // Longest label first, so none is cut when the row only just fits (Fit.swift).
                    .layoutPriority(Fit.segment(option.label))
                }
            }
            .padding(2)
        }
        // The whole control keeps its width on Windows' wider font ("Memory", never "Me…"); a search
        // field or title beside it gives way first.
        .layoutPriority(2)
    }
}

/// A small action button: text only (ghost), a filled track (secondary) or a danger colour.
private struct MonButton: View {
    enum Style: Equatable {
        case ghost
        case secondary
        case ghostDanger
        case danger
    }

    let label: String
    let style: Style
    var enabled = true
    var accessibilityLabel: String?
    var selected = false
    let action: @MainActor () -> Void
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    /// The page's small buttons: ghost is the plane colour with a hairline outline, secondary the
    /// grey fill, danger a solid red.
    private var fill: Color {
        switch style {
        case .secondary: return palette.fill2.color
        case .danger: return palette.danger.color
        case .ghost, .ghostDanger: return selected ? palette.fill2.color : palette.plane.color
        }
    }

    private var outline: Color {
        style == .danger ? palette.danger.color : palette.hairline.color
    }

    private var textColor: Color {
        switch style {
        case .danger: return Color.white
        case .ghostDanger: return palette.bad.color
        case .ghost, .secondary: return palette.ink.color
        }
    }

    var body: some View {
        Button {
            action()
        } label: {
            HubLayered {
                RoundedRectangle(cornerRadius: 6).fill(fill)
                RoundedRectangle(cornerRadius: 6).stroke(outline, lineWidth: 1)
                Text(verbatim: label)
                    .font(Font.system(size: 12))
                    .foregroundStyle(textColor)
                    .lineLimit(1)
                    .padding(.horizontal, 10)
                    .padding(.vertical, 4)
            }
        }
        .buttonStyle(.plain)
        .disabled(!enabled)
        .opacity(enabled ? 1 : 0.45)
        .accessibilityLabel(accessibilityLabel ?? label)
        // A button keeps its width on Windows' wider font.
        .layoutPriority(1)
    }
}
#endif

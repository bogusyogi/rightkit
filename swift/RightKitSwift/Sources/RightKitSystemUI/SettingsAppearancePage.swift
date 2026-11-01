#if os(macOS)
import RightKitHub
import SwiftUI

/// Appearance: the notch's gauges, placement, size, parts, rings, colour and language.
struct SettingsAppearancePage: View {
    let model: SettingsPageModel
    @Environment(\.hubTheme) private var theme

    private var rows: SettingsRows { SettingsRows(model: model) }

    var body: some View {
        SettingsCardStack(model: model, cards: cards)
    }

    private var cards: [SettingsCardSpec] {
        var cards = SettingsCardList()
        if let gauges = model.state?.gauges, !gauges.isEmpty {
            cards.add("Gauges", gaugeRows(gauges))
        }
        cards.add("Placement", placementRows())
        cards.add("Size and surface", sizeRows())
        cards.add("Notch parts", partsRows())
        cards.add("Rings", ringRows())
        cards.add("Colour", colourRows())
        cards.add("Language", languageRows())
        return cards.cards
    }

    // MARK: Gauges

    /// Every notch cell, in the notch's order, each with its own switch. The notch keeps at least one
    /// cell on, so the last one that is on cannot be turned off.
    private func gaugeRows(_ gauges: [SettingsGauge]) -> [SettingsRowSpec] {
        let sorted = gauges.sorted { ($0.order ?? 0) < ($1.order ?? 0) }
        let on = sorted.filter { $0.connected == true }.count
        var list = SettingsRowList()
        let ids = sorted.map { $0.id }
        for (index, gauge) in sorted.enumerated() {
            list.add(
                id: "gauge.\(gauge.id)", text: "\(gauge.name) \(SettingsGaugeRow.note(gauge))",
                SettingsGaugeRow(model: model, gauge: gauge, onCount: on, index: index, ids: ids))
        }
        list.foot(id: "gauges.foot", SettingsFootLine(text: "At least one gauge stays on."))
        return list.rows
    }

    // MARK: Groups

    private func placementRows() -> [SettingsRowSpec] {
        var list = SettingsRowList()
        let windows = model.isWindows
        list.add(rows.choice("notchEdge"))
        if !model.options("notchEdge").isEmpty {
            list.foot(
                id: "edge.foot",
                SettingsFootLine(
                    text: windows
                        ? "Alt-drag the notch to slide it along its edge, or towards another edge to dock there."
                        : "Option-drag the notch to slide it along its edge, or towards another edge to dock there."))
        }
        list.add(rows.choice("notchScope"))
        if model.has("displayPreference"), let setting = model.setting("displayPreference") {
            list.add(
                id: "displayPreference", text: setting.title,
                SettingsChoiceRow(setting: setting, model: model, segmented: false))
        }
        list.add(rows.choice("notchVisibility"))
        list.add(rows.toggle("notifications"))
        list.add(rows.toggle("foldsForFullScreen"))
        list.add(
            id: "resetPosition", text: "Position Reset position",
            SettingsButtonRow(title: "Position", button: "Reset position") {
                let current = model
                Task { await current.send("resetPosition") }
            })
        return list.rows
    }

    private func sizeRows() -> [SettingsRowSpec] {
        var list = SettingsRowList()
        list.add(rows.choice("notchSize"))
        list.add(rows.toggle("usesCustomNotchScale"))
        if model.has("usesCustomNotchScale") && model.bool("usesCustomNotchScale") {
            list.add(rows.stepper("customNotchScale", min: 0.5, max: 1.5, step: 0.05))
        }
        list.add(rows.choice("notchSurfaceStyle"))
        return list.rows
    }

    private func partsRows() -> [SettingsRowSpec] {
        var list = SettingsRowList()
        list.add(rows.toggle("showsSettingsHandle"))
        list.add(rows.toggle("notchMinimizeButton"))
        list.add(rows.toggle("notchOptionDrag"))
        list.add(rows.toggle("showsHoverCards"))
        return list.rows
    }

    private func ringRows() -> [SettingsRowSpec] {
        var list = SettingsRowList()
        list.add(rows.toggle("showsNotchReadings"))
        list.add(rows.choice("weeklyRing"))
        list.add(rows.toggle("weeklyRingDashed"))
        list.add(rows.toggle("weeklyHeadline"))
        list.add(rows.toggle("weeklyReading"))
        list.add(rows.toggle("claudeDailyPaceRing"))
        list.add(rows.toggle("claudeShowAllAccounts"))
        list.add(rows.toggle("showUsagePace"))
        list.add(rows.toggle("showCodexExtraLimits"))
        list.add(rows.choice("resetTimeFormat"))
        return list.rows
    }

    private func colourRows() -> [SettingsRowSpec] {
        var list = SettingsRowList()
        list.add(rows.choice("accentColor", segmented: false))
        list.add(rows.choice("colorTransitionStyle"))
        list.add(rows.slider("watchLimit", min: 0.1, max: 0.95, step: 0.05, percent: true))
        list.add(rows.slider("criticalLimit", min: 0.15, max: 1, step: 0.05, percent: true))
        return list.rows
    }

    private func languageRows() -> [SettingsRowSpec] {
        var list = SettingsRowList()
        list.add(rows.choice("language", segmented: false))
        return list.rows
    }
}

/// One notch cell with its switch.
struct SettingsGaugeRow: View {
    let model: SettingsPageModel
    let gauge: SettingsGauge
    let onCount: Int
    let index: Int
    let ids: [String]
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private func move(_ by: Int) {
        let target = index + by
        if target < 0 || target >= ids.count { return }
        var next = ids
        next.swapAt(index, target)
        let current = model
        Task { await current.setGaugeOrder(next) }
    }

    private var palette: HubPalette { theme.palette(scheme) }

    private static let icons: [String: (symbol: String, text: String)] = [
        "cpu": ("cpu", "Processor chip"),
        "memory": ("memorychip", "Memory module"),
        "disk": ("internaldrive", "Drive"),
        "send": ("paperplane", "Paper plane"),
        "tools": ("wrench", "Wrench"),
    ]

    /// What the row says about the cell's mark ("Processor chip", "Claude mark").
    static func note(_ gauge: SettingsGauge) -> String {
        let glyph = gauge.glyph ?? ""
        if let icon = icons[glyph] { return icon.text }
        if glyph == "claude" { return "Claude mark" }
        if glyph == "openai" { return "OpenAI mark" }
        return glyph
    }

    private var note: String { Self.note(gauge) }

    private var binding: Binding<Bool> {
        let current = model
        let id = gauge.id
        let on = gauge.connected == true
        return Binding(
            get: { on },
            set: { value in
                Task { await current.setGauge(id, on: value) }
            }
        )
    }

    var body: some View {
        let connected = gauge.connected == true
        SettingsRowFrame(title: gauge.name, note: note) {
            HStack(spacing: theme.spacing.sm) {
                if let icon = Self.icons[gauge.glyph ?? ""] {
                    Image(systemName: icon.symbol)
                        .foregroundStyle(palette.ink2.color)
                        .accessibilityHidden(true)
                }
                HStack(spacing: 4) {
                    SettingsMiniButton(symbol: "chevron.up", label: "Move \(gauge.name) up", enabled: index != 0) {
                        move(-1)
                    }
                    SettingsMiniButton(symbol: "chevron.down", label: "Move \(gauge.name) down", enabled: index != ids.count - 1) {
                        move(1)
                    }
                }
                Toggle("", isOn: binding)
                    .labelsHidden()
                    .toggleStyle(.switch)
                    .disabled(connected && onCount <= 1)
                    .accessibilityLabel("Show \(gauge.name)")
            }
        }
    }
}
#endif

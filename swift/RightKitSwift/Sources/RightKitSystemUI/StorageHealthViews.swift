#if os(macOS)
import RightKitHub
import SwiftUI

// Drive health on the volume card, its detail panel, and Chrome's leftover snapshots as a finding
// line. Ported from `DriveHealth.tsx` and `ChromeSnapshots.tsx`.

enum StorageHealth {
    static let needsAdmin = "Temperature and wear need Pulse to run as administrator on Windows"

    static func celsius(_ value: Double?) -> String? {
        value.map { "\(Int($0.rounded())) °C" }
    }

    /// Warning as the notch counts it: failed self-assessment, a critical warning, or media errors.
    static func isWarning(_ reading: StorageHealthReading) -> Bool {
        reading.passed == false || (reading.critical_warning ?? 0) != 0 || (reading.media_errors ?? 0) > 0
    }

    /// Windows only: the drive answered, but not one metric came with it (the unelevated paths gave nothing).
    static func lacksMetrics(_ reading: StorageHealthReading) -> Bool {
        #if os(Windows)
        return reading.temperature_c == nil && reading.wear_percent == nil && reading.written_bytes == nil
            && reading.power_on_hours == nil && reading.media_errors == nil
        #else
        return false
        #endif
    }

    /// Temperature, wear and writes, for whichever of them the drive reports.
    static func readingParts(_ reading: StorageHealthReading) -> [String] {
        var parts: [String] = []
        if let temperature = celsius(reading.temperature_c) { parts.append(temperature) }
        if let wear = reading.wear_percent { parts.append("\(StorageText.trimmed(wear))% worn") }
        if let written = reading.written_bytes { parts.append("\(StorageText.bytes(written)) written") }
        return parts
    }

    /// The readings of one metric over time, as a series the bar chart draws.
    static func series(_ history: [StorageHealthReading], _ pick: (StorageHealthReading) -> Double?) -> [(at: Double, value: Double)] {
        history.compactMap { reading in pick(reading).map { (at: reading.at, value: $0) } }
    }
}

/// One drive's health on its volume card. Never blocks the card's scan action.
struct StorageDriveHealthLine: View {
    let card: StorageDriveCard?
    let toolAvailable: Bool
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    private func muted(_ text: String) -> some View {
        Text(verbatim: text).font(theme.type.caption.font).foregroundStyle(palette.ink2.color)
    }

    var body: some View {
        if let card {
            if card.disk == nil {
                muted("Health not read for this volume")
            } else if card.status == "unknown" {
                muted(toolAvailable ? "Health not read yet" : "Install smartmontools for drive health")
            } else if card.status == "unavailable" {
                VStack(alignment: .leading, spacing: 2) {
                    muted("Health unavailable through this connection")
                    if let latest = card.latest {
                        muted("last reading \(StorageText.fullDay(latest.at)): \(StorageHealth.readingParts(latest).joined(separator: " · "))")
                    }
                }
            } else if let latest = card.latest {
                let warning = card.status == "warning"
                HStack(spacing: theme.spacing.sm) {
                    StorageBadge(text: warning ? "Warning" : "OK", tone: warning ? .warn : .ok)
                    muted(StorageHealth.readingParts(latest).joined(separator: " · "))
                    if StorageHealth.lacksMetrics(latest) { muted(StorageHealth.needsAdmin) }
                }
            }
        }
    }
}

/// The detail panel that opens from a drive's card.
struct StorageDriveHealthPanel: View {
    let card: StorageDriveCard?
    let alerts: [StorageHealthAlert]
    let toolAvailable: Bool
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    private func muted(_ text: String) -> some View {
        Text(verbatim: text).font(theme.type.caption.font).foregroundStyle(palette.ink2.color)
    }

    private func metric(_ label: String, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            muted(label)
            Text(verbatim: value).font(theme.type.label.font).foregroundStyle(palette.ink.color)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    var body: some View {
        if let card {
            let latest = card.latest
            let ownAlerts = alerts.filter { $0.disk == card.disk }
            StorageCard {
                VStack(alignment: .leading, spacing: 10) {
                    HStack {
                        Text(verbatim: card.model ?? card.disk ?? "Drive")
                            .font(theme.type.label.font)
                            .foregroundStyle(palette.ink.color)
                        Spacer()
                        muted(card.mount)
                    }
                    if card.disk == nil { muted("Pulse could not match this volume to a physical drive.") }
                    if card.disk != nil && !toolAvailable { muted("Install smartmontools for drive health.") }
                    if card.status == "unavailable" {
                        StorageNotice(
                            text: "Health unavailable through this connection."
                                + (latest.map { " Last good reading: \(StorageText.fullDay($0.at))" } ?? "")
                        )
                    }
                    if let latest, StorageHealth.lacksMetrics(latest) { muted("\(StorageHealth.needsAdmin).") }
                    if let latest {
                        HStack(alignment: .top, spacing: theme.spacing.md) {
                            metric("Health", StorageHealth.isWarning(latest) ? "Warning" : "OK")
                            metric("Temperature", StorageHealth.celsius(latest.temperature_c) ?? "—")
                            metric("Wear", latest.wear_percent.map { "\(StorageText.trimmed($0))%" } ?? "—")
                        }
                        HStack(alignment: .top, spacing: theme.spacing.md) {
                            metric("Written", latest.written_bytes.map(StorageText.bytes) ?? "—")
                            metric("Power-on time", latest.power_on_hours.map { "\(StorageText.count(Int($0))) h" } ?? "—")
                            metric("Critical warnings", latest.critical_warning.map { String($0) } ?? "—")
                            metric("Media errors", latest.media_errors.map { String($0) } ?? "—")
                        }
                        VStack(alignment: .leading, spacing: 3) {
                            muted("Self-tests")
                            if latest.self_tests.isEmpty { muted("No self-test results reported.") }
                            ForEach(Array(latest.self_tests.enumerated()), id: \.offset) { item in
                                let test = item.element
                                HStack {
                                    Text(verbatim: test.kind).font(theme.type.caption.font).foregroundStyle(palette.ink.color)
                                    Spacer()
                                    muted(
                                        test.result
                                            + (test.power_on_hours.map { " · at \(StorageText.count(Int($0))) h" } ?? "")
                                    )
                                }
                            }
                        }
                    }
                    StorageSparkline(label: "Temperature", unit: " °C", points: StorageHealth.series(card.history) { $0.temperature_c })
                    StorageSparkline(label: "Wear", unit: "%", points: StorageHealth.series(card.history) { $0.wear_percent })
                    VStack(alignment: .leading, spacing: 3) {
                        muted("Alerts")
                        if ownAlerts.isEmpty { muted("No alerts.") }
                        ForEach(ownAlerts.prefix(10)) { alert in
                            Text(verbatim: "\(StorageText.fullDay(alert.at)) · \(alert.message)")
                                .font(theme.type.caption.font)
                                .foregroundStyle(palette.ink.color)
                        }
                    }
                }
            }
        }
    }
}

/// Timestamped values as a bar chart (the page drew a line); needs two readings to draw.
struct StorageSparkline: View {
    let label: String
    let unit: String
    let points: [(at: Double, value: Double)]
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        if let first = points.first, let last = points.last {
            let values = points.map(\.value)
            let low = values.min() ?? 0
            let high = values.max() ?? 0
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 4) {
                    Text(verbatim: label).font(theme.type.caption.font).foregroundStyle(palette.ink2.color)
                    Text(verbatim: "\(StorageText.trimmed(last.value))\(unit)")
                        .font(theme.type.label.font)
                        .foregroundStyle(palette.ink.color)
                    Text(verbatim: "\(StorageText.trimmed(low))–\(StorageText.trimmed(high))\(unit) · \(StorageText.fullDay(first.at))"
                        + (first.at != last.at ? " – \(StorageText.fullDay(last.at))" : ""))
                        .font(theme.type.caption.font)
                        .foregroundStyle(palette.ink2.color)
                }
                if points.count < 2 {
                    Text(verbatim: "One reading so far; the line appears with the next.")
                        .font(theme.type.caption.font)
                        .foregroundStyle(palette.ink2.color)
                } else {
                    HubBarChart(
                        series: HubSeries(
                            capacity: points.count,
                            samples: points.map { HubSample(time: Date(timeIntervalSince1970: $0.at), value: $0.value) }
                        ),
                        range: min(0, low)...max(high, low + 1),
                        label: "\(label) over time",
                        format: { "\(StorageText.trimmed($0))\(unit)" },
                        width: 260,
                        height: 44,
                        showsCaption: false
                    )
                }
            }
        }
    }
}

/// Chrome's leftover signing snapshots as a normal finding row: how many, the change since the last
/// check, and what moving them would really give back.
struct StorageChromeSnapshotsLine: View {
    let info: StorageChromeSnapshots?
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        if let info, info.count > 0 {
            let palette = theme.palette(scheme)
            let n = info.count
            let delta: Int? = {
                guard info.since_at != nil, let before = info.since_count else { return nil }
                return n - before
            }()
            let since = info.since_at.map(StorageText.monthDay) ?? ""
            let change: String? = {
                guard let delta, delta != 0 else { return nil }
                return delta < 0 ? "\(-delta) fewer since \(since)" : "+\(delta) since \(since)"
            }()
            let detail: String = {
                if !info.reclaimable_known {
                    return "Apparent \(StorageText.bytes(info.apparent_bytes)), much less to free; shares space with Chrome"
                }
                if info.reclaimable_bytes == 0 { return "Nothing to free; shares space with Chrome" }
                return "About \(StorageText.bytes(info.reclaimable_bytes)) to gain (apparent \(StorageText.bytes(info.apparent_bytes)), shares space with Chrome)"
            }()
            VStack(spacing: 0) {
                Divider()
                HStack(alignment: .top, spacing: theme.spacing.row) {
                    VStack(alignment: .leading, spacing: 1) {
                        HStack(spacing: 0) {
                            Text(verbatim: "Chrome \(n == 1 ? "snapshot" : "snapshots")")
                                .font(theme.type.label.font)
                                .foregroundStyle(palette.ink.color)
                            Text(verbatim: " · \(n)\(change.map { " (\($0))" } ?? "")")
                                .font(theme.type.body.font)
                                .foregroundStyle(palette.ink2.color)
                        }
                        Text(verbatim: detail + (info.running ? " · Close Chrome to clear \(n == 1 ? "it" : "them")" : ""))
                            .font(theme.type.caption.font)
                            .foregroundStyle(palette.ink2.color)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    if info.reclaimable_known {
                        Text(verbatim: StorageText.bytes(info.reclaimable_bytes))
                            .font(theme.type.label.font)
                            .foregroundStyle(palette.ink.color)
                    }
                }
                .padding(.vertical, theme.spacing.sm)
            }
        }
    }
}
#endif

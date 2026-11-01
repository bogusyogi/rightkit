#if os(macOS)
import RightKitHub
import SwiftUI

/// "Starts by itself": login and background items, read only, grouped by who owns them.
struct StartupView: View {
    @Bindable var model: StartupModel
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    /// One row with an id that stays unique when two items share a name and kind.
    private struct Row: Identifiable {
        let id: String
        let item: AppsAPI.StartupItem
    }

    private func rows(for scope: String, in report: AppsAPI.StartupReport) -> [Row] {
        var out: [Row] = []
        for (index, item) in report.rows.enumerated() where item.scope == scope {
            out.append(Row(id: "\(scope):\(item.kind):\(item.name):\(index)", item: item))
        }
        return out
    }

    private var status: String {
        if model.loading { return "Reading…" }
        if let report = model.report { return "\(report.rows.count) items" }
        return ""
    }

    var body: some View {
        AppsDisclosure(
            "Starts by itself",
            bold: true,
            isOpen: model.isOpen,
            toggle: { model.toggle() }
        ) {
            reportBody
        }
    }

    private var reportBody: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                ACText(status, .caption, ink: .secondary)
                Spacer(minLength: 0)
                ACButton(model.loading ? "Reading…" : "Refresh", enabled: !model.loading) {
                    Task { await model.load() }
                }
            }
            if let message = model.error {
                ACNoticeText(message, tone: .bad)
            }
            if model.loading && model.report == nil {
                AppsStateBlock(title: "Reading startup items…", message: "")
            }
            if let report = model.report {
                if report.rows.isEmpty {
                    AppsStateBlock(
                        title: "Nothing starts by itself",
                        message: "No launch items or Run entries were found."
                    )
                }
                ForEach(StartupModel.scopes) { entry in
                    let scoped = rows(for: entry.id, in: report)
                    if !scoped.isEmpty {
                        VStack(alignment: .leading, spacing: 0) {
                            Text(verbatim: entry.label)
                                .font(Font.system(size: 12, weight: .semibold))
                                .foregroundStyle(palette.ink.color)
                                .padding(.top, 8)
                                .padding(.bottom, 2)
                            ForEach(scoped) { row in
                                startRow(row.item)
                                Divider()
                            }
                        }
                        .accessibilityLabel(entry.label)
                    }
                }
                if !report.notes.isEmpty {
                    VStack(alignment: .leading, spacing: 4) {
                        ForEach(report.notes, id: \.self) { note in
                            ACText(note, .caption, ink: .secondary)
                        }
                    }
                }
            }
        }
    }

    private func startRow(_ item: AppsAPI.StartupItem) -> some View {
        HStack(spacing: 10) {
            VStack(alignment: .leading, spacing: 2) {
                Text(verbatim: item.name)
                    .font(Font.system(size: 12, weight: .semibold))
                    .foregroundStyle(palette.ink.color)
                    .lineLimit(1)
                ACText(item.kind, .caption, ink: .secondary, lines: 1)
            }
            Spacer(minLength: 0)
            ACText(item.program.map { Self.middle($0) } ?? "Not known", .caption, ink: .secondary, lines: 1)
            stateBadge(item.enabled)
            AppsButton(
                "Reveal",
                enabled: item.path != nil,
                accessibility: "Reveal \(item.name)"
            ) {
                if let path = item.path {
                    Task { await model.reveal(path) }
                }
            }
        }
        .padding(.vertical, 6)
    }

    @ViewBuilder
    private func stateBadge(_ enabled: Bool?) -> some View {
        if enabled == true {
            AppsBadge("On", tone: .ok)
        } else if enabled == false {
            AppsBadge("Off")
        } else {
            AppsBadge("Unknown")
        }
    }

    /// Middle-truncates long text, as the page does; the full value is the item's own path or program.
    static func middle(_ text: String, limit: Int = 60) -> String {
        if text.count <= limit { return text }
        let head = limit / 2
        let tail = (limit - 1) / 2
        return "\(text.prefix(head))…\(text.suffix(tail))"
    }
}
#endif

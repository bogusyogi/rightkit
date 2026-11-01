#if os(macOS)
import RightKitHub
import SwiftUI

/// Activity's page: filter chips, one card of rows per day, "Load older" and the could-not-read
/// note. Port of `hub/src/views/Activity.tsx`.
struct ActivityView: View {
    let model: ActivityModel
    /// False when `preparedView` already loaded the first page.
    let loadsOnAppear: Bool
    @Environment(\.hubTheme) private var theme
    /// Shows another hub section ("cleanup", "apps", "storage", "chat").
    @Environment(\.hubNavigate) private var onOpen
    @Environment(\.colorScheme) private var scheme
    /// True while the kind choices are open under the header.
    @State private var filterOpen = false

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                header
                if filterOpen {
                    HStack(spacing: 0) {
                        Spacer(minLength: 0)
                        StorageChoicePanel(choices: filterChoices)
                            .frame(width: 180)
                    }
                }

                if let error = model.error {
                    HubBanner("Activity could not be loaded: \(error)", tone: .bad)
                }

                content

                if model.more {
                    HStack {
                        Spacer()
                        ActivityButton(title: "Load older", enabled: !model.busy) {
                            Task { await model.loadOlder() }
                        }
                        Spacer()
                    }
                }

                if !model.unreadable.isEmpty {
                    Text(verbatim: "Could not read: \(model.unreadable.joined(separator: ", ")).")
                        .font(theme.type.caption.font)
                        .foregroundStyle(palette.ink2.color)
                        .padding(.horizontal, 2)
                }
            }
            .padding(.horizontal, 18)
            .padding(.top, 14)
            .padding(.bottom, 18)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .task {
            await model.run(loadFirst: loadsOnAppear)
        }
    }

    // MARK: Filter

    /// The intro line and, on its right, the page's "Filter by kind" select: its value is the chosen kind
    /// and pressing it opens "All" and each kind present under the line.
    private var header: some View {
        HStack(spacing: 12) {
            Text(verbatim: "What the app did and saw, newest first. Read only: nothing is recorded here.")
                .font(theme.type.caption.font)
                .foregroundStyle(palette.ink2.color)
                .frame(maxWidth: .infinity, alignment: .leading)
                // The line keeps its width; the select, whose spacer would otherwise fill half the row
                // on Windows, is held to 180 pt there (Fit.swift).
                .layoutPriority(Fit.priority(windows: 3))
            StorageSelect(
                label: "Filter by kind",
                value: model.currentFilter,
                open: filterOpen,
                enabled: model.rows != nil,
                minWidth: 140,
                maxWidth: ACPlatform.isWindows ? 180 : nil
            ) {
                filterOpen.toggle()
            }
        }
    }

    private var filterChoices: [StorageChoice] {
        (["All"] + model.kinds).map { kind in
            StorageChoice(id: kind, title: kind, selected: model.currentFilter == kind) {
                model.filter = kind
                filterOpen = false
            }
        }
    }

    // MARK: Rows

    @ViewBuilder
    private var content: some View {
        if model.rows == nil, model.error == nil {
            Text(verbatim: "Loading activity…")
                .font(theme.type.caption.font)
                .foregroundStyle(palette.ink2.color)
        } else if let rows = model.rows, rows.isEmpty {
            HubCard {
                VStack(alignment: .leading, spacing: 4) {
                    Text(verbatim: "Nothing recorded yet")
                        .font(theme.type.label.font)
                        .foregroundStyle(palette.ink.color)
                    Text(verbatim: "Cleanup runs and undos, uninstalls, drive alerts, storage scans, file transfers and chat messages will appear here as they happen.")
                        .font(theme.type.caption.font)
                        .foregroundStyle(palette.ink2.color)
                }
            }
        } else if let rows = model.rows, !rows.isEmpty, model.visibleRows.isEmpty {
            Text(verbatim: "No \(model.currentFilter.lowercased()) records in the rows loaded so far.")
                .font(theme.type.caption.font)
                .foregroundStyle(palette.ink2.color)
        }
        if !model.totals.isEmpty, model.currentFilter == "All" {
            VStack(alignment: .leading, spacing: 6) {
                Text(verbatim: "Totals")
                    .font(theme.type.label.font)
                    .foregroundStyle(palette.ink.color)
                    .padding(.leading, 2)
                HubCard(padding: 0) {
                    ForEach(Array(model.totals.enumerated()), id: \.offset) { index, total in
                        if index > 0 { Divider() }
                        totalRow(total)
                    }
                }
                Text(verbatim: "Totals cover the newest records each source keeps, not every record ever made.")
                    .font(theme.type.caption.font)
                    .foregroundStyle(palette.ink2.color)
                    .padding(.leading, 2)
            }
        }
        ForEach(Array(model.groups.enumerated()), id: \.offset) { _, group in
            VStack(alignment: .leading, spacing: 6) {
                Text(verbatim: group.label)
                    .font(theme.type.label.font)
                    .foregroundStyle(palette.ink.color)
                    .padding(.leading, 2)
                HubCard(padding: 0) {
                    ForEach(Array(group.rows.enumerated()), id: \.offset) { index, row in
                        if index > 0 { Divider() }
                        rowView(row)
                    }
                }
            }
        }
    }

    private static func utc(_ at: Int64) -> String {
        let text = ISO8601DateFormatter().string(from: Date(timeIntervalSince1970: Double(at)))
        return String(text.prefix(16)).replacingOccurrences(of: "T", with: " ") + " UTC"
    }

    private func totalRow(_ total: ActivityTotalDTO) -> some View {
        let trash = ACPlatform.isWindows ? "Recycle Bin" : "Trash"
        let title = total.period == "week" ? "This week" : "This month"
        let records = "\(total.events) \(total.events == 1 ? "record" : "records")"
        let transfers = "\(total.transfers) \(total.transfers == 1 ? "transfer" : "transfers")"
        let summary = "\(records), \(transfers), \(PulseFormat.bytes(total.movedBytes)) moved to the \(trash) (not freed). From \(Self.utc(total.from)) to \(Self.utc(total.to))."
        return HStack(spacing: 10) {
            Text(verbatim: title)
                .font(theme.type.small.font)
                .foregroundStyle(palette.ink2.color)
                .frame(width: 186, alignment: .leading)
            Text(verbatim: summary)
                .font(theme.type.body.font)
                .foregroundStyle(palette.ink.color)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 9)
    }

    private func rowView(_ row: ActivityRowDTO) -> some View {
        HStack(spacing: 10) {
            Text(verbatim: ActivityFormat.clock(at: row.at))
                .font(theme.type.caption.font)
                .foregroundStyle(palette.ink2.color)
                .frame(width: 74, alignment: .leading)
            kindPill(row.kind)
                .frame(width: 112, alignment: .leading)
            Text(verbatim: row.summary)
                .font(theme.type.body.font)
                .foregroundStyle(palette.ink.color)
                .frame(maxWidth: .infinity, alignment: .leading)
            if let open = row.open {
                ActivityButton(title: "Open \(ActivityModel.sectionTitles[open] ?? open)", enabled: true) {
                    onOpen(open)
                }
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 9)
    }

    private func kindPill(_ kind: String) -> some View {
        HubLayered {
            // An outline from fills (a stroked pill is not delivered on Windows); the card is the base.
            HubPillOutline(line: palette.hairline.color, base: palette.panel.color, radius: theme.radii.pill)
            Text(verbatim: kind)
                .font(theme.type.small.font)
                .foregroundStyle(palette.ink2.color)
                .padding(.horizontal, 8)
                .padding(.vertical, 2)
        }
    }
}

/// A small secondary button: a rounded outline with a text label.
struct ActivityButton: View {
    let title: String
    let enabled: Bool
    let action: @MainActor () -> Void
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        Button {
            action()
        } label: {
            HubLayered {
                // The page's small secondary button: a flat tinted fill, no border.
                RoundedRectangle(cornerRadius: 7).fill(palette.fill2.color)
                Text(verbatim: title)
                    .font(theme.type.label.font)
                    .foregroundStyle(palette.ink.color)
                    .lineLimit(1)
                    .padding(.horizontal, 10)
                    .padding(.vertical, 5)
            }
        }
        .buttonStyle(.plain)
        .disabled(!enabled)
        .opacity(enabled ? 1 : 0.5)
    }
}
#endif

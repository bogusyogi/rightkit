#if os(macOS)
import RightKitHub
import SwiftUI

/// The Findings and Changes workspaces: what is safe to clear (with Move to Trash), and what grew
/// or shrank since the last scan.
struct StorageFindingsView: View {
    let model: StorageModel
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private static let visibleGroups = 6
    private static let visibleNotes = 3

    private var palette: HubPalette { theme.palette(scheme) }

    private func muted(_ text: String) -> some View {
        Text(verbatim: text).font(theme.type.caption.font).foregroundStyle(palette.ink2.color)
    }

    var body: some View {
        let groups = model.groups
        let shown = model.showAll ? groups : Array(groups.prefix(Self.visibleGroups))
        let others = model.others
        let notes = model.showNotes ? others : Array(others.prefix(Self.visibleNotes))
        StorageCard {
            VStack(alignment: .leading, spacing: 4) {
                head
                if let message = model.findingsError {
                    StorageNotice(text: "Could not refresh the findings: \(message)", tone: .bad)
                }
                StorageChromeSnapshotsLine(info: model.report?.chrome_snapshots)
                ForEach(shown) { group in
                    groupRow(group)
                }
                if groups.count > Self.visibleGroups {
                    StorageButton(
                        title: model.showAll ? "Show fewer" : "Show \(groups.count - Self.visibleGroups) more",
                        weight: .ghost
                    ) { model.showAll.toggle() }
                }
                if !others.isEmpty {
                    VStack(alignment: .leading, spacing: 4) {
                        Divider()
                        Text(verbatim: "Notes")
                            .font(theme.type.caption.font)
                            .foregroundStyle(palette.ink2.color)
                            .padding(.top, 4)
                        ForEach(notes) { finding in
                            HStack(alignment: .top, spacing: 8) {
                                Image(systemName: storageNoteSymbol(finding))
                                    .font(Font.system(size: 11))
                                    .foregroundStyle(palette.ink2.color)
                                    .frame(width: 14)
                                muted("\(finding.rule_name ?? finding.name) · \(finding.reason)")
                                    .frame(maxWidth: .infinity, alignment: .leading)
                                muted(StorageText.bytes(finding.bytes))
                            }
                        }
                        if others.count > Self.visibleNotes {
                            StorageButton(
                                title: model.showNotes ? "Show fewer notes" : "Show \(others.count - Self.visibleNotes) more notes",
                                weight: .ghost
                            ) { model.showNotes.toggle() }
                        }
                    }
                    .padding(.top, 6)
                }
            }
        }
    }

    private var subtitle: String {
        var text = "Home"
        if let report = model.report {
            text += " · scanned \(StorageText.ago(report.scanned_at))"
            if model.findingsBusy { text += " · Refreshing…" }
        }
        return text
    }

    private var head: some View {
        HStack(alignment: .center, spacing: theme.spacing.sm) {
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 7) {
                    Image(systemName: "checkmark.shield").foregroundStyle(palette.accent.color)
                    Text(verbatim: model.headline)
                        .font(Font.system(size: 14, weight: .semibold))
                        .foregroundStyle(palette.ink.color)
                }
                muted(subtitle)
                if model.freeable > 0 { muted(StorageText.estimate) }
            }
            // The text column wraps when the row is tight; the button keeps its width (Fit.swift).
            .layoutPriority(Fit.priority(windows: 3))
            Spacer()
            if model.safeItems.count > 1 {
                StorageButton(
                    title: "Clear all safe (\(StorageText.upTo(model.report?.safe_bytes ?? 0)))",
                    weight: .primary
                ) {
                    model.pending = StorageModel.PendingTrash(label: "Safe items", items: model.safeItems)
                }
                .disabled(model.busy)
                .layoutPriority(2)
            }
        }
        .padding(.bottom, 4)
    }

    private func groupRow(_ group: StorageFindingGroup) -> some View {
        let expanded = model.openGroups.contains(group.key)
        return VStack(alignment: .leading, spacing: 0) {
            Divider()
            HStack(alignment: .top, spacing: theme.spacing.row) {
                Button {
                    model.toggleGroup(group.key)
                } label: {
                    HubLayered {
                        HubHitArea()
                        HStack(alignment: .top, spacing: 6) {
                            Image(systemName: expanded ? "chevron.down" : "chevron.right")
                                .font(Font.system(size: 11))
                                .foregroundStyle(palette.ink2.color)
                                .frame(width: 14)
                                .padding(.top, 3)
                            VStack(alignment: .leading, spacing: 1) {
                                HStack(spacing: 8) {
                                    Text(verbatim: group.label + (group.countText.isEmpty ? "" : " · \(group.countText)"))
                                        .font(theme.type.label.font)
                                        .foregroundStyle(palette.ink.color)
                                        .lineLimit(1)
                                    StorageBadge(text: group.risk == "safe" ? "Safe" : "Review", tone: group.risk == "safe" ? .ok : .warn)
                                }
                                muted(group.reason)
                            }
                            .frame(maxWidth: .infinity, alignment: .leading)
                        }
                        .padding(.vertical, 4)
                    }
                }
                .buttonStyle(.plain)
                .frame(maxWidth: .infinity, alignment: .leading)
                .accessibilityLabel("\(group.label), \(expanded ? "expanded" : "collapsed")")
                // The title column wraps when the row is tight; the size and button keep their width (Fit.swift).
                .layoutPriority(Fit.priority(windows: 3))
                // The size and the button keep their width on Windows' wider font ("Move to Recycle Bin");
                // the title column gives way first.
                Text(verbatim: StorageText.upTo(group.bytes, partial: group.partial))
                    .font(theme.type.label.font)
                    .foregroundStyle(palette.ink.color)
                    .lineLimit(1)
                    .padding(.top, 5)
                    .layoutPriority(2)
                StorageButton(title: "Move to \(StorageText.trash)", enabled: !model.busy) {
                    model.pending = StorageModel.PendingTrash(label: group.label, items: group.items)
                }
                .layoutPriority(2)
            }
            .padding(.vertical, 4)
            if expanded {
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(group.items) { finding in
                        VStack(spacing: 0) {
                            Divider()
                            HStack(alignment: .center, spacing: theme.spacing.row) {
                                VStack(alignment: .leading, spacing: 1) {
                                    if group.items.count > 1 {
                                        Text(verbatim: finding.name)
                                            .font(theme.type.body.font)
                                            .foregroundStyle(palette.ink.color)
                                    }
                                    muted(StorageText.shortPath(finding.path))
                                }
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .layoutPriority(Fit.priority(windows: 3))
                                muted(StorageText.upTo(finding.bytes, partial: finding.partial))
                                    .lineLimit(1)
                                    .layoutPriority(2)
                                StorageButton(title: "Move to \(StorageText.trash)", weight: .ghost) {
                                    model.pending = StorageModel.PendingTrash(label: finding.name, items: [finding])
                                }
                                .disabled(model.busy)
                                .layoutPriority(2)
                            }
                            .padding(.vertical, 4)
                        }
                    }
                }
                .padding(.leading, 20)
            }
        }
    }
}

/// The Changes workspace: folders of the home folder that grew or shrank since the last scan.
struct StorageChangesView: View {
    let model: StorageModel
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        let growth = model.growth
        let changes: [StorageChange] = {
            if let growth, growth.available { return growth.grown + growth.shrunk }
            return []
        }()
        if !model.internalActive {
            StorageEmpty(symbol: "info.circle", title: "Changes are tracked for your home folder")
        } else if changes.isEmpty {
            StorageEmpty(
                symbol: "info.circle",
                title: growth?.available == true ? "No changes since the last scan" : "No comparison yet",
                message: growth?.available == true ? "" : (growth?.reason ?? "Changes appear after a second scan.")
            )
        } else {
            StorageCard {
                VStack(alignment: .leading, spacing: 4) {
                    Text(verbatim: "Home changes")
                        .font(Font.system(size: 14, weight: .semibold))
                        .foregroundStyle(palette.ink.color)
                    Text(verbatim: "Since last scan" + (growth?.since.map { " (\(StorageText.monthDay($0)))" } ?? ""))
                        .font(theme.type.caption.font)
                        .foregroundStyle(palette.ink2.color)
                        .padding(.bottom, 4)
                    ForEach(changes, id: \.path) { change in
                        VStack(spacing: 0) {
                            Divider()
                            Button {
                                model.openFromChange(change.path)
                            } label: {
                                HubLayered {
                                    HubHitArea()
                                    HStack(spacing: theme.spacing.row) {
                                        Text(verbatim: StorageText.shortPath(change.path))
                                            .font(theme.type.body.font)
                                            .foregroundStyle(palette.ink.color)
                                            .frame(maxWidth: .infinity, alignment: .leading)
                                        Text(verbatim: StorageText.signedBytes(change.bytes))
                                            .font(theme.type.label.font)
                                            .foregroundStyle(change.bytes > 0 ? palette.warn.color : palette.ok.color)
                                    }
                                    .padding(.vertical, 8)
                                    .padding(.horizontal, 4)
                                }
                            }
                            .buttonStyle(.plain)
                            .disabled(model.busy)
                        }
                    }
                }
            }
        }
    }
}
#endif

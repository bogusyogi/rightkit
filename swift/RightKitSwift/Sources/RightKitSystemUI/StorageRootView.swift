#if os(macOS)
import RightKitHub
import SwiftUI

/// Storage: the volume cards, the scan state, the four workspaces and the confirmations. Port of
/// `Storage()` in `hub/src/views/Storage.tsx`.
struct StorageRootView: View {
    @State private var model: StorageModel
    /// False when the model was loaded ahead (the off-screen renderer): no initial reload.
    private let reloads: Bool
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    init(ctx: HubContext, notch: any NotchBridge) {
        _model = State(initialValue: StorageModel(ctx: ctx, notch: notch))
        reloads = true
    }

    init(model: StorageModel) {
        _model = State(initialValue: model)
        reloads = false
    }

    private var palette: HubPalette { theme.palette(scheme) }

    private func muted(_ text: String) -> some View {
        Text(verbatim: text).font(theme.type.caption.font).foregroundStyle(palette.ink2.color)
    }

    var body: some View {
        let trashSnapshot = model.itemTrash
        let moveSnapshot = model.itemMove
        let pendingSnapshot = model.pending
        ScrollView {
            VStack(alignment: .leading, spacing: 10) {
                if model.pane != .duplicates {
                    volumeSection
                    metaLine
                }
                HStack(spacing: 0) {
                    StorageTabs(panes: StorageModel.Pane.allCases, selection: model.pane) { model.choosePane($0) }
                    Spacer(minLength: 0)
                }
                if model.pane == .duplicates {
                    StorageDuplicatesView(model: model.duplicates)
                } else {
                    notices
                    if let card = model.health?.drives.first(where: { $0.mount == model.active }), model.healthOpen == model.active {
                        StorageDriveHealthPanel(card: card, alerts: model.health?.alerts ?? [], toolAvailable: model.health?.tool_available ?? true)
                    }
                    if model.pane == .findings {
                        findings
                    } else if model.pane == .folders {
                        StorageFoldersView(model: model)
                    } else {
                        StorageChangesView(model: model)
                    }
                }
            }
            .padding(.horizontal, 18)
            .padding(.top, 14)
            .padding(.bottom, 18)
            .frame(maxWidth: .infinity, alignment: .leading)
            .hubConfirm(
                pendingSnapshot.map { $0.items.count == 1 ? "Move \($0.items[0].name) to the \(StorageText.trash)?" : "Move \($0.items.count) items to the \(StorageText.trash)?" } ?? "",
                message: pendingSnapshot.map(pendingMessage),
                confirmLabel: "Move to \(StorageText.trash)",
                isPresented: Binding(get: { model.pending != nil }, set: { if !$0 { model.pending = nil } })
            ) {
                if let pendingSnapshot { model.confirmMove(pendingSnapshot) }
            }
            .hubConfirm(
                trashSnapshot.map { "Move \($0.row.name) to the \(StorageText.trash)?" } ?? "",
                message: trashSnapshot.map(trashMessage),
                confirmLabel: "Move to \(StorageText.trash)",
                isPresented: Binding(get: { model.itemTrash != nil }, set: { if !$0 { model.itemTrash = nil } })
            ) {
                if let trashSnapshot { model.confirmItemTrash(trashSnapshot) }
            }
        }
        .hubConfirm(
            moveSnapshot.map { "Copy \($0.row.name) to another drive?" } ?? "",
            message: moveSnapshot.map {
                "\(StorageText.shortPath($0.destination)) is on another drive, so \($0.row.name) is copied to \(StorageText.shortPath($0.target)), and the original moves to the \(StorageText.trash). \(StorageText.trashFreesNothing)"
            },
            confirmLabel: "Copy and move to \(StorageText.trash)",
            isPresented: Binding(get: { model.itemMove != nil }, set: { if !$0 { model.itemMove = nil } })
        ) {
            if let moveSnapshot { model.confirmItemMove(moveSnapshot) }
        }
        .task {
            if reloads { await model.load() }
            await model.healthLoop()
        }
    }

    private func pendingMessage(_ pending: StorageModel.PendingTrash) -> String {
        let total = pending.items.reduce(Int64(0)) { $0 + $1.bytes }
        let prefix = pending.items.count > 1 ? "\(pending.label): " : ""
        return "\(prefix)\(StorageText.plural(pending.items.count, "item")), \(StorageText.upTo(total, partial: pending.items.contains { $0.partial })) on disk will move to the \(StorageText.trash) (\(StorageText.estimate)). Nothing is deleted: you can put it back, or empty the \(StorageText.trash) yourself. \(StorageText.trashFreesNothing)"
    }

    private func trashMessage(_ item: StorageModel.ItemTrash) -> String {
        let network = model.onNetwork(item.row.path) ? " Network volumes usually delete at once instead of using the Trash." : ""
        return "\(StorageText.shortPath(item.row.path)), \(StorageText.upTo(item.row.bytes)) on disk will move to the \(StorageText.trash) (\(StorageText.estimate)). Nothing is deleted: you can put it back, or empty the \(StorageText.trash) yourself. \(StorageText.trashFreesNothing)\(network)"
    }

    // MARK: Volumes

    /// The page's volume section: the volume select, its free space and Rescan on one line, the used
    /// bar, then the drive-health line with its link.
    private var volumeSection: some View {
        let volume = model.activeVolume
        let card = model.health?.drives.first { $0.mount == model.active }
        let healthShown = model.healthOpen == model.active
        var used: Double?
        if let volume, let total = volume.total_bytes, let available = volume.available_bytes, total > 0 {
            used = 1 - Double(available) / Double(total)
        }
        let icon: String = {
            guard let volume else { return "internaldrive" }
            if volume.isNetwork { return "network" }
            return volume.isInternal ? "internaldrive" : "externaldrive"
        }()
        return StorageCard {
            VStack(alignment: .leading, spacing: 6) {
                HStack(spacing: 10) {
                    Image(systemName: icon)
                        .foregroundStyle(palette.accent.color)
                    if model.volumeList.isEmpty {
                        Text(verbatim: "No volumes")
                            .font(Font.system(size: 13, weight: .semibold))
                            .foregroundStyle(palette.ink.color)
                    } else {
                        StorageSelect(
                            label: "Volume",
                            value: volume.map(volumeTitle) ?? model.active,
                            open: model.volumeMenu,
                            enabled: !model.busy,
                            // Wide enough for "Macintosh HD · Startup" in Segoe UI, so the free-space line
                            // beside it, which truncates, cannot take its room.
                            minWidth: Fit.length(windows: 215, other: 160),
                            maxWidth: 340
                        ) {
                            model.filterMenu = nil
                            model.volumeMenu.toggle()
                        }
                    }
                    if volume?.isNetwork == true {
                        Text(verbatim: "Network")
                            .font(theme.type.small.font)
                            .foregroundStyle(palette.ink2.color)
                            .lineLimit(1)
                    }
                    if let volume { freeText(volume) }
                    Spacer(minLength: 0)
                    StorageButton(
                        title: "Rescan",
                        symbol: "arrow.clockwise",
                        enabled: !(model.busy || model.scanning || volume?.state == "not responding")
                    ) { model.rescan() }
                }
                if model.volumeMenu {
                    StorageChoicePanel(choices: volumeChoices)
                }
                if let used, let volume {
                    StorageFractionBar(
                        fraction: used,
                        color: StorageText.tone(used).color(in: palette).color,
                        height: 6,
                        label: "\(volume.name) used",
                        value: "\(Int((used * 100).rounded())) percent",
                        track: palette.plane.color
                    )
                }
                if card != nil || model.health != nil {
                    HStack(spacing: 10) {
                        StorageDriveHealthLine(card: card, toolAvailable: model.health?.tool_available ?? true)
                        if card != nil {
                            Button {
                                model.healthOpen = healthShown ? nil : model.active
                            } label: {
                                Text(verbatim: healthShown ? "Hide drive health" : "Drive health")
                                    .font(theme.type.caption.font)
                                    .foregroundStyle(palette.accent.color)
                                    .lineLimit(1)
                                    .padding(.vertical, 4)
                            }
                            .buttonStyle(.plain)
                            .layoutPriority(1)
                            .accessibilityAddTraits(healthShown ? .isSelected : [])
                        }
                        Spacer(minLength: 0)
                    }
                    .frame(minHeight: 18)
                } else if let message = model.healthError {
                    muted("Drive health unavailable: \(message)")
                }
            }
        }
    }

    /// The select's text for a volume: "Macintosh HD · Startup", or the name of a drive that is not the startup disk.
    private func volumeTitle(_ volume: StorageVolume) -> String {
        if volume.isNetwork {
            if !volume.isResponding { return "\(volume.name) · Not responding" }
            if let available = volume.available_bytes { return "\(volume.name) · \(StorageText.bytes(available)) free" }
            return volume.name
        }
        return volume.isInternal ? "\(volume.name) · Startup" : volume.name
    }

    /// The choices of the Volume select: local volumes, then the network ones under a "Network" heading.
    private var volumeChoices: [StorageChoice] {
        var choices: [StorageChoice] = []
        var firstNetwork = true
        for volume in model.volumeList.filter({ !$0.isNetwork }) + model.volumeList.filter({ $0.isNetwork }) {
            var heading: String?
            if volume.isNetwork && firstNetwork {
                heading = "Network"
                firstNetwork = false
            }
            choices.append(
                StorageChoice(
                    id: volume.mount_point,
                    title: volumeTitle(volume),
                    selected: volume.mount_point == model.active,
                    enabled: !volume.isNetwork || volume.isResponding,
                    heading: heading
                ) { model.chooseVolume(volume.mount_point) }
            )
        }
        return choices
    }

    /// "27.4 GB free of 155 GB", with the reclaimable note after a dot.
    @ViewBuilder
    private func freeText(_ volume: StorageVolume) -> some View {
        if !volume.isResponding {
            Text(verbatim: "Not responding")
                .font(Font.system(size: 13, weight: .semibold))
                .foregroundStyle(palette.ink.color)
                .lineLimit(1)
                .layoutPriority(2)
        } else if let total = volume.total_bytes, let available = volume.available_bytes {
            HStack(spacing: 4) {
                Text(verbatim: "\(StorageText.bytes(available)) free")
                    .font(Font.system(size: 13, weight: .semibold))
                    .foregroundStyle(palette.ink.color)
                    .lineLimit(1)
                    .layoutPriority(2)
                Text(verbatim: "of \(StorageText.bytes(total))")
                    .font(theme.type.body.font)
                    .foregroundStyle(palette.ink2.color)
                    .lineLimit(1)
                    .layoutPriority(1)
                if let note = StorageText.reclaimableNote(volume) {
                    Text(verbatim: "· \(note)")
                        .font(theme.type.body.font)
                        .foregroundStyle(palette.ink2.color)
                        .lineLimit(1)
                        // The note is what truncates when the line is tight (Fit.swift).
                        .layoutPriority(Fit.priority(windows: 3))
                }
            }
            // The free-space line is the part of the volume row that gives way, so the select and
            // Rescan keep their width (Fit.swift).
            .layoutPriority(Fit.priority(windows: 5))
        } else {
            Text(verbatim: "Capacity unknown")
                .font(theme.type.body.font)
                .foregroundStyle(palette.ink2.color)
                .lineLimit(1)
        }
    }

    /// Scope and scan state under the volume section, with the mounted installers beside it.
    private var metaLine: some View {
        VStack(alignment: .leading, spacing: 4) {
            muted([model.internalActive ? "Home folder" : "Whole drive", model.scanText].filter { !$0.isEmpty }.joined(separator: " · "))
                .frame(minHeight: 18)
            if !model.installers.isEmpty {
                HStack(spacing: theme.spacing.sm) {
                    muted("Mounted installers")
                    ForEach(model.installers) { volume in
                        HubLayered {
                            RoundedRectangle(cornerRadius: theme.radii.item).fill(palette.fill2.color)
                            RoundedRectangle(cornerRadius: theme.radii.item).stroke(palette.hairline.color, lineWidth: 1)
                            HStack(spacing: 6) {
                                Text(verbatim: volume.name)
                                    .font(theme.type.caption.font)
                                    .foregroundStyle(palette.ink.color)
                                    .lineLimit(1)
                                    // The name is the longest: served first, so an equal share does not cut it (Fit.swift).
                                    .layoutPriority(Fit.priority(windows: 3))
                                Text(verbatim: StorageText.bytes(volume.total_bytes ?? 0))
                                    .font(theme.type.caption.font)
                                    .foregroundStyle(palette.ink2.color)
                                    .lineLimit(1)
                                StorageButton(title: "Eject", weight: .ghost) { model.eject(volume) }
                                    .disabled(model.ejecting == volume.mount_point)
                            }
                            .padding(.leading, 10)
                            .padding(.trailing, 2)
                            .padding(.vertical, 2)
                        }
                    }
                    Spacer(minLength: 0)
                }
            }
            if let message = model.ejectError {
                Text(verbatim: message).font(theme.type.caption.font).foregroundStyle(palette.bad.color)
            }
        }
    }

    // MARK: Notices

    @ViewBuilder
    private var notices: some View {
        let volume = model.activeVolume
        if let volume, volume.isNetwork, !model.scanning, model.folder == nil {
            StorageNotice(
                text: volume.isResponding
                    ? "This is a network volume. Scanning it reads every folder over the network."
                    : "Not responding. The share did not answer in time, so nothing has been read from it.",
                actionTitle: volume.isResponding ? "Scan anyway" : nil,
                actionDisabled: model.busy,
                action: volume.isResponding ? { model.rescan() } : nil
            )
        }
        if let error = model.error { StorageNotice(text: error, tone: .bad) }
        if let note = model.compressNote {
            StorageNotice(text: note, actionTitle: "Dismiss", actionDisabled: false, action: { model.compressNote = nil })
        }
        if let undo = model.undo {
            StorageNotice(text: undo.text, actionTitle: "Undo", actionDisabled: model.busy, action: { model.undoMove() })
        }
        if let folder = model.folder {
            let needsAccess = folder.needs_access == true
            let accessMissing = model.access != nil && model.access != "granted"
            if needsAccess && accessMissing && model.results == nil {
                StorageNotice(
                    text: StorageText.accessNotice,
                    tone: .warn,
                    actionTitle: StorageText.accessButton,
                    action: { model.openFullDiskAccess() }
                )
            }
            if folder.incomplete == true && model.results == nil && !(needsAccess && accessMissing) {
                let unread = folder.unread ?? 0
                muted(
                    (unread > 0
                        ? "\(StorageText.count(unread)) \(unread == 1 ? "item" : "items") couldn't be read"
                        : "Some items couldn't be read")
                        + " (long paths, access denied, or the scan's entry limit)"
                )
            }
            if !needsAccess && folder.limited == true && model.results == nil {
                muted("This is a very large folder, so sizes may be a little low.")
            }
        }
    }

    // MARK: Findings

    @ViewBuilder
    private var findings: some View {
        if model.internalActive {
            StorageFindingsView(model: model)
        } else {
            StorageEmpty(
                symbol: "checkmark.shield",
                title: "Findings cover your home folder",
                actionTitle: "Browse this drive",
                action: { model.choosePane(.folders) }
            )
        }
    }
}
#endif

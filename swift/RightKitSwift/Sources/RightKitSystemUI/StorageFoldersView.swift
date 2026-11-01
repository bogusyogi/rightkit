#if os(macOS)
import RightKitHub
import SwiftUI

/// The Folders workspace: breadcrumbs, search, Kind / Size / Changed filters, the folder as one
/// proportional bar, the list with percent bars, and the inspector of the selected item.
struct StorageFoldersView: View {
    let model: StorageModel
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    private func muted(_ text: String) -> some View {
        Text(verbatim: text).font(theme.type.caption.font).foregroundStyle(palette.ink2.color)
    }

    var body: some View {
        let listing = model.listing
        let selectedRow = model.selected.flatMap { path in listing.allRows.first { $0.path == path } }
        StorageCard {
            VStack(alignment: .leading, spacing: theme.spacing.sm) {
                if let results = model.results {
                    muted(StorageText.plural(results.count, "match"))
                } else {
                    crumbs
                }
                StorageSearchField(
                    placeholder: "Search files",
                    label: "Search files",
                    text: Binding(get: { model.query }, set: { model.setQuery($0) })
                )
                filters(listing)
                if let menu = model.filterMenu { filterChoices(menu) }
                if !listing.allRows.isEmpty && listing.undated > 0 {
                    // With the count on the filter row, only a long "no date" note needs a line of its own.
                    muted(Self.countText(listing))
                }
                states(listing)
                if !listing.rows.isEmpty {
                    if model.results == nil { strip(listing.rows) }
                    HStack(alignment: .top, spacing: theme.spacing.md) {
                        VStack(spacing: 0) {
                            ForEach(listing.rows) { row in
                                rowView(row, listing: listing, compact: selectedRow != nil)
                            }
                        }
                        .frame(maxWidth: .infinity)
                        if let selectedRow {
                            StorageInspectorView(
                                model: model,
                                row: selectedRow,
                                parentBytes: model.results == nil ? listing.total : nil
                            )
                            .frame(width: 280)
                        }
                    }
                }
            }
        }
    }

    // MARK: Breadcrumbs

    private var crumbs: some View {
        let parts = model.crumbs
        return ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 0) {
                ForEach(Array(parts.enumerated()), id: \.offset) { item in
                    if item.offset > 0 {
                        Image(systemName: "chevron.right")
                            .font(Font.system(size: 9))
                            .foregroundStyle(palette.ink3.color)
                    }
                    Button {
                        model.open(item.element.path)
                    } label: {
                        Text(verbatim: item.element.name)
                            .font(item.offset == parts.count - 1 ? Font.system(size: 12, weight: .semibold) : theme.type.caption.font)
                            .foregroundStyle(item.offset == parts.count - 1 ? palette.ink.color : palette.ink2.color)
                            .padding(.horizontal, 4)
                            .padding(.vertical, 4)
                    }
                    .buttonStyle(.plain)
                    .disabled(model.busy)
                    .accessibilityAddTraits(item.offset == parts.count - 1 ? .isSelected : [])
                }
            }
        }
    }

    // MARK: Filters

    /// "3 of 3 shown", with how many items had no date when a date filter is on.
    private static func countText(_ listing: StorageModel.Listing) -> String {
        "\(StorageText.count(listing.rows.count)) of \(StorageText.count(listing.allRows.count)) shown"
            + (listing.undated > 0 ? " · \(StorageText.count(listing.undated)) \(listing.undated == 1 ? "item has" : "items have") no date" : "")
    }

    /// Kind, Size and Changed selects, Clear filters, and the count on the right: the page's filter row.
    private func filters(_ listing: StorageModel.Listing) -> some View {
        HStack(spacing: 8) {
            filterButton("Kind", model.kindFilter.label, .kind)
            filterButton("Size", StorageSizeFilter.label(model.minSize), .size)
            filterButton("Changed", model.effectiveChanged.label, .changed, enabled: model.hasDates)
            if listing.filtersOn {
                StorageButton(title: "Clear filters", weight: .ghost) { model.clearFilters() }
            }
            Spacer(minLength: 0)
            if !listing.allRows.isEmpty && listing.undated == 0 {
                Text(verbatim: Self.countText(listing))
                    .font(theme.type.caption.font)
                    .foregroundStyle(palette.ink2.color)
                    .lineLimit(1)
            }
        }
    }

    /// A select of the page, as a button named by what it chooses ("Kind") and holding its value.
    private func filterButton(_ name: String, _ value: String, _ menu: StorageModel.FilterMenu, enabled: Bool = true) -> some View {
        let open = model.filterMenu == menu
        return StorageSelect(label: name, value: value, open: open, enabled: enabled) {
            model.volumeMenu = false
            model.filterMenu = open ? nil : menu
        }
        // The page shows "Scan again to filter by date" as a tooltip; the disabled filter explains itself in the count line.
    }

    @ViewBuilder
    private func filterChoices(_ menu: StorageModel.FilterMenu) -> some View {
        HubLayered {
            RoundedRectangle(cornerRadius: theme.radii.item).fill(palette.plane.color)
            RoundedRectangle(cornerRadius: theme.radii.item).stroke(palette.line.color, lineWidth: 1)
            VStack(alignment: .leading, spacing: 0) {
                switch menu {
                case .kind:
                    HubChoiceList(
                        options: StorageKindFilter.allCases.map { HubChoiceOption(value: $0, title: $0.label) },
                        selection: Binding(get: { model.kindFilter }, set: { model.setKindFilter($0) })
                    )
                case .size:
                    HubChoiceList(
                        options: StorageSizeFilter.options,
                        selection: Binding(get: { model.minSize }, set: { model.setMinSize($0) })
                    )
                case .changed:
                    HubChoiceList(
                        options: StorageChangedFilter.allCases.map { HubChoiceOption(value: $0, title: $0.label) },
                        selection: Binding(get: { model.effectiveChanged }, set: { model.setChangedFilter($0) })
                    )
                }
            }
            .padding(.horizontal, theme.spacing.md)
            .padding(.vertical, theme.spacing.xs)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    // MARK: States

    @ViewBuilder
    private func states(_ listing: StorageModel.Listing) -> some View {
        if model.folder == nil {
            if model.opening {
                muted("Loading…")
            } else if model.scanning {
                muted("Scanning… \(model.scanSeconds) s")
            }
        }
        if model.folder != nil && listing.allRows.isEmpty && !model.busy && !model.scanning {
            StorageEmpty(symbol: "folder", title: model.results != nil ? "No matches" : "This folder is empty")
        }
        if !listing.allRows.isEmpty && listing.rows.isEmpty {
            StorageEmpty(
                symbol: "folder",
                title: "No items match these filters",
                actionTitle: "Clear filters",
                action: { model.clearFilters() }
            )
        }
    }

    // MARK: Proportional bar

    /// The current folder as one level of proportional segments, coloured by kind (the page drew a
    /// treemap).
    @ViewBuilder
    private func strip(_ rows: [StorageRow]) -> some View {
        let items = Array(rows.filter { $0.bytes > 0 }.prefix(40))
        if !items.isEmpty {
            VStack(alignment: .leading, spacing: 4) {
                StorageStrip(
                    segments: items.map {
                        StorageStrip.Segment(
                            id: $0.path,
                            weight: Double($0.bytes),
                            color: StorageKind.of(path: $0.path, isDir: $0.is_dir).color,
                            label: $0.name
                        )
                    },
                    height: 96
                )
                .accessibilityElement(children: .ignore)
                .accessibilityLabel("Folder contents by size")
                .accessibilityValue(items.prefix(3).map { "\($0.name) \(StorageText.bytes($0.bytes))" }.joined(separator: ", "))
            }
        }
    }

    // MARK: Rows

    private func percentText(_ bytes: Int64, total: Int64) -> String {
        let pct = Double(bytes) / Double(total) * 100
        return pct >= 1 ? "\(Int(pct.rounded()))%" : "<1%"
    }

    private func rowView(_ row: StorageRow, listing: StorageModel.Listing, compact: Bool) -> some View {
        let kind = StorageKind.of(path: row.path, isDir: row.is_dir)
        let chosen = model.selected == row.path
        let pct = percentText(row.bytes, total: listing.total)
        let content = HStack(spacing: 8) {
            RoundedRectangle(cornerRadius: 4).fill(kind.color).frame(width: 8, height: 8)
            Image(systemName: row.is_dir ? "folder" : "doc")
                .font(Font.system(size: 12))
                .foregroundStyle(palette.ink3.color)
            Text(verbatim: model.results != nil ? StorageText.shortPath(row.path) : row.name)
                .font(theme.type.body.font)
                .foregroundStyle(palette.ink.color)
                .lineLimit(1)
                .frame(maxWidth: .infinity, alignment: .leading)
            StorageFractionBar(
                fraction: Double(row.bytes) / Double(listing.largest),
                color: kind.color,
                height: 6,
                label: "Size of \(row.name)",
                value: "\(pct) of this listing, \(kind.label)",
                track: palette.plane.color
            )
            .frame(width: compact ? 56 : 90)
            Text(verbatim: pct)
                .font(theme.type.caption.font)
                .foregroundStyle(palette.ink2.color)
                .frame(width: 34, alignment: .trailing)
            Text(verbatim: StorageText.bytes(row.bytes))
                .font(Font.system(size: 13, weight: .semibold))
                .foregroundStyle(palette.ink.color)
                .lineLimit(1)
                .frame(width: 64, alignment: .trailing)
        }
        .padding(.vertical, 7)
        .padding(.horizontal, 4)
        return VStack(spacing: 0) {
            Divider()
            HubLayered {
                Rectangle().fill(chosen ? palette.plane.color : Color.clear)
                HStack(spacing: 0) {
                    Rectangle().fill(palette.accent.color).frame(width: 2).opacity(chosen ? 1 : 0)
                    if row.isActionable {
                        Button {
                            model.select(row.path)
                        } label: {
                            HubLayered {
                                HubHitArea()
                                content
                            }
                        }
                        .buttonStyle(.plain)
                        .accessibilityAddTraits(chosen ? .isSelected : [])
                    } else {
                        content
                    }
                    if row.isActionable {
                        // The page's "Actions for <name>" menu: a panel opens under the row with the same
                        // actions as the Tauri menu (Open or Reveal, Copy path, Copy name, Move to…, Move to the Trash).
                        Button {
                            model.rowMenu = model.rowMenu == row.path ? nil : row.path
                        } label: {
                            HStack(spacing: 2) {
                                ForEach(0..<3, id: \.self) { _ in
                                    RoundedRectangle(cornerRadius: 1.5).fill(palette.ink2.color).frame(width: 3, height: 3)
                                }
                            }
                            .frame(width: 28, height: 28)
                        }
                        .buttonStyle(.plain)
                        .accessibilityAddTraits(model.rowMenu == row.path ? .isSelected : [])
                        .accessibilityLabel("Actions for \(row.name)")
                    } else {
                        Color.clear.frame(width: 28, height: 28)
                    }
                    if row.isActionable && row.is_dir && model.results == nil {
                        Button {
                            model.open(row.path)
                        } label: {
                            Image(systemName: "chevron.right")
                                .font(Font.system(size: 11))
                                .foregroundStyle(palette.ink2.color)
                                .frame(width: 24, height: 24)
                        }
                        .buttonStyle(.plain)
                        .disabled(model.busy)
                        .accessibilityLabel("Open \(row.name)")
                    } else {
                        Color.clear.frame(width: 24, height: 24)
                    }
                }
            }
            if row.isActionable && model.rowMenu == row.path {
                StorageChoicePanel(choices: rowMenuChoices(row))
                    .padding(.bottom, 6)
                    .accessibilityLabel("Item actions")
            }
        }
    }

    /// The Tauri item menu's entries, in its order.
    private func rowMenuChoices(_ row: StorageRow) -> [StorageChoice] {
        func run(_ action: @escaping () -> Void) -> () -> Void {
            { model.rowMenu = nil; action() }
        }
        return [
            StorageChoice(id: "finder", title: "\(row.is_dir ? "Open" : "Reveal") in \(StorageText.fileManager)", action: run { model.showInFileManager(row.path) }),
            StorageChoice(id: "copy-path", title: "Copy path", action: run { model.copyText(row.path) }),
            StorageChoice(id: "copy-name", title: "Copy name", action: run { model.copyText(row.name) }),
            StorageChoice(id: "move", title: "Move to…", enabled: !model.busy, action: run { model.startMove(row) }),
            StorageChoice(id: "trash", title: "Move to \(StorageText.trash)", enabled: !model.busy, action: run { model.askTrash(row) }),
        ]
    }
}

// MARK: - Inspector

/// Facts about one selected item, with the same actions and confirmations as the page's right-click menu.
struct StorageInspectorView: View {
    let model: StorageModel
    let row: StorageRow
    /// The whole listing's bytes, for the share; nil for search results.
    let parentBytes: Int64?
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    private var info: StorageModel.Inspection? {
        guard let inspect = model.inspect, inspect.path == row.path else { return nil }
        return inspect
    }

    private func muted(_ text: String) -> some View {
        Text(verbatim: text).font(theme.type.caption.font).foregroundStyle(palette.ink2.color)
    }

    /// Unknown reads as "Unknown", never as zero; while reading, "Reading…".
    private func field(_ pick: (StorageInspect) -> String) -> String {
        guard let info else { return "Reading…" }
        guard let data = info.data else { return "Unknown" }
        return pick(data)
    }

    var body: some View {
        let kind = StorageKind.of(path: row.path, isDir: row.is_dir)
        let data = info?.data
        let difference = data?.logical_bytes.map { row.bytes - $0 }
        let share = parentBytes.map { Double(row.bytes) / Double($0) * 100 }
        let network = model.onNetwork(row.path)
        HubLayered {
            RoundedRectangle(cornerRadius: theme.radii.card).fill(palette.plane.color)
            RoundedRectangle(cornerRadius: theme.radii.card).stroke(palette.line.color, lineWidth: 1)
            VStack(alignment: .leading, spacing: 10) {
                HStack(alignment: .top, spacing: 6) {
                    Image(systemName: row.is_dir ? "folder" : "doc").foregroundStyle(palette.accent.color)
                    Text(verbatim: row.name)
                        .font(theme.type.title.font)
                        .foregroundStyle(palette.ink.color)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    StorageButton(title: "Close", symbol: "xmark", weight: .ghost) { model.select(nil) }
                }
                muted(row.path)
                StorageButton(title: "Copy path", symbol: "doc.on.doc") { model.copyText(row.path) }
                preview(network: network)
                VStack(alignment: .leading, spacing: 4) {
                    StorageFact(label: "Kind", value: kind.label)
                    StorageFact(
                        label: "Logical size",
                        value: row.is_dir
                            ? (info != nil ? "Unknown" : "Reading…")
                            : field { $0.logical_bytes.map(StorageText.bytes) ?? "Unknown" }
                    )
                    StorageFact(label: "Allocated size", value: StorageText.bytes(row.bytes))
                    if let difference, abs(difference) >= 1_000_000 {
                        StorageFact(label: "Allocated minus logical", value: StorageText.signedBytes(difference))
                    }
                    if let share {
                        StorageFact(label: "Share of this folder", value: share >= 1 ? "\(Int(share.rounded()))%" : "<1%")
                    }
                    if row.is_dir {
                        StorageFact(label: "Files directly inside", value: field { $0.files.map { StorageText.count($0) } ?? "Unknown" })
                        StorageFact(label: "Folders directly inside", value: field { $0.folders.map { StorageText.count($0) } ?? "Unknown" })
                    }
                    StorageFact(label: "Created", value: field { $0.created_at.map(StorageText.localTime) ?? "Unknown" })
                    StorageFact(label: "Modified", value: modifiedText)
                }
                if let message = info?.error { muted(message) }
                if let draft = model.moveDraft, draft.row.path == row.path { moveForm(draft) }
                actions
                muted(
                    "Allocated is the disk space the scan counts for this item. Logical is the file's own length, which can be smaller for sparse or compressed files. Folders have no logical size here, because it is not added up. Removing this item could free up to its allocated size once the \(StorageText.trash) is emptied; hard links and clones can make the real gain smaller. This view does not show how the volume's free space changed. Folder counts cover direct children only."
                )
            }
            .padding(.horizontal, theme.spacing.md)
            .padding(.vertical, theme.spacing.row)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private var modifiedText: String {
        if info != nil || (row.modified ?? 0) == 0 {
            return field { $0.modified_at.map(StorageText.localTime) ?? "Unknown" }
        }
        return "\(StorageText.localTime(row.modified ?? 0))\(row.is_dir ? " (newest inside)" : "")"
    }

    /// The image itself, for a selected image file only (the page's `ItemPreview`). The model reads it
    /// (`storage_preview`) and the platform services decode it; nothing is read for other kinds, and
    /// nothing on a network volume. The page draws it at most 200 px high and as wide as the inspector.
    @ViewBuilder
    private func preview(network: Bool) -> some View {
        if model.isPreviewable(row) {
            if network {
                muted("No preview on network drives")
            } else if let shown = model.preview, shown.path == row.path {
                switch shown.phase {
                case .reading:
                    muted("Reading preview…")
                case .failed(let reason):
                    muted(reason)
                case .ready(let bytes):
                    previewImage(bytes)
                }
            } else {
                muted("Reading preview…")
            }
        }
    }

    private func previewImage(_ bytes: Data) -> some View {
        HubImageView(
            data: bytes,
            maxPixel: StorageModel.previewPixels,
            fitting: CGSize(width: 256, height: 200)
        ) {
            muted("Reading preview…")
        }
        .overlay {
            Rectangle().stroke(palette.hairline.color, lineWidth: 1)
        }
        .accessibilityLabel("Preview of \(row.name)")
    }

    private var actions: some View {
        VStack(alignment: .leading, spacing: 6) {
            if row.is_dir {
                StorageButton(title: "Open folder", symbol: "folder", enabled: !model.busy) { model.open(row.path) }
            }
            StorageButton(title: "\(row.is_dir ? "Open" : "Reveal") in \(StorageText.fileManager)") {
                model.showInFileManager(row.path)
            }
            StorageButton(title: "Copy name", symbol: "doc.on.doc") { model.copyText(row.name) }
            StorageButton(title: "Move to…", symbol: "folder.badge.plus", enabled: !model.busy) { model.startMove(row) }
            if model.canCompress(row) {
                StorageButton(title: "Compress a copy", symbol: "arrow.down.right.and.arrow.up.left", enabled: !model.busy) { model.compress(row) }
            }
            StorageButton(title: "Move to \(StorageText.trash)", symbol: "trash", weight: .ghost) { model.askTrash(row) }
                .disabled(model.busy)
        }
    }

    /// The destination of "Move to…", typed here. Only shown where the platform has no folder chooser
    /// (`chooseFolder` unsupported); elsewhere "Move to…" opens the chooser and never shows this form.
    private func moveForm(_ draft: StorageModel.MoveDraft) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(verbatim: "Move \(row.name) into which folder?")
                .font(theme.type.label.font)
                .foregroundStyle(palette.ink.color)
            HubLayered {
                RoundedRectangle(cornerRadius: theme.radii.field).fill(palette.fill2.color)
                TextField(
                    "/Users/you/Documents",
                    text: Binding(get: { draft.destination }, set: { model.setMoveDestination($0) })
                )
                .textFieldStyle(.plain)
                .font(theme.type.body.font)
                .foregroundStyle(palette.ink.color)
                .onSubmit { model.planMove() }
                .padding(.horizontal, theme.spacing.sm + 2)
                .padding(.vertical, 5)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .accessibilityLabel("Destination folder")
            HStack(spacing: 6) {
                StorageButton(title: "Move here", weight: .primary) { model.planMove() }
                    .disabled(model.busy || draft.destination.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                StorageButton(title: "Cancel", weight: .ghost) { model.cancelMove() }
            }
        }
    }
}
#endif

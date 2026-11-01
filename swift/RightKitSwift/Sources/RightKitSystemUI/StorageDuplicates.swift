#if os(macOS)
import Foundation
import Observation
import RightKitHub
import SwiftUI

/// Exact-content duplicates under a folder: the `Duplicates` panel of the page. Extra copies move to
/// the Trash after the engine re-checks them; the kept copy stays.
@MainActor
@Observable
final class StorageDuplicatesModel {
    struct Item: Sendable { let kept: String; let path: String; let bytes: Int64 }
    struct Pending { let title: String; let items: [Item] }

    @ObservationIgnored let ctx: HubContext
    var folder = ""
    var draft = ""
    var report: StorageDuplicateReport?
    var removed: Set<String> = []
    var busy = false
    var error: String?
    var notice: String?
    var pending: Pending?

    init(ctx: HubContext) {
        self.ctx = ctx
    }

    func prepare() async {
        guard folder.isEmpty, draft.isEmpty else { return }
        if let home = try? await ctx.engine.call("home_path", EngineEmpty(), as: String.self) {
            if draft.isEmpty { draft = home }
            if folder.isEmpty { folder = home }
        }
    }

    func useHome() {
        Task { [self] in
            if let home = try? await ctx.engine.call("home_path", EngineEmpty(), as: String.self) { draft = home }
        }
    }

    func setDraft(_ value: String) { draft = value }

    /// Looks under the chosen folder. The engine bounds the file count, bytes read and time, and runs
    /// it off the UI thread, so the rest of the window stays responsive.
    func find() {
        let path = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        if path.isEmpty || busy { return }
        busy = true
        error = nil
        notice = nil
        pending = nil
        Task { [self] in
            do {
                let next = try await ctx.engine.call("duplicates_scan", StoragePathParams(path: path), as: StorageDuplicateReport.self, timeout: .seconds(1800))
                folder = path
                report = next
                removed = []
            } catch {
                report = nil
                self.error = StorageText.describe(error)
            }
            busy = false
        }
    }

    func moveToTrash(_ items: [Item]) {
        pending = nil
        busy = true
        error = nil
        notice = nil
        Task { [self] in
            do {
                let result = try await ctx.engine.call(
                    "duplicates_trash",
                    StorageDuplicatesTrashParams(items: items.map { StorageDuplicateTrashItem(kept: $0.kept, path: $0.path) }),
                    as: StorageDuplicatesTrashResult.self,
                    timeout: .seconds(900)
                )
                removed.formUnion(result.moved.map(\.path))
                notice = "Moved \(StorageText.copies(result.moved.count)) to the \(StorageText.trash), \(StorageText.bytes(result.moved_bytes)). The kept copies are untouched."
                    + (result.skipped.isEmpty ? "" : " \(result.skipped.count) skipped.")
                if !result.skipped.isEmpty {
                    error = result.skipped.map { "\(StorageText.shortPath($0.path)): \($0.reason)" }.joined(separator: "\n")
                }
            } catch {
                self.error = StorageText.describe(error)
            }
            busy = false
        }
    }

    /// The groups still holding extra copies.
    var groups: [StorageDuplicateGroup] {
        (report?.groups ?? []).compactMap { group in
            let extras = group.extras.filter { !removed.contains($0) }
            return extras.isEmpty ? nil : StorageDuplicateGroup(kept_path: group.kept_path, extras: extras, size_bytes: group.size_bytes)
        }
    }

    func items(kept: String, size: Int64, paths: [String]) -> [Item] {
        paths.map { Item(kept: kept, path: $0, bytes: size) }
    }
}

struct StorageDuplicatesView: View {
    let model: StorageDuplicatesModel
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    private func muted(_ text: String) -> some View {
        Text(verbatim: text).font(theme.type.caption.font).foregroundStyle(palette.ink2.color)
    }

    var body: some View {
        let groups = model.groups
        let reclaimable = groups.reduce(Int64(0)) { $0 + Int64($1.extras.count) * $1.size_bytes }
        let pendingSnapshot = model.pending
        VStack(alignment: .leading, spacing: theme.spacing.sm + 2) {
            controls
            if model.busy {
                muted("Reading files in \(StorageText.shortPath(model.draft.trimmingCharacters(in: .whitespacesAndNewlines))) to compare them. This can take a little while.")
            }
            if let notice = model.notice { StorageNotice(text: notice) }
            if let error = model.error { StorageNotice(text: error, tone: .bad) }

            if let report = model.report, !model.busy {
                muted(summary(report, groups: groups, reclaimable: reclaimable))
                ForEach(report.diagnostics, id: \.self) { line in muted(line) }
            }

            if model.report == nil && !model.busy && model.error == nil {
                StorageEmpty(symbol: "doc.on.doc", title: "Choose a folder, then find copies")
            }

            ForEach(groups, id: \.kept_path) { group in
                groupCard(group)
            }
        }
        .task { await model.prepare() }
        .hubConfirm(
            pendingSnapshot.map { $0.items.count == 1 ? "Move this copy to the \(StorageText.trash)?" : "Move \($0.items.count) copies to the \(StorageText.trash)?" } ?? "",
            message: pendingSnapshot.map {
                "\($0.title): \(StorageText.copies($0.items.count)), \(StorageText.bytes($0.items.reduce(0) { $0 + $1.bytes })) will move to the \(StorageText.trash). The copy you keep stays where it is. Nothing is deleted: you can put them back from the \(StorageText.trash)."
            },
            confirmLabel: "Move to \(StorageText.trash)",
            isPresented: Binding(get: { model.pending != nil }, set: { if !$0 { model.pending = nil } })
        ) {
            if let pendingSnapshot { model.moveToTrash(pendingSnapshot.items) }
        }
    }

    private func summary(_ report: StorageDuplicateReport, groups: [StorageDuplicateGroup], reclaimable: Int64) -> String {
        var text: String
        if groups.isEmpty {
            text = "No extra copies in \(StorageText.shortPath(model.folder))."
        } else {
            let extras = groups.reduce(0) { $0 + $1.extras.count }
            text = "\(StorageText.copies(extras)) in \(groups.count) group\(groups.count == 1 ? "" : "s") · \(StorageText.bytes(reclaimable)) can be freed by moving the extras to the \(StorageText.trash)"
        }
        text += " · checked \(StorageText.count(report.files_considered)) files"
        if report.truncated { text += " · the folder is very large, so only part of it was checked" }
        if !report.skipped.isEmpty { text += " · \(report.skipped.count) skipped (links, cloud placeholders, unreadable)" }
        return text
    }

    private var controls: some View {
        HStack(spacing: theme.spacing.sm) {
            HubLayered {
                RoundedRectangle(cornerRadius: theme.radii.field).fill(palette.fill2.color)
                RoundedRectangle(cornerRadius: theme.radii.field).stroke(palette.hairline.color, lineWidth: 1)
                TextField(
                    "/Users/you/Documents",
                    text: Binding(get: { model.draft }, set: { model.setDraft($0) })
                )
                .textFieldStyle(.plain)
                .font(theme.type.body.font)
                .foregroundStyle(palette.ink.color)
                .accessibilityLabel("Folder to check for duplicates")
                .onSubmit { model.find() }
                .padding(.horizontal, theme.spacing.md)
                .padding(.vertical, 6)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            StorageButton(title: "Home", weight: .outline, enabled: !model.busy) { model.useHome() }
            StorageButton(
                title: model.busy ? "Looking…" : "Find copies",
                symbol: "doc.on.doc",
                enabled: !(model.busy || model.draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            ) { model.find() }
        }
    }

    private func groupCard(_ group: StorageDuplicateGroup) -> some View {
        let all = model.items(kept: group.kept_path, size: group.size_bytes, paths: group.extras)
        return StorageCard {
            VStack(alignment: .leading, spacing: 4) {
                HStack {
                    Text(verbatim: "\(StorageText.copies(group.extras.count + 1)) · \(StorageText.bytes(group.size_bytes)) each")
                        .font(theme.type.label.font)
                        .foregroundStyle(palette.ink.color)
                    Spacer()
                    StorageButton(title: "Move \(all.count) extra\(all.count == 1 ? "" : "s") to \(StorageText.trash)", enabled: !model.busy) {
                        model.pending = StorageDuplicatesModel.Pending(
                            title: "\(StorageText.copies(all.count)) of \(StorageText.shortPath(group.kept_path))", items: all
                        )
                    }
                }
                HStack(spacing: theme.spacing.row) {
                    StorageBadge(text: "Keep", tone: .ok)
                    Text(verbatim: StorageText.shortPath(group.kept_path))
                        .font(theme.type.caption.font)
                        .foregroundStyle(palette.ink.color)
                        .lineLimit(1)
                    Spacer()
                }
                .padding(.top, 4)
                ForEach(group.extras, id: \.self) { path in
                    VStack(spacing: 0) {
                        Divider()
                        HStack(spacing: theme.spacing.row) {
                            Text(verbatim: StorageText.shortPath(path))
                                .font(theme.type.caption.font)
                                .foregroundStyle(palette.ink.color)
                                .lineLimit(1)
                            Spacer()
                            Text(verbatim: StorageText.bytes(group.size_bytes))
                                .font(theme.type.caption.font)
                                .foregroundStyle(palette.ink2.color)
                            StorageButton(title: "Move to \(StorageText.trash)", weight: .ghost) {
                                model.pending = StorageDuplicatesModel.Pending(
                                    title: StorageText.shortPath(path),
                                    items: model.items(kept: group.kept_path, size: group.size_bytes, paths: [path])
                                )
                            }
                            .disabled(model.busy)
                        }
                        .padding(.leading, 18)
                        .padding(.vertical, 2)
                    }
                }
            }
        }
    }
}
#endif

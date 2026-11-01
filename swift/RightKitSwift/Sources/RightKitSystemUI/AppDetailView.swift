#if os(macOS)
import RightKitHub
import SwiftUI

/// One app's detail: what Pulse found of it, what to move, and the uninstall.
struct AppDetailView: View {
    let list: AppsModel
    @Bindable var detail: AppDetailModel
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    private var app: AppsAPI.Entry { detail.app }

    private var subtitle: String {
        [app.version, app.bundleId, "Last used: \(AppsModel.lastUsedText(app.lastUsed))"]
            .compactMap { $0 }
            .joined(separator: " · ")
    }

    var body: some View {
        VStack(spacing: 0) {
            ScrollView {
                VStack(alignment: .leading, spacing: 8) {
                    header
                    notices
                    card
                }
                .padding(.horizontal, 18)
                .padding(.vertical, 14)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            if app.protectedReason == nil {
                foot
            }
        }
        .hubConfirm(
            detail.confirmTitle,
            message: detail.confirmMessage,
            confirmLabel: detail.confirmLabel,
            isPresented: $detail.confirmOpen
        ) {
            Task { await detail.uninstall() }
        }
    }

    /// The associated files' card (`.ck-apps-card`): a grey panel with a hairline outline.
    private var card: some View {
        HubLayered {
            RoundedRectangle(cornerRadius: theme.radii.card).fill(palette.fill2.color)
            RoundedRectangle(cornerRadius: theme.radii.card).stroke(palette.hairline.color, lineWidth: 1)
            VStack(alignment: .leading, spacing: 0) {
                content
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 2)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private var header: some View {
        HStack(spacing: 10) {
            ACButton("Back to apps", accessibility: "Back to apps") { list.closeDetail() }
            VStack(alignment: .leading, spacing: 2) {
                ACText(app.name, .title, lines: 1)
                ACText(subtitle, .caption, ink: .secondary, lines: 1)
            }
            Spacer(minLength: 0)
            UpdateControlView(
                update: list.updates[app.path],
                job: list.jobs[app.path]
            ) { list.startUpdate(path: app.path) }
            ACButton("Reveal", enabled: !app.path.isEmpty, accessibility: "Reveal \(app.name)") {
                Task { await detail.reveal(app.path) }
            }
        }
    }

    @ViewBuilder
    private var notices: some View {
        if let reason = app.protectedReason {
            ACNoticeText("\(reason) Uninstall is not offered.", tone: .warn)
        }
        if app.running && app.protectedReason == nil {
            ACNoticeText(
                "Running. Pulse will ask it to quit first, and will not remove anything if it stays open.",
                tone: .warn
            )
        }
        if detail.status == .loading {
            ACNoticeText("Finding leftovers…")
        }
        if let message = detail.leftoverError {
            ACNoticeText(message, tone: .bad)
        }
        if let message = detail.error {
            ACNoticeText(message, tone: .bad)
        }
        if let result = detail.result {
            resultNotice(result)
        }
    }

    private func resultNotice(_ result: AppsAPI.UninstallResult) -> some View {
        let up = "up to \(AppsFormat.bytes(result.bytesMoved))"
        let moved = result.moved.count
        let text: String
        if ACPlatform.isWindows {
            let extra =
                moved > 0
                ? " · moved \(moved) leftover\(moved == 1 ? "" : "s") to the Recycle Bin: \(up) on disk · restore from the Recycle Bin · Space is not freed until the Recycle Bin is emptied."
                : ""
            text = "Started the uninstaller\(extra)"
        } else {
            text =
                "Moved \(moved) to the Trash: \(up) on disk · restore with Put Back in Finder · Space is not freed until the Trash is emptied."
        }
        return ACNotice(tone: result.failed.isEmpty ? .ok : .warn) {
            ACText(text, .caption, ink: result.failed.isEmpty ? .ok : .warn)
            if !result.failed.isEmpty {
                ACText("\(result.failed.count) failed and still listed", .caption, ink: .warn)
                ForEach(result.failed, id: \.path) { failure in
                    ACText("\(ACFormat.shortPath(failure.path)): \(failure.error)", .caption, ink: .warn)
                }
            }
        }
    }

    // MARK: Associated files

    @ViewBuilder
    private var content: some View {
        ForEach(AppDetailModel.locations, id: \.self) { location in
            group(location)
        }
        if !detail.background.isEmpty {
            VStack(alignment: .leading, spacing: 4) {
                ACText("Login and background items", .label)
                ForEach(detail.background, id: \.self) { entry in
                    ACText("\(entry.kind) · \(entry.label)", .caption, ink: .secondary)
                }
            }
            .padding(.vertical, 6)
        }
        if !detail.receipts.isEmpty {
            ACText("Installer packages: \(detail.receipts.joined(separator: ", "))", .caption, ink: .secondary)
                .padding(.vertical, 6)
        }
        AppsDisclosure(
            "About these numbers",
            isOpen: detail.aboutOpen,
            toggle: { detail.aboutOpen.toggle() }
        ) {
            ACText(aboutText, .caption, ink: .secondary)
                .frame(maxWidth: 560, alignment: .leading)
        }
        .padding(.vertical, 4)
    }

    private var aboutText: String {
        let tail =
            ACPlatform.isWindows
            ? "Leftover folders go to the Recycle Bin and can be restored. Registry and startup entries are listed only; Pulse never removes them."
            : "Everything goes to the Trash and can be put back."
        return
            "Sizes are measured on disk. Match shows how the file was tied to the app; items marked for review are left unchecked. \(tail)"
    }

    @ViewBuilder
    private func group(_ location: String) -> some View {
        let rows = detail.rows(in: location)
        if !rows.isEmpty {
            let paths = rows.map(\.path)
            let allOn = paths.allSatisfy { detail.picked.contains($0) }
            let size = rows.reduce(Int64(0)) { $0 + $1.sizeBytes }
            VStack(alignment: .leading, spacing: 0) {
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Text(verbatim: location)
                        .font(Font.system(size: 13, weight: .semibold))
                        .foregroundStyle(palette.ink.color)
                        .lineLimit(1)
                    ACText("· \(rows.count) · \(AppsFormat.bytes(size))", .body, ink: .tertiary, lines: 1)
                    Spacer(minLength: 0)
                    if rows.count > 1 {
                        AppsButton(allOn ? "Deselect group" : "Select group", enabled: !detail.busy) {
                            detail.set(paths, on: !allOn)
                        }
                    }
                }
                .frame(minHeight: 36)
                ForEach(rows) { item in
                    Divider()
                    itemRow(item)
                }
            }
            .padding(.bottom, 6)
            .accessibilityLabel(location)
        }
    }

    private func itemRow(_ item: AppsAPI.Related) -> some View {
        let on = detail.picked.contains(item.path)
        return HStack(alignment: .top, spacing: 8) {
            ACCheckbox(
                state: on ? .on : .off,
                label: "Select \(ACFormat.shortPath(item.path))",
                enabled: !detail.busy
            ) { detail.toggle(item.path) }
            VStack(alignment: .leading, spacing: 3) {
                Button {
                    detail.toggle(item.path)
                } label: {
                    HubLayered {
                        HubHitArea()
                        VStack(alignment: .leading, spacing: 2) {
                            Text(verbatim: ACFormat.shortPath(item.path))
                                .font(Font.system(size: 13, weight: .semibold))
                                .foregroundStyle(palette.ink.color)
                            ACText(item.reason, .caption, ink: .secondary)
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
                .buttonStyle(.plain)
                .disabled(detail.busy)
                .accessibilityLabel("Select \(ACFormat.shortPath(item.path))")
                HStack(spacing: 6) {
                    AppsBadge("Match: \(AppDetailModel.confidence[item.confidence] ?? item.confidence)")
                    if item.admin { AppsBadge("Admin required", tone: .warn) }
                    if !item.preselected { AppsBadge("Review before moving", tone: .warn) }
                    AppsButton(
                        "Reveal",
                        enabled: !item.path.isEmpty,
                        accessibility: "Reveal \(item.path)"
                    ) { Task { await detail.reveal(item.path) } }
                }
            }
            ACText(AppsFormat.bytes(item.sizeBytes), .body, ink: .secondary, lines: 1)
                .padding(.top, 5)
        }
        .padding(.vertical, 6)
    }

    // MARK: Foot

    private var foot: some View {
        let count = detail.pickedVisible.count
        var notes = ""
        if detail.adminPicked > 0 { notes += "\(detail.adminPicked) need an administrator password · " }
        if app.running { notes += "Must quit first · " }
        notes +=
            ACPlatform.isWindows
            ? "Runs the app's uninstaller; leftovers go to the Recycle Bin"
            : "Goes to Trash, restore with Put Back"
        return HStack(spacing: 10) {
            VStack(alignment: .leading, spacing: 2) {
                Text(verbatim: "\(count) selected · \(AppsFormat.bytes(detail.totalBytes))")
                    .font(Font.system(size: 13, weight: .semibold))
                    .foregroundStyle(palette.ink.color)
                ACText(notes, .caption, ink: .secondary)
            }
            Spacer(minLength: 0)
            ACButton(
                detail.busy ? "Working…" : (ACPlatform.isWindows ? "Uninstall" : "Move to Trash"),
                tone: .danger,
                enabled: !(detail.busy || detail.status == .loading || count == 0)
            ) { detail.confirmOpen = true }
        }
        .padding(.horizontal, 18)
        .padding(.top, 8)
        .padding(.bottom, 12)
    }
}
#endif

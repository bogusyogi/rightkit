#if os(macOS)
import RightKitHub
import SwiftUI

/// The Apps section: the list of installed apps, or one app's detail.
struct AppsRootView: View {
    @State private var model: AppsModel
    private let loadsOnAppear: Bool

    /// - Parameter loadsOnAppear: False when the model was loaded already (the off-screen renderer).
    init(model: AppsModel, loadsOnAppear: Bool) {
        _model = State(initialValue: model)
        self.loadsOnAppear = loadsOnAppear
    }

    var body: some View {
        VStack(spacing: 0) {
            if let detail = model.open {
                AppDetailView(list: model, detail: detail)
            } else {
                AppsListView(model: model)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .task {
            // Live: follow the engine's events; the first one starts the load (see `AppsModel.run`).
            if loadsOnAppear { await model.run() }
        }
    }
}

/// The list: tools, filters, the apps largest first, and the "Starts by itself" section.
struct AppsListView: View {
    @Bindable var model: AppsModel
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    /// The height of the app list's card. The page's card fills what the window leaves between the
    /// filters and "About these numbers", which is this at the default 900 x 600 window; the page
    /// scrolls when the window is shorter. SwiftRight proves no way to read the window's height.
    private static let cardHeight: CGFloat = 355

    private var summary: String {
        let shown = model.shown
        let total = shown.reduce(Int64(0)) { $0 + $1.sizeBytes }
        var text = "\(shown.count) of \(model.apps.count) apps · \(AppsFormat.bytes(total)) · largest first"
        if model.refreshing { text += " · sizes updating…" }
        if model.checking {
            text += " · checking for updates…"
        } else if let at = model.updatesAt {
            text += " · updates checked \(ACFormat.ago(at))"
        }
        return text
    }

    private var filterOptions: [ACSegmentOption<AppsFilter>] {
        let count = model.updateCount
        return [
            ACSegmentOption(value: .all, label: "All"),
            ACSegmentOption(value: .unused, label: "Unused \(AppsModel.unusedDays)+ days"),
            ACSegmentOption(value: .updates, label: count > 0 ? "Updates (\(count))" : "Updates"),
        ]
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 8) {
                tools
                filters
                if let error = model.error {
                    ACNoticeText(error, tone: .bad)
                }
                inventory
                AppsDisclosure(
                    "About these numbers",
                    isOpen: model.aboutOpen,
                    toggle: { model.aboutOpen.toggle() }
                ) {
                    ACText(aboutText, .caption, ink: .secondary)
                        .frame(maxWidth: 560, alignment: .leading)
                }
                StartupView(model: model.startup)
            }
            .padding(.horizontal, 18)
            .padding(.vertical, 14)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private var aboutText: String {
        let tail =
            ACPlatform.isWindows
            ? "Removing an app runs its own uninstaller; leftover folders you pick go to the Recycle Bin. Last used comes from what Windows recorded when you launched the app from Explorer or Start, so many apps show Unknown."
            : "Removing an app moves it to the Trash."
        return
            "Sizes are measured on disk and keep updating while the list refreshes. Unused means not opened for \(AppsModel.unusedDays)+ days; an unknown last use never counts as unused. Updates show what the last check found. Apps that update themselves open their own updater. \(tail)"
    }

    private var tools: some View {
        HStack(spacing: 8) {
            AppsSearchField("Search apps by name or bundle id", label: "Search apps", text: $model.query)
            ACButton(
                model.refreshing ? "Refreshing…" : "Refresh",
                enabled: !model.refreshing
            ) { model.startRefresh() }
            ACButton(
                model.checking ? "Checking…" : "Check updates",
                enabled: !model.checking
            ) { model.startUpdateCheck(force: true) }
        }
    }

    /// The filter choices, with the count line under them (the page lets it sit beside them only
    /// when the window is wide enough for both, which the default is not).
    private var filters: some View {
        VStack(alignment: .leading, spacing: 6) {
            ACSegmented(options: filterOptions, selection: $model.filter)
            if !model.apps.isEmpty {
                ACText(summary, .caption, ink: .secondary)
            }
        }
    }

    /// The list's card (`.ck-apps-card`): a grey panel with a hairline outline, the rows inside it
    /// scrolling on their own.
    private var inventory: some View {
        HubLayered {
            RoundedRectangle(cornerRadius: theme.radii.card).fill(palette.fill2.color)
            RoundedRectangle(cornerRadius: theme.radii.card).stroke(palette.hairline.color, lineWidth: 1)
            ScrollView {
                VStack(spacing: 0) {
                    inventoryRows
                }
                .padding(.horizontal, 12)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .frame(height: Self.cardHeight)
    }

    @ViewBuilder
    private var inventoryRows: some View {
        let shown = model.shown
        if model.apps.isEmpty && (model.refreshing || !model.loaded) {
            AppsStateBlock(title: "Reading your applications…", message: "Checking sizes and last use")
        } else if model.apps.isEmpty {
            AppsStateBlock(title: "No applications found", message: "Refresh to read them again.")
        } else if shown.isEmpty {
            VStack(alignment: .leading, spacing: 8) {
                AppsStateBlock(
                    title: "No matching apps",
                    message: "Clear the search or filter, or refresh the list.",
                    padding: false
                )
                if model.isFiltered {
                    AppsButton("Clear filters") { model.clearFilters() }
                }
            }
            .padding(.horizontal, 4)
            .padding(.vertical, 18)
            .frame(maxWidth: .infinity, alignment: .leading)
        } else {
            let largest = max(1, shown.map(\.sizeBytes).max() ?? 1)
            ForEach(shown) { app in
                VStack(spacing: 0) {
                    AppRowView(model: model, app: app, largest: largest)
                    if app.id != shown.last?.id { Divider() }
                }
            }
        }
    }
}

/// An empty or waiting state inside the list's card (`.ck-apps-state`): a heading and a line, at the
/// leading edge.
struct AppsStateBlock: View {
    let title: String
    let message: String
    var padding = true

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(verbatim: title)
                .font(Font.system(size: 14, weight: .semibold))
                .foregroundStyle(ink)
            if !message.isEmpty {
                ACText(message, .body, ink: .secondary)
            }
        }
        .padding(.horizontal, padding ? 4 : 0)
        .padding(.vertical, padding ? 18 : 0)
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var ink: Color { theme.palette(scheme).ink.color }
}

/// One app in the list: icon, name and badges, the update control and the size beside a bar.
struct AppRowView: View {
    let model: AppsModel
    let app: AppsAPI.Entry
    let largest: Int64
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    private var sub: String {
        let version = app.version.flatMap { $0.isEmpty ? nil : "\($0) · " } ?? ""
        return "\(version)Last used: \(AppsModel.lastUsedText(app.lastUsed))"
    }

    /// The row button's name, as the Tauri page's reads it: the name, its badges, then the second
    /// line ("Xcode 27.0 · Last used: 9 days ago").
    private var rowLabel: String {
        var parts = [app.name]
        if app.running { parts.append("Running") }
        if app.protectedReason != nil { parts.append("Protected") }
        parts.append(sub)
        return parts.joined(separator: " ")
    }

    var body: some View {
        HStack(spacing: 10) {
            Button {
                model.openDetail(app)
            } label: {
                HubLayered {
                    HubHitArea()
                    HStack(spacing: 10) {
                        AppIconView(model.icons[app.path])
                        VStack(alignment: .leading, spacing: 2) {
                            HStack(spacing: 8) {
                                Text(verbatim: app.name)
                                    .font(Font.system(size: 13, weight: .semibold))
                                    .foregroundStyle(palette.ink.color)
                                    .lineLimit(1)
                                if app.running { AppsBadge("Running", tone: .ok) }
                                if app.protectedReason != nil { AppsBadge("Protected") }
                            }
                            ACText(sub, .caption, ink: .secondary, lines: 1)
                        }
                        Spacer(minLength: 0)
                    }
                    .padding(.vertical, 4)
                    .frame(maxWidth: .infinity, minHeight: 48, alignment: .leading)
                }
            }
            .buttonStyle(.plain)
            .accessibilityLabel(rowLabel)
            // The name column is the one that truncates when a row is tight; the update control and the
            // size keep their width (Fit.swift).
            .layoutPriority(Fit.priority(windows: 3))
            UpdateControlView(
                update: model.updates[app.path],
                job: model.jobs[app.path]
            ) { model.startUpdate(path: app.path) }
            VStack(alignment: .trailing, spacing: 4) {
                ACText(AppsFormat.bytes(app.sizeBytes), .body, ink: .secondary, lines: 1)
                HubProgressBar(value: Double(app.sizeBytes) / Double(largest), height: 4)
            }
            .frame(width: 84)
            // The size keeps its width on Windows' wider font.
            .layoutPriority(2)
        }
        .padding(.vertical, 2)
    }
}

/// The update badge and action for one app. It sits beside the open button, never inside it.
struct UpdateControlView: View {
    let update: AppsAPI.Update?
    let job: AppsUpdateJob?
    let onUpdate: @MainActor () -> Void

    /// The longest "Update available 1.2.3" that fits on the badge's line beside the Update button
    /// in the page's 260 pt control; a longer one wraps and the button drops under it.
    private static let inlineLimit = 32

    var body: some View {
        if let job {
            switch job.state {
            case .running:
                ACText(job.message, .caption, ink: .secondary, lines: 2)
                    .frame(maxWidth: 260, alignment: .trailing)
                    .layoutPriority(2)
            case .failed:
                ACText("Update failed: \(job.message)", .caption, ink: .bad, lines: 2)
                    .frame(maxWidth: 260, alignment: .trailing)
                    .layoutPriority(2)
            case .done:
                ACText(job.message, .caption, ink: .ok, lines: 2)
                    .frame(maxWidth: 260, alignment: .trailing)
                    .layoutPriority(2)
            case .unconfirmed:
                ACText(job.message, .caption, ink: .warn, lines: 2)
                    .frame(maxWidth: 260, alignment: .trailing)
                    .layoutPriority(2)
            }
        } else if let update, update.state == "available" {
            let badge = "Update available" + (update.latestVersion.map { " \($0)" } ?? "")
            if badge.count <= Self.inlineLimit {
                HStack(spacing: 6) {
                    // The badge is the longer of the two, so it is served first and never wraps (Fit.swift).
                    AppsBadge(badge, tone: .accent)
                        .layoutPriority(Fit.priority(windows: 2, other: 1))
                    AppsButton("Update") { onUpdate() }
                }
                .frame(maxWidth: Fit.length(windows: 300, other: 260), alignment: .trailing)
                // The badge and button keep their width; the app's name column gives way first.
                .layoutPriority(2)
            } else {
                VStack(alignment: .trailing, spacing: 6) {
                    AppsBadge(badge, tone: .accent)
                    AppsButton("Update") { onUpdate() }
                }
                .frame(maxWidth: 260, alignment: .trailing)
                .layoutPriority(2)
            }
        } else if let update, update.state == "app_store" {
            AppsButton("Open in App Store") { onUpdate() }
        }
    }
}
#endif

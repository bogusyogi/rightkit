#if os(macOS)
import RightKitHub
import SwiftUI

/// The Cleanup section: findings you can select and move to the Trash, and the history with Restore.
struct CleanupRootView: View {
    @State private var model: CleanupModel
    private let loadsOnAppear: Bool

    /// - Parameter loadsOnAppear: False when the model was loaded already (the off-screen renderer).
    init(model: CleanupModel, loadsOnAppear: Bool) {
        _model = State(initialValue: model)
        self.loadsOnAppear = loadsOnAppear
    }

    var body: some View {
        CleanupView(model: model)
            .task {
                if loadsOnAppear { await model.load() }
            }
    }
}

struct CleanupView: View {
    @Bindable var model: CleanupModel
    @Environment(\.hubTheme) private var theme

    private var paneOptions: [ACSegmentOption<CleanupPane>] {
        [
            ACSegmentOption(value: .findings, label: "Findings"),
            ACSegmentOption(value: .history, label: "History · \(model.history.count)"),
        ]
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            head
            ACSegmented(options: paneOptions, selection: $model.pane)
            notices
            ScrollView {
                VStack(alignment: .leading, spacing: 10) {
                    if model.pane == .history {
                        CleanupHistoryView(model: model)
                    } else {
                        findings
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            if model.pane == .findings && !model.eligible.isEmpty {
                selectionBar
            }
        }
        .padding(.horizontal, 18)
        .padding(.top, 14)
        .padding(.bottom, 14)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .hubConfirm(
            model.confirmTitle,
            message: model.confirmMessage,
            confirmLabel: "Move to \(CleanupModel.trash)",
            isPresented: $model.confirmOpen
        ) {
            Task { await model.move(model.pending ?? []) }
        }
    }

    // MARK: Header and notices

    private var head: some View {
        HStack(spacing: 10) {
            VStack(alignment: .leading, spacing: 2) {
                ACText(model.heading, .title)
                if let sub = model.subheading {
                    ACText(sub, .caption, ink: .secondary)
                }
            }
            Spacer(minLength: 0)
            ACButton(
                model.refreshing ? "Refreshing…" : "Rescan",
                enabled: !(model.busy || model.refreshing)
            ) { model.startRefresh() }
        }
    }

    @ViewBuilder
    private var notices: some View {
        if let note = model.note {
            let ink: ACInk = note.tone == .ok ? .primary : .warn
            ACNotice(tone: note.tone == .ok ? .neutral : .warn) {
                ACText(note.text, .caption, ink: ink)
                if let detail = note.detail, !detail.isEmpty {
                    ACText(detail, .caption, ink: ink)
                }
                if model.pane == .findings && !model.history.isEmpty {
                    ACButton("History and restore") { model.pane = .history }
                }
            }
        }
        if let message = model.error {
            ACNoticeText(message, tone: .bad)
        }
        if let message = model.historyError {
            ACNoticeText(message, tone: .bad)
        }
    }

    // MARK: Findings

    @ViewBuilder
    private var findings: some View {
        if let report = model.report, report.findings.contains(where: { $0.partial }) {
            ACNoticeText("Some sizes are partial; ≥ marks a lower bound.", tone: .warn)
        }
        ChromeSnapshotsView(info: model.report?.chromeSnapshots)
        if model.report != nil && model.eligible.isEmpty {
            ACText("Nothing to clean up right now. Rescan after apps close.", .caption, ink: .secondary)
                .padding(.vertical, 16)
        }
        if !model.eligible.isEmpty {
            HStack(spacing: 8) {
                ACButton("Free this up", enabled: !model.busy && !model.freeable.isEmpty) { model.freeUp() }
                ACText("Selects the safe items and Chrome's signing copies for review. Nothing moves until you confirm.", .caption, ink: .secondary)
            }
            CleanupCard {
                FindingGroupView(model: model, title: "Safe to regenerate", findings: model.safe)
                FindingGroupView(model: model, title: "Review", findings: model.review)
            }
        }
        if !model.held.isEmpty {
            heldList
        }
        aboutBlock
    }

    private var heldList: some View {
        let held = model.held
        return VStack(alignment: .leading, spacing: 0) {
            Divider()
            ACDisclosure(
                isOpen: model.heldOpen,
                toggle: { model.heldOpen.toggle() },
                header: {
                    HStack(spacing: 4) {
                        ACText("\(held.count) not offered", .label)
                        ACText("· informational, running or unverified", .caption, ink: .secondary)
                    }
                },
                content: {
                    VStack(alignment: .leading, spacing: 6) {
                        ForEach(held) { finding in
                            HStack(alignment: .top, spacing: 8) {
                                VStack(alignment: .leading, spacing: 1) {
                                    ACText(finding.name, .label)
                                    ACText(finding.reason, .caption, ink: .secondary)
                                }
                                .layoutPriority(Fit.priority(windows: 3))
                                Spacer(minLength: 0)
                                ACText(StorageText.bytes(finding.bytes), .caption, ink: .secondary, lines: 1)
                            }
                        }
                    }
                }
            )
        }
    }

    private var aboutBlock: some View {
        let trash = CleanupModel.trash
        return VStack(alignment: .leading, spacing: 0) {
            Divider()
            ACDisclosure(
                isOpen: model.aboutOpen,
                toggle: { model.aboutOpen.toggle() },
                header: { ACText("About these numbers", .body) },
                content: {
                    ACText(
                        "Nothing is deleted. Items go to the \(trash), where you can put them back or empty it yourself. Sizes are what the scan measured; ≥ marks a lower bound when part of a folder could not be read. Items whose owner could not be verified, or whose app is running, are never offered.",
                        .caption,
                        ink: .secondary
                    )
                }
            )
        }
    }

    // MARK: Selection bar

    private var selectionBar: some View {
        let chosen = model.chosen
        let trash = CleanupModel.trash
        return VStack(spacing: 8) {
            Divider()
            HStack(spacing: 10) {
                HStack(spacing: 4) {
                    ACText("\(chosen.count) selected · \(CleanupModel.total(chosen))", .label)
                    ACText("to move to the \(trash)", .caption, ink: .secondary)
                }
                Spacer(minLength: 0)
                ACButton(
                    "Move \(chosen.count) to \(trash)",
                    tone: .primary,
                    enabled: !(model.busy || chosen.isEmpty)
                ) { model.askToMove() }
            }
        }
    }
}

/// One group of findings (safe, or review) with a select-all checkbox and the group's size.
struct FindingGroupView: View {
    let model: CleanupModel
    let title: String
    let findings: [CleanupAPI.Finding]

    private var checkState: ACCheckState {
        let count = findings.filter { model.selected.contains($0.id) }.count
        if count == 0 { return .off }
        return count == findings.count ? .on : .mixed
    }

    var body: some View {
        if !findings.isEmpty {
            VStack(alignment: .leading, spacing: 0) {
                HStack(spacing: 4) {
                    ACCheckbox(
                        state: checkState,
                        label: "Select all \(title.lowercased()) items",
                        enabled: !model.busy
                    ) { model.toggleGroup(findings, on: checkState != .on) }
                    ACText(title, .label)
                    Spacer(minLength: 0)
                    ACText(CleanupModel.total(findings), .caption, ink: .secondary, lines: 1)
                }
                .padding(.vertical, 2)
                ForEach(findings) { finding in
                    Divider()
                    FindingRowView(model: model, finding: finding)
                }
            }
            .padding(.bottom, 8)
        }
    }
}

/// One finding: a checkbox, its name with a Safe or Review badge and reason, and its size.
struct FindingRowView: View {
    let model: CleanupModel
    let finding: CleanupAPI.Finding

    private var size: String {
        let prefix = finding.partial ? "≥ " : (finding.isUpperBound ? "up to " : "")
        return "\(prefix)\(StorageText.bytes(finding.bytes))"
    }

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            ACCheckbox(
                state: model.selected.contains(finding.id) ? .on : .off,
                label: "Select \(finding.name)",
                enabled: !model.busy
            ) { model.toggle(finding.id) }
            ACDisclosure(
                isOpen: model.expanded.contains(finding.id),
                toggle: { model.toggleExpanded(finding.id) },
                header: {
                    VStack(alignment: .leading, spacing: 1) {
                        HStack(spacing: 6) {
                            ACText(finding.name, .label)
                            ACBadge(
                                finding.risk == "safe" ? "Safe" : "Review",
                                tone: finding.risk == "safe" ? .ok : .warn
                            )
                        }
                        ACText(finding.reason, .caption, ink: .secondary)
                    }
                },
                content: {
                    VStack(alignment: .leading, spacing: 2) {
                        ACText(ACFormat.shortPath(finding.path), .caption, ink: .secondary)
                        ACText(finding.category, .caption, ink: .secondary)
                        if finding.isUpperBound {
                            ACText(CleanupModel.estimate, .caption, ink: .secondary)
                        }
                    }
                }
            )
            ACText(size, .caption, ink: .secondary, lines: 1)
                .padding(.top, 9)
                // The size keeps its width on Windows' wider font; the name and reason give way first.
                .layoutPriority(2)
        }
    }
}

/// Chrome's leftover signing snapshots as a finding row: how many, the change since the last check,
/// and what moving them would really give back.
struct ChromeSnapshotsView: View {
    let info: CleanupAPI.ChromeSnapshots?

    private func detail(_ info: CleanupAPI.ChromeSnapshots) -> String {
        var text: String
        if !info.reclaimableKnown {
            text = "Apparent \(StorageText.bytes(info.apparentBytes)), much less to free; shares space with Chrome"
        } else if info.reclaimableBytes == 0 {
            text = "Nothing to free; shares space with Chrome"
        } else {
            text =
                "About \(StorageText.bytes(info.reclaimableBytes)) to gain (apparent \(StorageText.bytes(info.apparentBytes)), shares space with Chrome)"
        }
        if info.running { text += " · Close Chrome to clear \(info.count == 1 ? "it" : "them")" }
        return text
    }

    private func change(_ info: CleanupAPI.ChromeSnapshots) -> String? {
        guard let at = info.sinceAt, let before = info.sinceCount else { return nil }
        let delta = info.count - before
        if delta == 0 { return nil }
        let since = ACFormat.monthDay(at)
        return delta < 0 ? "\(-delta) fewer since \(since)" : "+\(delta) since \(since)"
    }

    var body: some View {
        if let info, info.count > 0 {
            CleanupCard {
                HStack(alignment: .top, spacing: 8) {
                    VStack(alignment: .leading, spacing: 2) {
                        HStack(spacing: 4) {
                            ACText("Chrome \(info.count == 1 ? "snapshot" : "snapshots")", .label)
                            ACText(
                                "· \(info.count)" + (change(info).map { " (\($0))" } ?? ""),
                                .caption,
                                ink: .secondary
                            )
                        }
                        ACText(detail(info), .caption, ink: .secondary)
                        ACText(
                            "Temporary copies of the Chrome app left behind when Chrome is force-quit",
                            .small,
                            ink: .tertiary
                        )
                    }
                    // Served before the size beside it, which is short: an equal share would wrap this
                    // column at half the card on Windows (Fit.swift).
                    .layoutPriority(Fit.priority(windows: 3))
                    Spacer(minLength: 0)
                    if info.reclaimableKnown {
                        ACText(StorageText.bytes(info.reclaimableBytes), .label, lines: 1)
                    }
                }
            }
        }
    }
}

/// The history: each run with its items, a warning when it needs one, and Restore.
struct CleanupHistoryView: View {
    let model: CleanupModel

    var body: some View {
        CleanupCard {
            ACText("Moves and restore", .title)
                .padding(.bottom, 6)
            if !model.historyWarnings.isEmpty {
                ACNotice(tone: .warn) {
                    ForEach(model.historyWarnings, id: \.self) { warning in
                        ACText(warning, .caption, ink: .warn)
                    }
                }
                .padding(.bottom, 6)
            }
            if model.history.isEmpty {
                ACText("Nothing moved yet. Confirmed moves appear here.", .caption, ink: .secondary)
            }
            ForEach(model.history) { activity in
                Divider()
                CleanupRunView(model: model, activity: activity)
            }
        }
    }
}

/// One run in the history.
struct CleanupRunView: View {
    let model: CleanupModel
    let activity: CleanupAPI.Activity

    private var pathsKey: String { "paths:\(activity.id)" }

    var body: some View {
        let trash = CleanupModel.trash
        let left = activity.items.filter { !$0.isRestored }
        // An item whose state is unknown (interrupted) is never offered for Undo.
        let canRestore = left.contains { $0.trashPath != nil && $0.interrupted == nil }
        let issue = model.restoreIssue[activity.id]
        let warning = CleanupModel.runWarning(activity)
        HStack(alignment: .top, spacing: 10) {
            VStack(alignment: .leading, spacing: 3) {
                ACText(ACFormat.dateTime(activity.at), .label)
                ACText(summary(left: left.count, trash: trash), .caption, ink: .secondary)
                if let warning {
                    ACNoticeText(warning, tone: .warn)
                }
                if let spaces = activity.freeSpace, !spaces.isEmpty {
                    ACText(
                        spaces.map(CleanupModel.freeSpaceLine).joined(separator: " ") + " " + CleanupModel.freeSpaceCaveat,
                        .caption,
                        ink: .secondary
                    )
                }
                if !activity.items.isEmpty {
                    ACDisclosure(
                        isOpen: model.expanded.contains(pathsKey),
                        toggle: { model.toggleExpanded(pathsKey) },
                        header: { ACText("Paths", .caption, ink: .secondary) },
                        content: {
                            VStack(alignment: .leading, spacing: 2) {
                                ForEach(activity.items, id: \.path) { item in
                                    ACText(
                                        "\(ACFormat.shortPath(item.path)) · \(state(of: item, trash: trash))",
                                        .caption,
                                        ink: .secondary
                                    )
                                }
                            }
                        }
                    )
                }
                ForEach(activity.items.filter { !$0.isRestored && $0.interrupted != nil }, id: \.path) { item in
                    ACText("\(ACFormat.shortPath(item.path)): \(item.interrupted ?? "")", .caption, ink: .secondary)
                }
                if let issue {
                    ACText("Some items stayed in the \(trash).", .caption, ink: .bad)
                    ACText(issue, .caption, ink: .bad)
                }
            }
            Spacer(minLength: 0)
            if !activity.items.isEmpty {
                ACButton(
                    issue != nil ? "Retry restore" : (left.isEmpty ? "Restored" : "Restore"),
                    enabled: !(model.busy || !canRestore)
                ) { Task { await model.restore(activity.id) } }
            }
        }
        .padding(.vertical, 8)
    }

    private func summary(left: Int, trash: String) -> String {
        var text =
            activity.items.isEmpty
            ? "Nothing was moved"
            : "\(ACFormat.items(activity.items.count)) moved to the \(trash) · \(StorageText.bytes(activity.bytes))"
        if left < activity.items.count { text += " · \(activity.items.count - left) restored" }
        return text
    }

    private func state(of item: CleanupAPI.ActivityItem, trash: String) -> String {
        if item.isRestored { return "Restored" }
        if item.interrupted != nil { return "State unknown" }
        if item.trashPath != nil { return "In the \(trash)" }
        return "Not restorable"
    }
}

/// The page's panel: a tinted fill with a hairline border, 12 points in at the sides and 8 above and
/// below, content top to bottom (`.ck-card`).
struct CleanupCard<Content: View>: View {
    private let content: Content
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    init(@ViewBuilder content: () -> Content) {
        self.content = content()
    }

    var body: some View {
        let palette = theme.palette(scheme)
        HubLayered {
            RoundedRectangle(cornerRadius: 10).fill(palette.fill2.color)
            RoundedRectangle(cornerRadius: 10).stroke(palette.hairline.color, lineWidth: 1)
            VStack(alignment: .leading, spacing: 0) {
                content
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}
#endif

import SwiftUI

/// **An update, offered in the notch** — and, taken, installed there.
///
/// A toast like the rest (see `NoticeCard`): Pulse's icon, "Pulse 1.19.0 is
/// available" and a line of what is in it, with Update, Later and a close.
/// Update turns it into the installing card — a spinner, and the download's
/// progress, then extracting and installing — until Codenotch relaunches as the
/// new version. See `Updater`.
///
/// Live, `NoticeStage` draws it out of the notch; this view is the card
/// settled and fully out, for the CI picture renderer.
struct UpdateCard: View {
    let prompt: UpdatePrompt
    let direction: NotchEdge.TooltipDirection
    var tailOffset: CGFloat = 0
    var onChoice: ((UpdateChoice) -> Void)?

    var body: some View {
        let notice = NoticePrompt(update: prompt)
        NoticeSettled(width: NoticeMetrics.width, height: NoticeCard.size(for: notice).height,
                      direction: direction, tailOffset: tailOffset) {
            NoticeCard(prompt: notice) { choice in
                if case .update(let answer) = choice { onChoice?(answer) }
            }
        }
    }
}

extension NoticePrompt {
    /// An update as a toast: offered, installing, or installed and waiting for a restart.
    init(update prompt: UpdatePrompt) {
        let installing = prompt.phase != .available && prompt.phase != .restart

        let title: String
        if prompt.phase == .restart {
            title = L10n.t("Pulse was updated")
        } else {
            title = installing ? L10n.t("Installing Pulse \(prompt.version)")
                               : L10n.t("Pulse \(prompt.version) is available")
        }

        let status: String
        switch prompt.phase {
        case .available:
            status = prompt.notes.isEmpty ? L10n.t("A new version of Pulse is ready to install.") : prompt.notes
        case .downloading(let share):
            if let share {
                status = L10n.t("Downloading… \(Int((share * 100).rounded()))%")
            } else {
                status = L10n.t("Downloading…")
            }
        case .extracting:
            status = L10n.t("Preparing…")
        case .installing:
            status = L10n.t("Installing…")
        case .restart:
            status = L10n.t("Restart to use the new version.")
        }

        // How far along the bar is: the download, then the unpacking, then full.
        let bar: Double?
        switch prompt.phase {
        case .available, .restart:     bar = nil
        case .downloading(let share):  bar = (share ?? 0) * 0.85
        case .extracting(let share):   bar = 0.85 + min(max(share, 0), 1) * 0.1
        case .installing:              bar = 1
        }

        var actions: [Action] = []
        switch prompt.phase {
        case .restart:
            actions = [Action(choice: .update(.restart), label: L10n.t("Restart"), symbol: "arrow.clockwise")]
        case .available:
            actions = [Action(choice: .update(.install), label: L10n.t("Update"), symbol: "arrow.down.circle"),
                       Action(choice: .update(.later), label: L10n.t("Later"), symbol: "clock")]
        default:
            break
        }

        self.init(glyph: installing ? .working : .app, title: title, detail: status,
                  actions: actions, progress: bar,
                  closeChoice: installing ? nil : NoticeChoice.update(.close), closeHelp: L10n.t("Close"))
    }
}

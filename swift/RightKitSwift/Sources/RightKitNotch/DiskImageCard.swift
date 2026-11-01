import SwiftUI

/// **A disk image in the notch**, drawn as a toast: its app's icon (or a mark
/// for how it went), a title, a line or two, and up to two pills with a close.
/// After an automatic install it reads "Installed <App>" with Undo and goes by
/// itself; a hover keeps it. Nearby sharing's cards ride the same prompt.
///
/// Live, `NoticeStage` draws the toast out of the notch; this view is the card
/// settled and fully out, for the CI picture renderer.
struct DiskImageCard: View {
    let prompt: DiskImagePrompt
    let direction: NotchEdge.TooltipDirection
    var tailOffset: CGFloat = 0
    var onChoice: ((DiskImageChoice) -> Void)?

    var body: some View {
        let notice = NoticePrompt(disk: prompt)
        NoticeSettled(width: NoticeMetrics.width, height: NoticeCard.size(for: notice).height,
                      direction: direction, tailOffset: tailOffset) {
            NoticeCard(prompt: notice) { choice in
                if case .disk(let answer) = choice { onChoice?(answer) }
            }
        }
    }
}

extension NoticePrompt {
    /// A disk image's card, or a nearby sharing one, as a toast.
    init(disk prompt: DiskImagePrompt) {
        self = Self.make(disk: prompt)
    }

    private static func make(disk prompt: DiskImagePrompt) -> NoticePrompt {
        if let send = prompt.send {
            if send.transfer == nil, send.rows.isEmpty, !send.shelf.isEmpty {
                return shelved(prompt, send)
            }
            return sending(prompt, send)
        }
        let glyph: Glyph
        switch prompt.style {
        case .working: glyph = .working
        case .problem: glyph = .problem
        case .stopped: glyph = .symbol("stop.fill")
        case .done where prompt.primary == nil: glyph = .done
        default: glyph = .file(prompt.iconPath)
        }
        var actions: [Action] = []
        if let primary = prompt.primary {
            actions.append(Action(choice: .disk(primary.choice), label: primary.label,
                                  symbol: symbol(for: primary.choice),
                                  choiceWhenChecked: prompt.checkbox.map { NoticeChoice.disk($0.primaryWhenChecked) }))
        }
        if let secondary = prompt.secondary {
            actions.append(Action(choice: .disk(secondary.choice), label: secondary.label,
                                  symbol: symbol(for: secondary.choice)))
        }
        // A working card offers Cancel (and the close) only while cancelling is possible.
        let closable = !(prompt.style == .working && prompt.primary == nil)
        return NoticePrompt(glyph: glyph, title: prompt.title, detail: prompt.detail, warning: prompt.warning,
                            isProblem: prompt.style == .problem, actions: actions,
                            checkbox: prompt.checkbox.map { Checkbox(label: $0.label, startsTicked: $0.startsTicked) },
                            life: prompt.life,
                            closeChoice: closable ? NoticeChoice.disk(.dismiss) : nil,
                            closeHelp: prompt.style == .working ? L10n.t("Cancel") : L10n.t("Close"))
    }

    /// A saved-files card: its own icon, Show, and the received-files shelf under the detail.
    private static func shelved(_ prompt: DiskImagePrompt, _ send: SendCardContent) -> NoticePrompt {
        var actions: [Action] = []
        if let primary = prompt.primary {
            actions.append(Action(choice: .disk(primary.choice), label: primary.label,
                                  symbol: symbol(for: primary.choice)))
        }
        return NoticePrompt(glyph: .file(prompt.iconPath), title: prompt.title, detail: prompt.detail,
                            actions: actions, shelf: send.shelf, life: prompt.life,
                            closeChoice: .disk(.dismiss), closeHelp: L10n.t("Close"))
    }

    /// Nearby sharing's device list, or its transfer in flight.
    private static func sending(_ prompt: DiskImagePrompt, _ send: SendCardContent) -> NoticePrompt {
        if let transfer = send.transfer {
            let glyph: Glyph
            switch transfer.state {
            case .waiting: glyph = .working
            case .active:  glyph = .symbol(transfer.symbol)
            case .done:    glyph = .done
            case .problem: glyph = .problem
            }
            var actions: [Action] = []
            if transfer.canCancel {
                actions.append(Action(choice: .disk(.cancel), label: L10n.t("Cancel"),
                                      symbol: symbol(for: .cancel)))
            }
            return NoticePrompt(glyph: glyph, title: prompt.title, detail: prompt.detail,
                                isProblem: prompt.style == .problem, actions: actions,
                                progress: transfer.state == .active ? min(max(transfer.fraction ?? 0, 0), 1) : nil,
                                life: prompt.life,
                                closeChoice: .disk(.dismiss), closeHelp: L10n.t("Cancel"))
        }
        return NoticePrompt(glyph: .provider(.send), title: prompt.title, detail: prompt.detail,
                            actions: [],
                            rows: Rows(items: send.rows, emptyText: L10n.t("Waiting for a device to appear…")),
                            life: prompt.life,
                            closeChoice: .disk(.dismiss), closeHelp: L10n.t("Cancel"))
    }

    /// An SF Symbol for what a pill does.
    fileprivate static func symbol(for choice: DiskImageChoice) -> String {
        switch choice {
        case .install, .installAlways: return "arrow.down.app"
        case .replace, .quitAndUpdate: return "arrow.triangle.2.circlepath"
        case .undo:          return "arrow.uturn.backward"
        case .openInstalled: return "arrow.up.forward.app"
        case .openInstaller: return "shippingbox"
        case .showImage:     return "folder"
        case .cancel:        return "stop.circle"
        case .dismiss:       return "clock"
        case .sendTo:        return "paperplane"
        case .refresh:       return "arrow.clockwise"
        case .copyText:      return "doc.on.doc"
        case .openLink:      return "safari"
        case .noteAction:    return "gearshape"
        case .approvePairing: return "checkmark.circle"
        case .rejectPairing:  return "xmark.circle"
        case .allowChatPlugin: return "checkmark.circle"
        case .declineChatPlugin: return "xmark.circle"
        case .bringChatsIn: return "checkmark.circle"
        case .keepChatsApart: return "xmark.circle"
        case .browserApprove: return "checkmark.circle"
        case .browserDeny: return "xmark.circle"
        case .browserOpen: return "arrow.up.forward.app"
        }
    }
}

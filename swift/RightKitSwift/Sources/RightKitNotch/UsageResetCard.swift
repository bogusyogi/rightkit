import SwiftUI

/// The notice a provider's limit reset, a limit reached, or any other note from
/// the app, drawn beside the notch as a toast (see `NoticeCard`).
///
/// Live, `NoticeStage` draws it out of the notch; this view is the card
/// settled and fully out, for the CI picture renderer.
struct UsageResetCard: View {
    let event: UsageResetEvent
    let direction: NotchEdge.TooltipDirection
    var tailOffset: CGFloat = 0
    /// How long its owner leaves it up: the ring round the ×.
    var life: NoticePrompt.Life? = nil
    var onDismiss: (() -> Void)? = nil

    var body: some View {
        let notice = NoticePrompt(event: event, dismissable: onDismiss != nil, life: life)
        NoticeSettled(width: NoticeMetrics.width, height: NoticeCard.size(for: notice).height,
                      direction: direction, tailOffset: tailOffset) {
            NoticeCard(prompt: notice) { choice in
                // The settled picture opens nothing; its button, like the ×, only puts the card away.
                switch choice {
                case .dismissNotice, .openHub: onDismiss?()
                default: break
                }
            }
        }
    }
}

extension NoticePrompt {
    /// A usage alert or a note as a toast. `life` is how long its owner leaves
    /// it up, for the ring round the ×.
    init(event: UsageResetEvent, dismissable: Bool = true, life: Life? = nil) {
        let title: String
        if let notice = event.noticeTitle {
            title = notice
        } else {
            switch event.kind {
            case .reset:
                title = L10n.t("\(event.providerName) Reset")
            case .sessionLimitReached:
                title = L10n.t("\(event.providerName) Limit Reached")
            case .weeklyLimitReached:
                title = L10n.t("\(event.providerName) Weekly Limit")
            }
        }

        let subtitle: String
        if let notice = event.noticeSubtitle {
            subtitle = notice
        } else {
            switch event.kind {
            case .reset:
                subtitle = L10n.t("\(event.windowLabel) limit refreshed")
            case .sessionLimitReached, .weeklyLimitReached:
                subtitle = L10n.t("\(event.windowLabel) limit is spent")
            }
        }

        let status: String
        if let notice = event.noticeStatus {
            status = notice
        } else {
            switch event.kind {
            case .reset:
                status = L10n.t("Quota is available (0% used)")
            case .sessionLimitReached:
                status = L10n.t("Session limit reached (100% used)")
            case .weeklyLimitReached:
                status = L10n.t("Weekly limit reached (100% used)")
            }
        }

        let timePrefix: String
        switch event.kind {
        case .reset:
            timePrefix = L10n.t("Next reset")
        case .sessionLimitReached, .weeklyLimitReached:
            timePrefix = L10n.t("Resets at")
        }

        // The words the card always had, in the toast's anatomy: the detail is
        // what it said under the title, then when it resets; a limit's status
        // line is the warning, in red.
        let limit = event.kind != .reset
        var lines = [subtitle]
        if !limit && !status.isEmpty { lines.append(status) }
        if let resetsAt = event.resetsAt {
            lines.append("\(timePrefix) \(resetsAt.formatted(date: .omitted, time: .shortened))")
        }

        self.init(glyph: .provider(event.glyph), title: title,
                  detail: lines.filter { !$0.isEmpty }.joined(separator: "\n"),
                  warning: limit && !status.isEmpty ? status : nil,
                  isAlert: limit,
                  actions: event.noticeHubLink.map {
                      [Action(choice: .openHub($0.section), label: $0.label, symbol: $0.symbol)]
                  } ?? [],
                  life: life,
                  closeChoice: dismissable ? NoticeChoice.dismissNotice : nil,
                  closeHelp: L10n.t("Close"))
    }
}

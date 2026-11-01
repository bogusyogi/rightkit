#if os(macOS)
import RightKitHub
import SwiftUI

/// Notifications: where alerts go, which events announce, and buttons that try the alerts.
struct SettingsNotificationsPage: View {
    let model: SettingsPageModel
    @Environment(\.hubTheme) private var theme

    private var rows: SettingsRows { SettingsRows(model: model) }

    var body: some View {
        SettingsCardStack(model: model, cards: cards)
    }

    private var cards: [SettingsCardSpec] {
        var cards = SettingsCardList()
        cards.add("Where", whereRows())
        cards.add("Agent sessions", sessionRows())
        cards.add("Limits", limitRows())
        cards.add("Drives", driveRows())
        cards.add("Remote approvals", approvalRows())
        cards.add("Mute alerts", muteRows())
        cards.add("Try them", tryRows())
        return cards.cards
    }

    private func whereRows() -> [SettingsRowSpec] {
        var list = SettingsRowList()
        list.add(rows.choice("notificationChannel"))
        list.add(rows.choice("peekDuration", segmented: false))
        return list.rows
    }

    private func sessionRows() -> [SettingsRowSpec] {
        var list = SettingsRowList()
        list.add(rows.toggle("watchesAgentSessions"))
        list.add(rows.toggle("announceSessionEnd"))
        list.add(rows.toggle("sessionEndSound"))
        return list.rows
    }

    private func limitRows() -> [SettingsRowSpec] {
        var list = SettingsRowList()
        list.add(rows.toggle("announceUsageReset"))
        list.add(rows.toggle("usageResetSound"))
        list.add(rows.toggle("announceSessionLimitReached"))
        list.add(rows.toggle("announceWeeklyLimitReached"))
        list.add(rows.toggle("limitReachedSound"))
        return list.rows
    }

    private func driveRows() -> [SettingsRowSpec] {
        var list = SettingsRowList()
        list.add(rows.toggle("announceDriveAlerts"))
        list.add(rows.toggle("driveAlertSound"))
        return list.rows
    }

    private func approvalRows() -> [SettingsRowSpec] {
        var list = SettingsRowList()
        list.add(rows.toggle("remoteApprovalAlerts"))
        return list.rows
    }

    private func muteRows() -> [SettingsRowSpec] {
        var list = SettingsRowList()
        list.add(rows.toggle("mute_claude_alerts"))
        list.add(rows.toggle("mute_codex_alerts"))
        return list.rows
    }

    private func tryRows() -> [SettingsRowSpec] {
        [
            SettingsRowSpec(
                id: "try", text: "Try the alerts \(SettingsTryAlertsRow.note)",
                view: SettingsTryAlertsRow(model: model))
        ]
    }
}

/// "Try the alerts": a test notification and a preview of each limit alert.
struct SettingsTryAlertsRow: View {
    let model: SettingsPageModel

    static let note = "Send a test notification, or preview the reset, session limit and weekly limit alerts."

    private func send(_ command: String) {
        let current = model
        Task { await current.send(command) }
    }

    var body: some View {
        SettingsStackedFrame(title: "Try the alerts", note: Self.note) {
            HStack(spacing: 8) {
                Spacer(minLength: 0)
                SettingsButton("Send a test") { send("sendTestNotification") }
                SettingsButton("Preview reset") { send("previewResetAlert") }
                SettingsButton("Preview session limit") { send("previewSessionLimitAlert") }
                SettingsButton("Preview weekly limit") { send("previewWeeklyLimitAlert") }
            }
        }
    }
}
#endif

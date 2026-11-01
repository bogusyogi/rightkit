#if os(macOS)
import RightKitHub
import SwiftUI

/// Permissions: which permission each switched-on feature needs, with a button that opens the place
/// to grant it. On the Windows notch the rows come from the system (`windows_permissions`).
struct SettingsPermissionsPage: View {
    let model: SettingsPageModel
    @Environment(\.hubTheme) private var theme

    var body: some View {
        if model.isWindows {
            windowsBody
        } else {
            macBody
        }
    }

    // MARK: Mac

    @ViewBuilder
    private var macBody: some View {
        SettingsCardStack(model: model, cards: macCards)
    }

    private var macCards: [SettingsCardSpec] {
        var cards = SettingsCardList()
        if let permissions = model.state?.permissions {
            cards.add("Permissions", macRows(permissions))
        } else {
            var list = SettingsRowList()
            list.add(
                id: "waiting", pinned: true,
                SettingsFootLine(text: "Waiting for permission status from Pulse notch…").padding(.top, 10))
            cards.add("Permissions", list.rows)
        }
        return cards.cards
    }

    private func macRows(_ permissions: [SettingsPermission]) -> [SettingsRowSpec] {
        var list = SettingsRowList()
        let missing = permissions.filter { $0.required == true && $0.status != "granted" }.count
        let lead = missing > 0
            ? "\(missing) required permission\(missing == 1 ? "" : "s") still need\(missing == 1 ? "s" : "") approval."
            : "Every required permission is granted."
        list.add(id: "lead", pinned: true, SettingsLead(text: lead, tone: missing > 0 ? .warn : .ok))
        let hub = model.permissions.fdaStatus
        let errors = model.state?.permissionErrors ?? [:]
        for permission in permissions {
            list.add(
                id: permission.id, text: "\(permission.title) \(permission.why)",
                SettingsPermissionRow(model: model, permission: permission, hub: hub))
            if let needs = permission.needs {
                list.foot(id: "\(permission.id).needs", SettingsFootLine(text: SettingsPermissionText.needs(needs)))
            }
            if let message = errors[permission.id], !message.isEmpty {
                list.foot(id: "\(permission.id).error", alert: true, SettingsErrorLine(text: message))
            }
        }
        list.add(
            id: "stale",
            text: "Clean up old apps Finds permissions still granted to apps that are no longer installed",
            SettingsStaleGrantsView(model: model))
        return list.rows
    }

    // MARK: Windows

    private var windowsBody: some View {
        var cards = SettingsCardList()
        cards.add("Permissions", windowsRows(model.permissions, model.state?.permissions))
        return SettingsCardStack(model: model, cards: cards.cards)
    }

    private func windowsRows(_ perms: SettingsPermissionsModel, _ notchRows: [SettingsPermission]?) -> [SettingsRowSpec] {
        var list = SettingsRowList()
        if let error = perms.windowsError {
            list.foot(id: "error", alert: true, SettingsErrorLine(text: error))
        }
        if let rows = perms.windowsRows {
            let missing = rows.filter { $0.required == true && $0.status != "granted" }.count
            let lead = missing > 0
                ? "\(missing) required permission\(missing == 1 ? "" : "s") still need\(missing == 1 ? "s" : "") approval."
                : "Every required permission is granted."
            list.add(id: "lead", pinned: true, SettingsLead(text: lead, tone: missing > 0 ? .warn : .ok))
            for permission in rows {
                list.add(
                    id: permission.id, text: "\(permission.title) \(permission.why)",
                    SettingsWindowsPermissionRow(model: model, permission: permission))
                if let needs = notchRows?.first(where: { $0.id == permission.id })?.needs {
                    list.foot(id: "\(permission.id).needs", SettingsFootLine(text: SettingsPermissionText.needs(needs)))
                }
            }
        }
        if let message = perms.openError {
            list.foot(id: "openError", alert: true, SettingsErrorLine(text: message))
        }
        list.add(
            id: "firewall", text: "Windows Firewall Lets Pulse find and talk to nearby devices for sharing files",
            SettingsLocalNetworkRow(model: model))
        if let needs = notchRows?.first(where: { $0.id == "firewall" })?.needs {
            list.foot(id: "firewall.needs", SettingsFootLine(text: SettingsPermissionText.needs(needs)))
        }
        return list.rows
    }
}

enum SettingsPermissionText {
    /// Under a Permissions row: what switched-on features need it.
    static func needs(_ needs: [String]) -> String {
        needs.isEmpty ? "Not needed by anything you have on" : "Needed for: \(needs.joined(separator: " \u{00b7} "))"
    }

    static func tone(_ status: String) -> HubTone {
        switch status {
        case "granted": return .ok
        case "needsApproval": return .warn
        default: return .neutral
        }
    }
}

/// One Mac permission: its title and why, its status, and the button that asks or opens System Settings.
/// Full Disk Access is confirmed against the hub's own probe (`fda_status`): granted to the notch but not
/// yet to the hub reads "Restart Pulse to apply".
struct SettingsPermissionRow: View {
    let model: SettingsPageModel
    let permission: SettingsPermission
    let hub: String?

    private var isFDA: Bool { permission.id == "fullDiskAccess" }
    private var awaitingHub: Bool { isFDA && permission.status == "granted" && hub == nil }
    private var restart: Bool { isFDA && permission.status == "granted" && hub != nil && hub != "granted" }

    private var status: String {
        if awaitingHub { return "unknown" }
        if restart { return "needsApproval" }
        return permission.status
    }

    private var statusText: String {
        if awaitingHub { return "Checking…" }
        if restart { return "Restart Pulse to apply" }
        switch status {
        case "granted": return "Granted"
        case "needsApproval": return "Needs approval"
        case "off": return "Off"
        default: return "Unknown"
        }
    }

    private var buttonLabel: String {
        let opensSettings = isFDA || status == "needsApproval"
        if permission.id == "finderMenu" { return "Turn On" }
        if permission.id == "automation" { return "Allow" }
        return opensSettings ? "Open Settings" : "Allow"
    }

    var body: some View {
        SettingsRowFrame(title: permission.title, note: permission.why) {
            HStack(spacing: 10) {
                SettingsBadge(text: statusText, tone: SettingsPermissionText.tone(status))
                SettingsButton(buttonLabel, enabled: status != "granted") {
                    let current = model
                    let id = permission.id
                    let fda = isFDA
                    Task {
                        if fda {
                            await current.permissions.requestFDA()
                        } else {
                            await current.send("permissionRequest", ["id": .string(id)])
                        }
                    }
                }
            }
        }
    }
}

/// A Windows permission row (Windows reads these from the system).
struct SettingsWindowsPermissionRow: View {
    let model: SettingsPageModel
    let permission: SettingsPermission

    private var statusText: String {
        switch permission.status {
        case "granted": return permission.id == "startup" ? "On" : "Allowed"
        case "needsApproval": return "Blocked"
        case "off": return "Off"
        default: return "Unknown"
        }
    }

    var body: some View {
        SettingsRowFrame(title: permission.title, note: permission.why) {
            HStack(spacing: 10) {
                SettingsBadge(text: statusText, tone: SettingsPermissionText.tone(permission.status))
                SettingsButton("Open Settings") {
                    let current = model
                    let id = permission.id
                    Task { await current.permissions.openWindowsPermission(id) }
                }
            }
        }
    }
}

/// The Windows Firewall row for nearby sharing (macOS Local Network is one of the notch's own rows).
/// Neither system can be asked for its state, so it is read from whether multicast sends are refused.
struct SettingsLocalNetworkRow: View {
    let model: SettingsPageModel

    var body: some View {
        let nearby = model.bool("nearbyEnabled")
        let network = model.share.share?.localNetwork
        let blocked = nearby && network == "blocked"
        let granted = nearby && network == "granted"
        let status = !nearby ? "off" : blocked ? "needsApproval" : granted ? "granted" : "unknown"
        let text = !nearby ? "Off" : blocked ? "Needs approval" : granted ? "Granted" : "Checking…"
        SettingsRowFrame(
            title: model.isWindows ? "Windows Firewall" : "Local network",
            note: model.isWindows
                ? "Lets Pulse find and talk to nearby devices for sharing files. Windows Firewall must allow Pulse on private networks."
                : "Lets Pulse find and talk to nearby devices for sharing files. macOS asks the first time a device connects."
        ) {
            HStack(spacing: 10) {
                SettingsBadge(text: text, tone: SettingsPermissionText.tone(status))
                SettingsButton(model.isWindows ? "Open Firewall" : "Open Settings", enabled: !granted) {
                    let current = model
                    Task { await current.share.openLocalNetworkSettings() }
                }
            }
        }
    }
}

/// Permission grants that belong to apps that no longer exist. Read from TCC.db (read-only) by the
/// engine; removed with `tccutil reset`, never by writing the database.
struct SettingsStaleGrantsView: View {
    let model: SettingsPageModel
    @State private var confirm = false
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        let perms = model.permissions
        let bundles = perms.bundles
        let paths = perms.paths
        let count = perms.picked.count
        let cleared = perms.results?.filter { $0.ok }.count ?? 0
        let failed = perms.results?.filter { !$0.ok } ?? []
        VStack(alignment: .leading, spacing: 0) {
            SettingsRowFrame(
                title: "Clean up old apps",
                note: "Finds permissions still granted to apps that are no longer installed. Needs Full Disk Access to look."
            ) {
                SettingsButton(perms.busy ? "Working…" : "Find old permissions", enabled: !perms.busy) {
                    let current = perms
                    Task { await current.scan() }
                }
            }
            if let error = perms.staleError {
                SettingsErrorLine(text: error)
            }
            if let apps = perms.stale, apps.isEmpty {
                SettingsFootLine(text: "No permissions from removed apps were found.")
            }
            ForEach(bundles) { app in
                VStack(alignment: .leading, spacing: 2) {
                    Text(verbatim: app.id)
                        .font(Font.system(size: 13, weight: .semibold))
                        .foregroundStyle(palette.ink.color)
                    ForEach(app.services, id: \.service) { service in
                        Toggle(service.label, isOn: pickedBinding(app.id, service.service))
                    }
                }
                .padding(.horizontal, theme.spacing.md + 2)
                .padding(.vertical, 4)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            if !bundles.isEmpty {
                HStack {
                    SettingsButton("Clear \(count) selected", enabled: !(perms.busy || count == 0)) { confirm = true }
                    Spacer()
                }
                .padding(.horizontal, theme.spacing.md + 2)
                .padding(.vertical, theme.spacing.sm)
            }
            if !paths.isEmpty {
                VStack(alignment: .leading, spacing: 2) {
                    Text(verbatim: "These were added by file path, which macOS will not let Pulse remove. Remove each with − in the matching System Settings list:")
                        .font(theme.type.caption.font)
                        .foregroundStyle(palette.ink2.color)
                    ForEach(paths) { app in
                        Text(verbatim: "\(app.id): \(app.services.map { $0.label }.joined(separator: ", "))")
                            .font(theme.type.caption.font)
                            .foregroundStyle(palette.ink2.color)
                            .padding(.leading, 12)
                    }
                }
                .padding(.horizontal, theme.spacing.md + 2)
                .padding(.bottom, theme.spacing.row)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            if perms.results != nil {
                VStack(alignment: .leading, spacing: 2) {
                    Text(verbatim: "\(cleared) cleared\(failed.isEmpty ? "" : ", \(failed.count) failed").")
                        .font(theme.type.caption.font)
                        .foregroundStyle(palette.ink.color)
                    ForEach(Array(failed.enumerated()), id: \.offset) { entry in
                        Text(verbatim: "\(entry.element.id) (\(entry.element.service)): \(entry.element.message)")
                            .font(theme.type.caption.font)
                            .foregroundStyle(palette.bad.color)
                    }
                }
                .padding(.horizontal, theme.spacing.md + 2)
                .padding(.bottom, theme.spacing.row)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .hubConfirm(
            "Clear \(count) old permission\(count == 1 ? "" : "s")?",
            message: "These apps are no longer installed. Pulse asks macOS to forget their grants with tccutil. Nothing else changes, and an app you reinstall will simply ask again.",
            confirmLabel: "Clear",
            isPresented: $confirm
        ) {
            let current = perms
            Task { await current.clearPicked() }
        }
    }

    private func pickedBinding(_ id: String, _ service: String) -> Binding<Bool> {
        let current = model.permissions
        let key = SettingsPermissionsModel.key(id, service)
        return Binding(
            get: { MainActor.assumeIsolated { current.picked.contains(key) } },
            set: { on in
                Task { @MainActor in current.toggle(key, on: on) }
            }
        )
    }
}
#endif

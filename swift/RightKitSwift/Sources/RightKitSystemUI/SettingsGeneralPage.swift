#if os(macOS)
import RightKitHub
import SwiftUI

/// General: startup, nearby sharing, shortcuts, updates, tools, the launcher, readings, conveniences and
/// window management, in the order of the Tauri page.
struct SettingsGeneralPage: View {
    let model: SettingsPageModel
    @Environment(\.hubTheme) private var theme
    @Environment(\.hubNavigate) private var navigate

    private var rows: SettingsRows { SettingsRows(model: model) }

    var body: some View {
        let state = model.state
        let windows = model.isWindows
        VStack(alignment: .leading, spacing: theme.spacing.lg - 2) {
            SettingsCardStack(model: model, cards: cards)
            if !model.searching, let version = state?.version {
                SettingsAboutLine(text: windows ? "Pulse notch \(version)" : "Pulse notch \(version) · built on Codenotch (MIT)")
            }
        }
    }

    /// The groups, in the order of the Tauri page.
    private var cards: [SettingsCardSpec] {
        let state = model.state
        let windows = model.isWindows
        var cards = SettingsCardList()
        cards.add("Startup", startupRows())
        cards.add("Diagnostics and Claude accounts", diagnosticsRows())
        cards.add("Agent", agentRows())
        cards.add("Devices", deviceLinkRows())
        cards.add("Nearby sharing", SettingsNearby.groupRows(model: model))
        if model.bool("nearbyEnabled") || model.settings["nearbyEnabled"] == nil,
           model.share.share?.running == true {
            cards.add("Devices", SettingsNearby.deviceRows(model: model))
        }
        cards.add("Keyboard shortcuts", shortcutRows(state))
        cards.add("Installers", installerRows())
        cards.add("Updates", updateRows(state))
        if !windows {
            cards.add("Uninstalling", uninstallRows(state))
        }
        if model.has("toolsOff") {
            cards.add("Tools", toolRows(windows: windows))
        }
        if model.has("wheelTools") {
            cards.add("Wheel", wheelRows(windows: windows))
        }
        if model.has("claudeChatMirror") {
            cards.add("Claude accounts", claudeRows())
        }
        if model.has("hubDriveHealth") {
            cards.add("Background", backgroundRows())
        }
        cards.add("Hub health", SettingsHub.healthRows(model: model))
        cards.add("Background processes", SettingsHub.processRows(model: model))
        cards.add("Remote access", SettingsHub.remoteRows(model: model))
        cards.add("Agent logins and cards", SettingsSecureBox.rows(model: model))
        cards.add("Launcher", SettingsLauncher.rows(model: model))
        cards.add("Keyboard", SettingsKeyboard.keyboardRows(model: model, navigate: navigate))
        if state?.conveniences == nil && model.has("convAutoQuit") {
            cards.add("Auto Quit", SettingsKeyboard.windowsAutoQuitRows(model: model))
        }
        cards.add("Readings", readingRows())
        if let conveniences = state?.conveniences {
            cards.add(
                "Conveniences",
                SettingsKeyboard.conveniencesRows(model: model, conveniences: conveniences, navigate: navigate))
        }
        if let management = state?.conveniences?.windowManagement {
            cards.add(
                "Window management",
                SettingsKeyboard.windowManagementRows(
                    model: model, management: management, accessibility: state?.conveniences?.accessibility == true))
        } else if state?.conveniences == nil, let management = state?.windowManagement {
            cards.add(
                "Window management",
                SettingsKeyboard.windowManagementRows(model: model, management: management, accessibility: true))
        }
        return cards.cards
    }

    private func oneRow(_ spec: SettingsRowSpec?) -> [SettingsRowSpec] {
        var list = SettingsRowList()
        list.add(spec)
        return list.rows
    }

    // MARK: Groups

    /// "Open Pulse at login" and "Show onboarding again". The Tauri button fires a window event the hub turns into
    /// the first-run window (`onboarding_open`, which the Swift client cannot reach); the notch's own
    /// `onboardingDone` setting is what it writes first, and the notch reopens setup while it is false.
    private func startupRows() -> [SettingsRowSpec] {
        var list = SettingsRowList()
        list.add(rows.toggle("launchAtLogin"))
        if model.has("onboardingDone") {
            list.add(
                id: "showOnboarding", text: "Show onboarding again Opens the first-run setup again Show onboarding",
                SettingsButtonRow(
                    title: "Show onboarding again",
                    note: "Opens the first-run setup again: the feature switches with their permissions, and the optional licence step. Nothing changes until you finish.",
                    button: "Show onboarding"
                ) {
                    let current = model
                    Task { await current.setValue("onboardingDone", .bool(false)) }
                })
        }
        return list.rows
    }

    private func shortcutRows(_ state: SettingsNotchState?) -> [SettingsRowSpec] {
        var list = SettingsRowList()
        list.add(rows.toggle("mac_shortcuts"))
        list.add(rows.toggle("altRemapSaveFindTabClose"))
        list.add(rows.toggle("screenshot_shortcuts"))
        if state?.conveniences == nil, let hotkey = state?.settingsHotkey {
            list.add(id: "settingsHotkey", text: "Open Settings with a shortcut", SettingsHotkeyRow(model: model, hotkey: hotkey))
        }
        list.add(rows.toggle("screenshot_to_desktop"))
        return list.rows
    }

    private func installerRows() -> [SettingsRowSpec] {
        var list = SettingsRowList()
        // The Tauri page shows this group on both platforms; the Mac's own row is under Conveniences.
        list.add(
            rows.toggle(
                "convDiskImageInstaller", title: "Watch Downloads for installers",
                note: "A new installer in Downloads brings up a card on the notch. Off, Pulse does not look at Downloads."))
        list.add(rows.toggle("installer_auto"))
        return list.rows
    }

    private func diagnosticsRows() -> [SettingsRowSpec] {
        var list = SettingsRowList()
        list.add(rows.toggle("hubDiagnosticsPage"))
        list.add(rows.toggle("claudeAccountScan"))
        return list.rows
    }

    private func agentRows() -> [SettingsRowSpec] {
        var list = SettingsRowList()
        list.add(rows.toggle("agentRequests"))
        list.add(rows.toggle("agentActions"))
        list.add(rows.toggle("membraneAutoEnroll"))
        return list.rows
    }

    private func deviceLinkRows() -> [SettingsRowSpec] {
        var list = SettingsRowList()
        list.add(rows.toggle("deviceLinkListen"))
        list.add(rows.toggle("deviceMeshLocal"))
        list.add(rows.toggle("chatOverDeviceLink"))
        list.add(rows.toggle("devicePhoneChats"))
        list.add(rows.choice("devicePhoneRead"))
        list.add(rows.choice("devicePhoneSend"))
        list.add(rows.toggle("devicePhoneTypeIntoChat"))
        list.add(rows.toggle("devicePhoneReceive"))
        list.add(rows.number("devicePhoneReceiveTtlMinutes", min: 1, max: 1440))
        list.add(rows.choice("chatClaudeHookDelivery"))
        list.add(rows.toggle("sendOverMesh"))
        list.add(rows.toggle("agentFileAutoAccept"))
        if model.has("sendDefaultDevice"), let setting = model.setting("sendDefaultDevice") {
            list.add(
                id: "sendDefaultDevice", text: SettingsText.searchText(setting, model: model),
                SettingsTextRow(setting: setting, model: model, placeholder: "Device id"))
        }
        if model.has("deviceLinkRelayUrl"), let setting = model.setting("deviceLinkRelayUrl") {
            list.add(
                id: "deviceLinkRelayUrl", text: SettingsText.searchText(setting, model: model),
                SettingsTextRow(setting: setting, model: model, placeholder: "https://"))
        }
        return list.rows
    }

    private func claudeRows() -> [SettingsRowSpec] {
        var list = SettingsRowList()
        list.add(rows.toggle("claudeChatMirror"))
        list.add(rows.toggle("claudeChatRemember"))
        list.add(rows.toggle("claudeAccountSwitcher"))
        return list.rows
    }

    private func backgroundRows() -> [SettingsRowSpec] {
        var list = SettingsRowList()
        list.add(rows.toggle("hubDriveHealth"))
        list.add(rows.toggle("hubMetricsHistory"))
        list.add(rows.toggle("hubFileIndex"))
        list.add(rows.toggle("hubFileIndexWholeDisk"))
        list.add(rows.list("hubFileIndexVolumes", placeholder: "/Volumes/Archive"))
        list.add(rows.list("hubFileIndexExcludes", placeholder: "target"))
        list.add(SettingsHub.fileIndexPauseRows(model: model))
        list.add(rows.toggle("chromeSnapshotCount"))
        list.add(rows.toggle("cleanupDeveloperItems"))
        list.add(rows.toggle("engineMemoryGuard"))
        list.add(rows.number("engineMemoryCapMb", min: 512, max: 65_536))
        list.add(rows.toggle("hubAutoScan"))
        list.add(rows.toggle("mediaCompression"))
        return list.rows
    }

    private func readingRows() -> [SettingsRowSpec] {
        var list = SettingsRowList()
        list.add(rows.toggle("gpuTemperature"))
        list.add(rows.toggle("asksProviderOnLook"))
        list.add(
            id: "refresh", text: "Refresh Refresh now",
            SettingsButtonRow(title: "Refresh", button: "Refresh now") {
                let current = model
                Task { await current.send("refresh") }
            })
        return list.rows
    }

    // MARK: Updates

    private func updateRows(_ state: SettingsNotchState?) -> [SettingsRowSpec] {
        var list = SettingsRowList()
        if let updates = state?.updates {
            let status = updates.status ?? ""
            let busy = ["checking", "downloading", "installing"].contains(status)
            let build = (updates.build ?? "").isEmpty ? "" : " (build \(updates.build ?? ""))"
            let versionTitle = "Version \(updates.current ?? "?")\(build)"
            list.add(
                id: "version", text: "\(versionTitle) \(SettingsUpdateText.line(updates) ?? "")",
                SettingsButtonRow(
                    title: versionTitle,
                    note: SettingsUpdateText.line(updates),
                    button: "Check now",
                    disabled: busy
                ) {
                    let current = model
                    Task { await current.send("checkUpdates") }
                })
            if let available = updates.available, !available.isEmpty, !["downloading", "installing"].contains(status) {
                list.add(
                    id: "install", text: "Install Pulse \(available) Downloads from GitHub, checks Apple's signature and notarization",
                    SettingsButtonRow(
                        title: "Install Pulse \(available)",
                        note: "Downloads from GitHub, checks Apple's signature and notarization, then replaces Pulse in Applications and reopens it.",
                        button: "Update"
                    ) {
                        let current = model
                        Task { await current.send("installUpdate") }
                    })
            }
        } else {
            list.foot(id: "waiting", SettingsFootLine(text: "Waiting for the notch to report its version.").padding(.top, 10))
        }
        list.add(rows.toggle("autoUpdateCheck"))
        return list.rows
    }

    // MARK: Uninstalling (Mac)

    private func uninstallRows(_ state: SettingsNotchState?) -> [SettingsRowSpec] {
        var list = SettingsRowList()
        let helper = state?.helper ?? ""
        let enabled = helper == "enabled" || helper == "requiresApproval"
        let current = model
        let helperNote = "Lets Pulse move root-owned apps and their files to the Trash with no administrator password. Approve once in System Settings."
        list.add(
            id: "helper", text: "Uninstall without password \(helperNote)",
            SettingsValueToggleRow(
                title: "Uninstall without password",
                note: helperNote,
                isOn: enabled
            ) { on in
                Task { await current.send(on ? "helperEnable" : "helperDisable") }
            })
        let text: String
        let tone: HubTone
        switch helper {
        case "enabled":
            text = "On. Root-owned items go to the Trash without a password."
            tone = .ok
        case "requiresApproval":
            text = "Waiting for your approval in System Settings."
            tone = .warn
        case "notFound":
            text = "This build of Pulse does not include the helper."
            tone = .neutral
        default:
            text = "Off. Root-owned items ask for an administrator password in Finder."
            tone = .neutral
        }
        list.foot(id: "helper.status", SettingsFootLine(text: text, tone: tone))
        list.foot(id: "helper.manage", SettingsManagePermissions(navigate: navigate))
        if let error = state?.helperError, !error.isEmpty {
            list.foot(id: "helper.error", alert: true, SettingsErrorLine(text: error))
        }
        return list.rows
    }

    // MARK: Tools and wheel

    private func toolRows(windows: Bool) -> [SettingsRowSpec] {
        var list = SettingsRowList()
        list.add(rows.toggle("toolWheelEnabled"))
        let off = model.list("toolsOff")
        let current = model
        for tool in SettingsCatalog.tools where tool.id != "clip" || model.has("launcherClipboard") {
            let note = SettingsNotes.note("tools.\(tool.id)", model.noteContext) ?? tool.note
            list.add(
                id: "tools.\(tool.id)", text: "\(tool.name) \(note)",
                SettingsValueToggleRow(
                    title: tool.name,
                    note: note,
                    isOn: !off.contains(tool.id),
                    switchName: "\(tool.name) tool"
                ) { on in
                    let next = on ? off.filter { $0 != tool.id } : off + [tool.id]
                    Task { await current.setValue("toolsOff", SettingsJSON.strings(next)) }
                })
        }
        return list.rows
    }

    private func wheelRows(windows: Bool) -> [SettingsRowSpec] {
        var list = SettingsRowList()
        let defaults = SettingsCatalog.wheel.filter { $0.on }.map { $0.id }
        let chosen = model.has("wheelTools") ? model.list("wheelTools") : defaults
        var offered: [String]?
        if case .array(let items)? = model.settings["wheelToolsOffered"] {
            offered = items.compactMap { $0.stringValue }
        }
        let available = SettingsCatalog.wheel.filter { offered?.contains($0.id) ?? true }
        let order = available.map { $0.id }
        let current = model
        for tool in available {
            let isWindowsSettings = windows && tool.id == "settings"
            let title = isWindowsSettings ? "Windows Settings" : tool.name
            let note = SettingsNotes.note("wheel.\(tool.id)", model.noteContext) ?? tool.note
            list.add(
                id: "wheel.\(tool.id)", text: "\(title) \(note)",
                SettingsValueToggleRow(
                    title: title, note: note, isOn: chosen.contains(tool.id), switchName: "\(title) on the wheel"
                ) { on in
                    let next = order.filter { $0 == tool.id ? on : chosen.contains($0) }
                    Task { await current.setValue("wheelTools", SettingsJSON.strings(next)) }
                })
        }
        return list.rows
    }
}

/// Wording of the Updates row (`updateLine` in Settings.tsx).
enum SettingsUpdateText {
    static func line(_ update: SettingsUpdateState) -> String? {
        guard let status = update.status, !status.isEmpty else { return nil }
        switch status {
        case "checking": return "Checking for updates…"
        case "upToDate": return update.message ?? "Pulse is up to date."
        case "available": return "Pulse \(update.available ?? "") is available."
        case "downloading":
            if let progress = update.progress { return "Downloading… \(Int((progress * 100).rounded()))%" }
            return "Downloading…"
        case "installing": return "Installing. Pulse reopens when it is done."
        case "failed": return "Update failed: \(update.message ?? "unknown reason")"
        default: return update.lastChecked != nil ? "Not checked since launch." : "Not checked yet."
        }
    }
}

/// The line at the foot of General: which notch this is.
struct SettingsAboutLine: View {
    let text: String
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        Text(verbatim: text)
            .font(theme.type.caption.font)
            .foregroundStyle(theme.palette(scheme).ink2.color)
            .padding(.horizontal, 2)
    }
}
#endif

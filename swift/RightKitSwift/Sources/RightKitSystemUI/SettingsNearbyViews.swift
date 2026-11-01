#if os(macOS)
import RightKitHub
import SwiftUI

/// General > Nearby sharing and Devices: port of hub/src/views/NearbySettings.tsx. The sharing service's
/// state is `share_state`, polled while the page is open; the switches for nearby sharing and the Send
/// ring are notch settings, the rest are engine commands (`chat_set_*`, `share_trust_*`).
enum SettingsNearby {
    /// "42s", "5min", "3h" or "2d" since `ms`.
    static func ageText(_ ms: Double, now: Date = Date()) -> String {
        let seconds = Swift.max(0, Int(now.timeIntervalSince1970 - ms / 1000))
        if seconds < 60 { return "\(seconds)s" }
        if seconds < 3600 { return "\(seconds / 60)min" }
        if seconds < 86400 { return "\(seconds / 3600)h" }
        return "\(seconds / 86400)d"
    }

    static let kindNames: [String: String] = [
        "mobile": "Phone", "desktop": "Computer", "web": "Browser", "headless": "Terminal", "server": "Server",
    ]

    static let notVerified = "Not verified: this app can't prove its identity. Allow asks first until it answers a check at its address."

    static func proof(_ device: SettingsTrustDevice) -> (text: String, note: String) {
        if device.proof == "certificate" {
            return ("verified (proved its key)", "This device proved it holds its key when it connected.")
        }
        if device.proof == "callback" {
            return (
                "answered at its address (weaker: files only, kept apart, never the clipboard)",
                "This device answered a check at its network address. Anyone on the network can relay that check while the real device is online, so it only skips the question; files are kept apart and nothing reaches the clipboard."
            )
        }
        return ("not verified", notVerified)
    }

    /// The Nearby sharing card.
    @MainActor
    static func groupRows(model: SettingsPageModel) -> [SettingsRowSpec] {
        var list = SettingsRowList()
        let windows = model.isWindows
        let share = model.share.share
        let enabled = model.settings["nearbyEnabled"]?.boolValue ?? true
        if let setting = model.setting("nearbyEnabled") {
            list.add(
                id: "nearbyEnabled", text: SettingsText.searchText(setting, model: model),
                SettingsToggleRow(setting: setting, model: model, switchName: "Nearby sharing"))
        }
        if enabled {
            if let setting = model.setting("nearbyAlias") {
                let placeholder = share?.alias ?? (windows ? "This PC" : "This Mac")
                list.add(
                    id: "nearbyAlias", text: SettingsText.searchText(setting, model: model),
                    SettingsTextRow(setting: setting, model: model, placeholder: placeholder))
            }
            if let setting = model.setting("nearbySaveFolder") {
                let folder = model.text("nearbySaveFolder")
                list.add(
                    id: "nearbySaveFolder", text: "\(setting.title) \(folder.isEmpty ? "Downloads" : folder)",
                    SettingsTextRow(
                        setting: setting, model: model, placeholder: "Downloads",
                        note: folder.isEmpty ? "Downloads" : folder,
                        choosePrompt: "Save received files to which folder?", resetLabel: "Use Downloads"))
            }
            chatRows(model: model, share: share, into: &list)
        }
        var status = ""
        var statusTone = HubTone.neutral
        if !enabled {
            status = "Off."
        } else if share == nil {
            status = "Starting…"
        } else if let error = share?.error, !error.isEmpty {
            status = error
            statusTone = .bad
        } else if share?.running == true {
            let count = share?.devices.count ?? 0
            status = "Visible as \"\(share?.alias ?? "")\". \(count == 0 ? "No devices nearby." : "\(count) nearby.")"
        }
        if !status.isEmpty {
            list.foot(id: "status", SettingsFootLine(text: status, tone: statusTone))
        }
        if enabled, let warnings = share?.warnings {
            for (index, warning) in warnings.enumerated() {
                list.foot(id: "warning.\(index)", SettingsFootLine(text: warning))
            }
        }
        return list.rows
    }

    /// Chat: chats talking to chats on linked computers.
    @MainActor
    private static func chatRows(model: SettingsPageModel, share: SettingsShareState?, into list: inout SettingsRowList) {
        let chat = share?.chat
        let enabled = chat?.enabled != false
        let shareModel = model.share
        func text(_ key: String) -> String {
            SettingsNotes.note(key, model.noteContext) ?? ""
        }
        list.add(
            id: "chat.enabled", text: "Chat \(text("chat.enabled"))",
            SettingsValueToggleRow(title: "Chat", note: text("chat.enabled"), isOn: enabled) { on in
                Task { await shareModel.setChatEnabled(on) }
            })
        if enabled {
            if let setting = model.setting("chatRingLight") {
                list.add(
                    id: "chatRingLight", text: SettingsText.searchText(setting, model: model),
                    SettingsToggleRow(setting: setting, model: model))
            }
            if let setting = model.setting("chatPluginAutoInstall") {
                list.add(
                    id: "chatPluginAutoInstall", text: SettingsText.searchText(setting, model: model),
                    SettingsToggleRow(setting: setting, model: model))
            }
            list.add(
                id: "chat.claudeHookDelivery",
                text: "Deliver chat messages to Claude through the Pulse plugin \(text("chat.claudeHookDelivery"))",
                SettingsValueToggleRow(
                    title: "Deliver chat messages to Claude through the Pulse plugin",
                    note: text("chat.claudeHookDelivery"), isOn: chat?.claudeHookDelivery == true
                ) { on in
                    Task { await shareModel.setClaudeHookDelivery(on) }
                })
            list.add(
                id: "chat.terminalChats",
                text: "List terminal chats active in the last 6 hours \(text("chat.terminalChats"))",
                SettingsValueToggleRow(
                    title: "List terminal chats active in the last 6 hours",
                    note: text("chat.terminalChats"), isOn: chat?.terminalChats != false
                ) { on in
                    Task { await shareModel.setTerminalChats(on) }
                })
            list.add(
                id: "chat.claudeProfiles",
                text: "Claude profiles in other home folders \(text("chat.claudeProfiles"))",
                SettingsValueToggleRow(
                    title: "Claude profiles in other home folders",
                    note: text("chat.claudeProfiles"), isOn: chat?.chatClaudeProfiles != false
                ) { on in
                    Task { await shareModel.setClaudeProfiles(on) }
                })
            list.add(
                id: "chat.muse",
                text: "Muse chats \(text("chat.muse"))",
                SettingsValueToggleRow(
                    title: "Muse chats",
                    note: text("chat.muse"), isOn: chat?.chatMuse != false
                ) { on in
                    Task { await shareModel.setMuseChats(on) }
                })
            list.add(
                id: "chat.stampUserWords",
                text: "Stamp my words into messages \(text("chat.stampUserWords"))",
                SettingsValueToggleRow(
                    title: "Stamp my words into messages",
                    note: text("chat.stampUserWords"), isOn: chat?.stampUserWords != false
                ) { on in
                    Task { await shareModel.setStampUserWords(on) }
                })
            list.add(
                id: "chat.codexWakeIdle",
                text: "Start a turn in an idle Codex chat when a message arrives \(text("chat.codexWakeIdle"))",
                SettingsValueToggleRow(
                    title: "Start a turn in an idle Codex chat when a message arrives",
                    note: text("chat.codexWakeIdle"), isOn: chat?.codexWakeIdle != false
                ) { on in
                    Task { await shareModel.setCodexWakeIdle(on) }
                })
            list.add(
                id: "chat.claudeWakeIdle",
                text: "Start a turn in an idle Claude chat when a message arrives \(text("chat.claudeWakeIdle"))",
                SettingsValueToggleRow(
                    title: "Start a turn in an idle Claude chat when a message arrives",
                    note: text("chat.claudeWakeIdle"), isOn: chat?.claudeWakeIdle != false
                ) { on in
                    Task { await shareModel.setClaudeWakeIdle(on) }
                })
            list.add(
                id: "chat.museWakeIdle",
                text: "Start a turn in an idle Muse chat when a message arrives \(text("chat.museWakeIdle"))",
                SettingsValueToggleRow(
                    title: "Start a turn in an idle Muse chat when a message arrives",
                    note: text("chat.museWakeIdle"), isOn: chat?.museWakeIdle != false
                ) { on in
                    Task { await shareModel.setMuseWakeIdle(on) }
                })
            list.add(
                id: "chat.receiptNotices",
                text: "Show receipts of messages you sent in chat hooks \(text("chat.receiptNotices"))",
                SettingsValueToggleRow(
                    title: "Show receipts of messages you sent in chat hooks",
                    note: text("chat.receiptNotices"), isOn: chat?.chatReceiptNotices != false
                ) { on in
                    Task { await shareModel.setReceiptNotices(on) }
                })
            list.add(
                id: "pulseSkill",
                text: "Pulse skill Teaches Claude and Codex chats how to message chats on other computers. Install the Pulse skill for Claude and Codex",
                SettingsButtonRow(
                    title: "Pulse skill",
                    note: "Teaches Claude and Codex chats how to message chats on other computers. Restart a chat to see it.",
                    button: "Install the Pulse skill for Claude and Codex",
                    disabled: shareModel.installing
                ) {
                    Task { await shareModel.installSkill() }
                })
        }
        if let report = shareModel.report {
            let names = ["claude": "Claude", "codex": "Codex"]
            for change in report.changes {
                list.foot(
                    id: "report.\(change.target)",
                    SettingsFootLine(text: "\(names[change.target] ?? change.target): \(change.action ?? ""). \(change.path ?? "")"))
            }
        }
        if let message = shareModel.message {
            list.foot(id: "message", alert: true, SettingsErrorLine(text: message))
        }
        var status = ""
        if !enabled {
            status = "Off."
        } else {
            let local = chat?.localChats ?? 0
            let links = (chat?.links ?? []).map { link -> String in
                if let failure = link.error, !failure.isEmpty {
                    let since = link.lastErrorMs.map { "offline \(ageText($0))" } ?? "offline"
                    return "\(link.device): \(since): \(failure)"
                }
                let ok = link.lastOkMs.map { ", ok \(ageText($0))" } ?? ""
                return "\(link.chats ?? 0) on \(link.device)\(ok)"
            }
            status = (["\(local) \(local == 1 ? "chat" : "chats") here."] + links.map { "\($0)." }).joined(separator: " ")
        }
        if let failure = chat?.lastError, !failure.isEmpty {
            list.foot(id: "chat.status", alert: true, SettingsErrorLine(text: failure))
        } else {
            list.foot(id: "chat.status", SettingsFootLine(text: status))
        }
        if enabled, let sent = chat?.activity?.sentMs {
            let outcome = chat?.activity?.lastOutcome ?? "delivered"
            list.foot(id: "chat.outcome", SettingsFootLine(text: "Last message: \(outcome) · \(ageText(sent)) ago"))
        }
        if enabled, let chats = chat?.chats, !chats.isEmpty {
            list.add(id: "chat.chats", text: "Chats Pulse can message", SettingsChatList(chats: chats))
        }
    }

    /// The Devices card: every device seen on the network or in a request, with Allow / Ask / Deny.
    @MainActor
    static func deviceRows(model: SettingsPageModel) -> [SettingsRowSpec] {
        guard let trust = model.share.share?.trust else { return [] }
        let shareModel = model.share
        var list = SettingsRowList()
        let devices = (trust.devices ?? []).sorted { left, right in
            let leftPresent = left.present == true
            let rightPresent = right.present == true
            if leftPresent != rightPresent { return leftPresent }
            return (left.lastSeenMs ?? 0) > (right.lastSeenMs ?? 0)
        }
        list.add(
            id: "newDevices", text: "New devices What a device gets the first time it is seen",
            SettingsNewDevicesRow(share: shareModel, state: trust.defaultState ?? "ask"))
        if devices.isEmpty {
            list.foot(id: "none", SettingsFootLine(text: "No devices seen yet."))
        }
        for device in devices {
            list.add(
                id: "device.\(device.fingerprint)", text: "\(device.label ?? "") \(device.alias ?? "") \(device.model ?? "")",
                SettingsDeviceRow(share: shareModel, device: device))
        }
        if let message = shareModel.message {
            list.foot(id: "devices.message", alert: true, SettingsErrorLine(text: message))
        }
        return list.rows
    }
}

/// What a device gets the first time it is seen: Ask or Deny.
struct SettingsNewDevicesRow: View {
    let share: SettingsShareModel
    let state: String

    private var binding: Binding<String> {
        let current = share
        let value = state
        return Binding(
            get: { value },
            set: { next in
                Task { await current.setTrustDefault(next) }
            }
        )
    }

    var body: some View {
        SettingsRowFrame(
            title: "New devices",
            note: "What a device gets the first time it is seen. Allow sends and receives with no question; Ask asks every time; Deny refuses."
        ) {
            SettingsSegmented(
                label: "New devices",
                options: [HubChoice(value: "ask", title: "Ask"), HubChoice(value: "deny", title: "Deny")],
                selection: binding)
        }
    }
}

/// One device with a trust record: its name (renamed in place), how it proved itself, and Allow / Ask / Deny.
struct SettingsDeviceRow: View {
    let share: SettingsShareModel
    let device: SettingsTrustDevice
    @State private var editing = false
    @State private var draft = ""
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    private var name: String {
        if let label = device.label, !label.isEmpty { return label }
        if let alias = device.alias, !alias.isEmpty { return alias }
        return "Unnamed device"
    }

    private func commit() {
        let next = draft.trimmingCharacters(in: .whitespaces)
        editing = false
        if next != (device.label ?? "") {
            let current = share
            let fingerprint = device.fingerprint
            Task { await current.setLabel(fingerprint, next) }
        }
    }

    private var trustBinding: Binding<String> {
        let current = share
        let fingerprint = device.fingerprint
        let value = device.state ?? "ask"
        return Binding(
            get: { value },
            set: { next in
                Task { await current.setTrust(fingerprint, next) }
            }
        )
    }

    private var subtitle: String {
        var parts: [String] = []
        if let kind = device.kind { parts.append(SettingsNearby.kindNames[kind] ?? "Device") }
        if let model = device.model, !model.isEmpty { parts.append(model) }
        parts.append(device.present == true ? "nearby now" : "last seen \(SettingsNearby.ageText(device.lastSeenMs ?? 0)) ago")
        let refusedCount = device.refused ?? 0
        if refusedCount > 0 { parts.append("refused \(refusedCount) \(refusedCount == 1 ? "time" : "times")") }
        return parts.joined(separator: " · ")
    }

    var body: some View {
        let proof = SettingsNearby.proof(device)
        HStack(alignment: .center, spacing: theme.spacing.lg) {
            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: theme.spacing.sm) {
                    if editing {
                        TextField(device.alias ?? "Name this device", text: $draft)
                            .textFieldStyle(.roundedBorder)
                            .frame(width: 200)
                            .accessibilityLabel("Name for \(name)")
                            .onSubmit { commit() }
                        SettingsButton("Save") { commit() }
                        SettingsButton("Cancel", kind: .ghost) { editing = false }
                    } else {
                        Button {
                            draft = device.label ?? ""
                            editing = true
                        } label: {
                            HStack(spacing: 6) {
                                Text(verbatim: name)
                                    .font(Font.system(size: 13, weight: .semibold))
                                    .foregroundStyle(palette.ink.color)
                                    // Served before the pencil, or an equal share cuts the name (Fit.swift).
                                    .layoutPriority(Fit.priority(windows: 1))
                                Image(systemName: "pencil")
                                    .foregroundStyle(palette.ink3.color)
                            }
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel("Rename \(name)")
                    }
                    SettingsBadge(text: proof.text, tone: device.proof == "certificate" ? .ok : .neutral)
                        .accessibilityHint(proof.note)
                }
                Text(verbatim: subtitle)
                    .font(theme.type.caption.font)
                    .foregroundStyle(palette.ink2.color)
                if device.verified != true && device.state == "allow" {
                    Text(verbatim: SettingsNearby.notVerified)
                        .font(theme.type.caption.font)
                        .foregroundStyle(palette.ink2.color)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            HStack(spacing: theme.spacing.md) {
                SettingsSegmented(
                    label: "Trust for \(name)",
                    options: [HubChoice(value: "allow", title: "Allow"), HubChoice(value: "ask", title: "Ask"), HubChoice(value: "deny", title: "Deny")],
                    selection: trustBinding)
                if device.present != true {
                    Button {
                        let current = share
                        let fingerprint = device.fingerprint
                        Task { await current.forget(fingerprint) }
                    } label: {
                        Image(systemName: "xmark")
                            .foregroundStyle(palette.ink3.color)
                            .frame(width: 22, height: 22)
                    }
                    .buttonStyle(.plain)
                    .layoutPriority(2)
                    .accessibilityLabel("Forget \(name)")
                    .accessibilityHint("Removes this device from the list; it returns when it is next seen.")
                }
            }
        }
        .padding(.horizontal, theme.spacing.md + 2)
        .padding(.vertical, theme.spacing.sm)
        .frame(minHeight: 48)
        .task(id: device.label ?? "") {
            if !editing { draft = device.label ?? "" }
        }
    }
}

/// Every chat Pulse can message, grouped by computer: Claude chats live; Codex threads active when
/// written to in the last ten minutes.
struct SettingsChatList: View {
    let chats: [SettingsChatRow]
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        let devices = orderedDevices
        VStack(alignment: .leading, spacing: theme.spacing.row) {
            ForEach(devices, id: \.self) { device in
                section(device)
            }
        }
        .padding(.horizontal, theme.spacing.md + 2)
        .padding(.bottom, theme.spacing.md)
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Chats Pulse can message")
    }

    private var orderedDevices: [String] {
        var seen: [String] = []
        for chat in chats {
            let device = chat.device ?? ""
            if !seen.contains(device) { seen.append(device) }
        }
        let local = seen.filter { device in chats.contains { ($0.device ?? "") == device && $0.local == true } }
        return local + seen.filter { !local.contains($0) }
    }

    private func section(_ device: String) -> some View {
        let here = chats.filter { ($0.device ?? "") == device }
        let claude = here.filter { $0.kind == "claude" }
        let codex = here.filter { $0.kind == "codex" }
        return VStack(alignment: .leading, spacing: 2) {
            HStack(alignment: .firstTextBaseline, spacing: theme.spacing.sm) {
                Text(verbatim: device)
                    .font(Font.system(size: 12, weight: .semibold))
                    .foregroundStyle(palette.ink.color)
                Text(verbatim: "\(claude.count) Claude · \(codex.count) Codex")
                    .font(theme.type.caption.font)
                    .foregroundStyle(palette.ink2.color)
            }
            ForEach(Array(claude.enumerated()), id: \.offset) { entry in
                row(entry.element, kind: "claude")
            }
            ForEach(Array(codex.prefix(8).enumerated()), id: \.offset) { entry in
                row(entry.element, kind: "codex")
            }
            if codex.count > 8 {
                Text(verbatim: "+\(codex.count - 8) more Codex threads (`pulse chat peers` lists all)")
                    .font(theme.type.caption.font)
                    .foregroundStyle(palette.ink2.color)
                    .padding(.leading, 60)
            }
        }
    }

    private func row(_ chat: SettingsChatRow, kind: String) -> some View {
        let folder = (chat.cwd ?? "").split(whereSeparator: { $0 == "/" || $0 == "\\" }).last.map(String.init) ?? ""
        let status = (chat.status ?? "") + ((chat.liveness ?? "").isEmpty ? "" : " · \(chat.liveness ?? "")")
        let inbox = chat.local == true && ((chat.unread ?? 0) > 0 || (chat.evicted ?? 0) > 0)
        return VStack(alignment: .leading, spacing: 1) {
            HStack(spacing: theme.spacing.sm) {
                SettingsBadge(text: kind == "claude" ? "Claude" : "Codex", tone: kind == "claude" ? .accent : .neutral)
                    .frame(width: 60, alignment: .leading)
                Text(verbatim: chat.name)
                    .font(theme.type.caption.font)
                    .foregroundStyle(palette.ink.color)
                    .lineLimit(1)
                    .frame(maxWidth: .infinity, alignment: .leading)
                Text(verbatim: folder)
                    .font(theme.type.caption.font)
                    .foregroundStyle(palette.ink2.color)
                    .lineLimit(1)
                    .frame(width: 150, alignment: .leading)
                HStack(spacing: 6) {
                    if let updated = chat.updatedMs {
                        Text(verbatim: "\(SettingsNearby.ageText(updated)) ago")
                    }
                    Text(verbatim: status)
                }
                .font(Font.system(size: 11))
                .foregroundStyle(palette.ink3.color)
                .lineLimit(1)
                .frame(width: 140, alignment: .trailing)
            }
            if inbox {
                Text(verbatim: "Inbox: \(chat.unread ?? 0) unread\((chat.evicted ?? 0) > 0 ? ", \(chat.evicted ?? 0) evicted" : "")")
                    .font(Font.system(size: 11))
                    .foregroundStyle(palette.ink2.color)
                    .padding(.leading, 68)
            }
        }
    }
}
#endif

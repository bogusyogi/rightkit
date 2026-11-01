#if os(macOS)
import RightKitHub
import SwiftUI
import UniformTypeIdentifiers
#if os(macOS)
import AppKit
#endif

/// General: Remote access, Hub health, the file-index pause ladder (under Background's "Index file
/// names" row) and the hub's process row. Each row follows the `has(key)` rule of `SettingsRows`: a row
/// shows only when the notch publishes its key, so a notch that has not published one shows no switch
/// for it.
@MainActor
enum SettingsHub {
    /// Remote access (`remoteTools` and the rest of the remote settings, docs/settings.md).
    static func remoteRows(model: SettingsPageModel) -> [SettingsRowSpec] {
        let rows = SettingsRows(model: model)
        var list = SettingsRowList()
        list.add(rows.toggle("remoteTools"))
        list.add(folderRow(model: model))
        list.add(blockedAppsRow(model: model))
        list.add(rows.list("remoteChatRead", placeholder: "chat ids from pulse chat peers"))
        list.add(rows.toggle("phoneChatReadAll"))
        list.add(rows.number("remoteReadMaxBytes", min: 1, max: 2_097_152))
        list.add(rows.number("remoteFindMaxResults", min: 1, max: 1_000))
        list.add(rows.number("remoteChatReadMaxTurns", min: 1, max: 500))
        list.add(rows.number("remoteChatReadMaxBytes", min: 1, max: 2_097_152))
        list.add(rows.number("remoteImageMaxBytes", min: 1, max: 26_214_400))
        list.add(rows.toggle("remoteGateway"))
        return list.rows
    }

    /// Approved folders: a path list with Remove, and "Choose…" through the platform's folder chooser.
    static func folderRow(model: SettingsPageModel) -> SettingsRowSpec? {
        guard model.has("remoteRoots"), let setting = model.setting("remoteRoots") else { return nil }
        return SettingsRowSpec(
            id: "remoteRoots", text: SettingsText.searchText(setting, model: model),
            view: SettingsFolderListRow(setting: setting, model: model))
    }

    /// Blocked apps: the apps no remote tool may open, see or act on. A bundle-id list with Remove,
    /// an "Add app…" chooser and a drop target for .app bundles.
    static func blockedAppsRow(model: SettingsPageModel) -> SettingsRowSpec? {
        guard model.has("remoteBlockedApps"), let setting = model.setting("remoteBlockedApps") else { return nil }
        return SettingsRowSpec(
            id: "remoteBlockedApps", text: SettingsText.searchText(setting, model: model),
            view: SettingsBlockedAppsRow(setting: setting, model: model))
    }

    /// Hub health: the resource watchdog and its budgets, the macOS report watch and the hub log.
    static func healthRows(model: SettingsPageModel) -> [SettingsRowSpec] {
        let rows = SettingsRows(model: model)
        var list = SettingsRowList()
        list.add(rows.toggle("hubWatchdog"))
        list.add(rows.number("hubWatchdogCpuPercent", min: 1, max: 1_000))
        list.add(rows.number("hubWatchdogMinutes", min: 1, max: 60))
        list.add(rows.number("hubWatchdogFootprintMb", min: 64, max: 65_536))
        list.add(rows.number("hubWatchdogThreads", min: 10, max: 10_000))
        list.add(rows.number("hubWatchdogWriteMbPerHour", min: 1, max: 100_000))
        list.add(rows.toggle("hubDiagReports"))
        list.add(rows.toggle("hubLog"))
        return list.rows
    }

    /// The file index's pause ladder, listed under Background's "Index file names" row.
    static func fileIndexPauseRows(model: SettingsPageModel) -> [SettingsRowSpec] {
        let rows = SettingsRows(model: model)
        var list = SettingsRowList()
        list.add(rows.toggle("hubFileIndexAutoPause"))
        list.add(rows.number("hubFileIndexMaxTrips", min: 1, max: 20))
        list.add(rows.number("hubFileIndexPauseMinutes", min: 1, max: 1_440))
        list.add(rows.number("hubFileIndexMaxCpuPercent", min: 1, max: 1_000))
        list.add(rows.number("hubFileIndexMaxFootprintMb", min: 64, max: 65_536))
        list.add(rows.number("hubFileIndexMaxWriteMbPerHour", min: 1, max: 100_000))
        return list.rows
    }

    /// Background processes: the hub's supervision state (`hubProcess`, top level of notch-state.json),
    /// read-only, with Retry while it is latched.
    static func processRows(model: SettingsPageModel) -> [SettingsRowSpec] {
        guard let status = model.state?.hubProcess, !status.isEmpty else { return [] }
        var list = SettingsRowList()
        list.add(
            id: "hubProcess", text: "Hub process \(SettingsHubProcessRow.line(status))",
            SettingsHubProcessRow(status: status, model: model))
        return list.rows
    }
}

/// A list of folder paths kept by the notch as an array: each path with Remove, and an Add line with
/// "Choose…", which opens the platform's folder chooser and adds the folder at once. The typed path is
/// the fallback where the platform has no chooser.
struct SettingsFolderListRow: View {
    let setting: HubSetting
    let model: SettingsPageModel

    var body: some View {
        if let published = model.settings[setting.key] {
            let items = published.stringList
            let asText = published.stringValue != nil
            let current = model
            let key = setting.key
            SettingsStackedFrame(title: setting.title, note: SettingsText.note(setting, model: model)) {
                VStack(alignment: .leading, spacing: 6) {
                    ForEach(Array(items.enumerated()), id: \.offset) { entry in
                        SettingsPathLine(text: entry.element, removeLabel: "Remove \(entry.element)") {
                            var next = items
                            next.remove(at: entry.offset)
                            Task { await current.setValue(key, SettingsListRow.encode(next, asText: asText)) }
                        }
                    }
                    SettingsAddLine(placeholder: "Folder path", choosePrompt: "Choose a folder to approve") { value in
                        guard !items.contains(value) else { return }
                        let next = items + [value]
                        Task { await current.setValue(key, SettingsListRow.encode(next, asText: asText)) }
                    }
                }
            }
        }
    }
}

/// The blocked-apps list (`remoteBlockedApps`, bundle ids): each entry with Remove, "Add app…"
/// (a chooser for a .app) and a drop target that takes .app bundles and reads their bundle id.
/// Adrian, 2026-10-17: a block list, the opposite of an allow list; nothing is blocked by default.
struct SettingsBlockedAppsRow: View {
    let setting: HubSetting
    let model: SettingsPageModel
    @State private var over = false
    @State private var problem: String?

    /// The bundle id of a .app, or nil with a sentence in `problem`.
    @MainActor
    private static func bundleId(_ url: URL) -> String? {
        guard url.pathExtension.lowercased() == "app" else { return nil }
        return Bundle(url: url)?.bundleIdentifier
    }

    @MainActor
    private func add(_ ids: [String], items: [String], asText: Bool) {
        var next = items
        for id in ids where !next.contains(where: { $0.caseInsensitiveCompare(id) == .orderedSame }) {
            next.append(id)
        }
        guard next.count != items.count else { return }
        let current = model
        let key = setting.key
        Task { await current.setValue(key, SettingsListRow.encode(next, asText: asText)) }
    }

    var body: some View {
        if let published = model.settings[setting.key] {
            let items = published.stringList
            let asText = published.stringValue != nil
            let current = model
            let key = setting.key
            SettingsStackedFrame(title: setting.title, note: SettingsText.note(setting, model: model)) {
                VStack(alignment: .leading, spacing: 6) {
                    ForEach(Array(items.enumerated()), id: \.offset) { entry in
                        SettingsPathLine(text: entry.element, removeLabel: "Remove \(entry.element)") {
                            var next = items
                            next.remove(at: entry.offset)
                            Task { await current.setValue(key, SettingsListRow.encode(next, asText: asText)) }
                        }
                    }
                    if items.isEmpty {
                        Text(verbatim: "Drop a .app here")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    #if canImport(AppKit)
                    SettingsButton("Add app…", label: "Choose an app to block") {
                        let panel = NSOpenPanel()
                        panel.allowedContentTypes = [UTType.applicationBundle]
                        panel.directoryURL = URL(fileURLWithPath: "/Applications")
                        panel.prompt = "Block"
                        panel.message = "Choose an app the app's agents may never open, see or control"
                        if panel.runModal() == .OK, let url = panel.url {
                            if let id = Self.bundleId(url) {
                                problem = nil
                                add([id], items: items, asText: asText)
                            } else {
                                problem = "That app has no bundle id the app can read."
                            }
                        }
                    }
                    #endif
                    if let problem {
                        Text(verbatim: problem)
                            .font(.caption)
                            .foregroundStyle(.red)
                    }
                }
                .padding(6)
                .overlay(
                    RoundedRectangle(cornerRadius: 8)
                        .strokeBorder(lineWidth: 1)
                        .foregroundStyle(over ? Color.accentColor : Color.secondary.opacity(0.4))
                )
                .onDrop(of: [UTType.fileURL], isTargeted: $over) { providers in
                    for provider in providers {
                        _ = provider.loadObject(ofClass: URL.self) { url, _ in
                            guard let url else { return }
                            Task { @MainActor in
                                if let id = Self.bundleId(url) {
                                    problem = nil
                                    add([id], items: items, asText: asText)
                                } else {
                                    problem = "That is not an app. Drop a .app from Applications."
                                }
                            }
                        }
                    }
                    return true
                }
            }
        }
    }
}

/// The hub's process row (Tauri's ProcessRow for `hubProcess`): the status in words, or "Stopped: reason"
/// with Retry while the supervisor has latched. Retry sends `hubRetry`.
struct SettingsHubProcessRow: View {
    let status: String
    let model: SettingsPageModel

    /// The status in words (Settings.tsx ProcessRow).
    static func line(_ status: String) -> String {
        if status.hasPrefix("latched:") {
            return "Stopped:\(status.dropFirst("latched:".count))"
        }
        switch status {
        case "running": return "Running"
        case "starting": return "Starting"
        case "restarting": return "Restarting"
        case "not installed": return "Not installed"
        default: return status
        }
    }

    var body: some View {
        let current = model
        let latched = status.hasPrefix("latched:")
        SettingsRowFrame(title: "Hub", note: Self.line(status)) {
            if latched {
                SettingsButton("Retry", label: "Retry the hub") {
                    Task { await current.send("hubRetry") }
                }
            }
        }
    }
}
#endif

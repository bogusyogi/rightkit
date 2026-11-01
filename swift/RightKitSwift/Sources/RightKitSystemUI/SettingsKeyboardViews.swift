#if os(macOS)
import RightKitHub
import SwiftUI

// General > Keyboard, Conveniences, Auto Quit and Window management: ports of `SettingsHotkeyRow`,
// `ConveniencesGroup`, `AutoQuitRows`, `WindowsAutoQuit` and `WindowManagementGroup` in Settings.tsx.

/// The Keyboard row that sets the global shortcut opening Settings. Off until a key is recorded.
struct SettingsHotkeyRow: View {
    let model: SettingsPageModel
    let hotkey: SettingsHotkeyState

    var body: some View {
        let windows = model.isWindows
        let raw = hotkey.shortcut ?? ""
        VStack(alignment: .leading, spacing: 0) {
            SettingsRowFrame(
                title: "Open Settings with a shortcut",
                note: windows
                    ? "Off until you set one. Needs at least one of Ctrl, Alt, Shift or the Windows key. Escape cancels."
                    : "Off until you set one. Needs at least one of Control, Option, Shift or Command. Escape cancels."
            ) {
                SettingsShortcutField(
                    display: hotkey.display ?? "", raw: raw, canClear: true,
                    onSet: { value in
                        let current = model
                        Task { await current.setValue("settingsHotkey", .string(value)) }
                    },
                    onClear: {
                        let current = model
                        Task { await current.setValue("settingsHotkey", .string("")) }
                    })
            }
            if let error = hotkey.error, !error.isEmpty {
                SettingsErrorLine(text: error)
            }
        }
    }
}

/// One window action: its shortcut, a switch for it, and Change.
struct SettingsWindowShortcutRow: View {
    let model: SettingsPageModel
    let action: SettingsWindowShortcut
    let note: String?
    @State private var editing = false

    private func set(_ value: String) {
        let current = model
        let key = "windowHotkey.\(action.id)"
        Task { await current.setValue(key, .string(value)) }
    }

    private var onBinding: Binding<Bool> {
        let on = !(action.shortcut ?? "").isEmpty
        let fallback = action.defaultShortcut ?? ""
        let edit = $editing
        let current = model
        let key = "windowHotkey.\(action.id)"
        return Binding(
            get: { on },
            set: { value in
                if !value {
                    Task { await current.setValue(key, .string("")) }
                } else if !fallback.isEmpty {
                    Task { await current.setValue(key, .string(fallback)) }
                } else {
                    edit.wrappedValue = true
                }
            }
        )
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            SettingsRowFrame(title: action.title, note: note) {
                HStack(spacing: 8) {
                    Toggle("", isOn: onBinding)
                        .labelsHidden()
                        .toggleStyle(.switch)
                        .accessibilityLabel("\(action.title) shortcut")
                    SettingsShortcutControl(
                        display: action.display ?? "", raw: action.shortcut ?? "", setLabel: "Change",
                        editing: $editing, onSet: { value in set(value) })
                }
            }
            if let error = action.error, !error.isEmpty {
                SettingsErrorLine(text: error)
            }
        }
    }
}

/// An app in the Auto Quit list, with Remove.
struct SettingsAutoQuitAppRow: View {
    let model: SettingsPageModel
    let app: SettingsAppRef
    let apps: [SettingsAppRef]
    let enabled: Bool

    var body: some View {
        SettingsRowFrame(title: app.name, note: enabled ? app.id : "\(app.id). Does nothing until Auto Quit is on.") {
            SettingsButton("Remove", kind: .ghost) {
                let current = model
                let next = apps.map { $0.id }.filter { $0 != app.id }
                Task { await current.setValue("convAutoQuitApps", SettingsJSON.strings(next)) }
            }
        }
    }
}

/// "Add a running app": the running apps that are not listed yet, each with Add.
struct SettingsAutoQuitAddRow: View {
    let model: SettingsPageModel
    let apps: [SettingsAppRef]
    let running: [SettingsAppRef]
    @State private var open = false

    var body: some View {
        let listed = apps.map { $0.id }
        let addable = running.filter { !listed.contains($0.id) }
        SettingsStackedFrame(title: "Add a running app") {
            VStack(alignment: .leading, spacing: 4) {
                SettingsButton(open ? "Hide running apps" : "Choose…", label: "Running app to add") { open.toggle() }
                if open {
                    if addable.isEmpty {
                        Text(verbatim: "Every running app is already listed.")
                            .font(Font.system(size: 12))
                    }
                    ForEach(addable, id: \.id) { app in
                        HStack {
                            Text(verbatim: app.name)
                                .frame(maxWidth: .infinity, alignment: .leading)
                            SettingsButton("Add", label: "Add \(app.name)") {
                                let current = model
                                let next = listed + [app.id]
                                open = false
                                Task { await current.setValue("convAutoQuitApps", SettingsJSON.strings(next)) }
                            }
                        }
                    }
                }
            }
        }
    }
}

enum SettingsKeyboard {
    /// The Auto Quit list rows, shown while Auto Quit is on.
    @MainActor
    static func autoQuitRows(model: SettingsPageModel, enabled: Bool, apps: [SettingsAppRef], running: [SettingsAppRef]) -> [SettingsRowSpec] {
        // While a search is under way the rows show even with Auto Quit off, so they can be found.
        if !enabled && !model.searching { return [] }
        var list = SettingsRowList()
        for app in apps {
            list.add(
                id: "autoquit.\(app.id)", text: "\(app.name) \(app.id)",
                SettingsAutoQuitAppRow(model: model, app: app, apps: apps, enabled: enabled))
        }
        list.add(id: "autoquit.add", text: "Add a running app", SettingsAutoQuitAddRow(model: model, apps: apps, running: running))
        return list.rows
    }

    /// The Keyboard card.
    @MainActor
    static func keyboardRows(model: SettingsPageModel, navigate: HubNavigateAction) -> [SettingsRowSpec] {
        var list = SettingsRowList()
        let rows = SettingsRows(model: model)
        if let hotkey = model.state?.conveniences?.settingsHotkey {
            list.add(id: "settingsHotkey", text: "Open Settings with a shortcut", SettingsHotkeyRow(model: model, hotkey: hotkey))
        }
        list.add(rows.toggle("convFnCommand"))
        if model.bool("convFnCommand"), let conveniences = model.state?.conveniences {
            let status = conveniences.fnStatus ?? ""
            let text: String
            let tone: HubTone
            if status == "running" {
                text = "Running."
                tone = .ok
            } else if status == "needsAccessibility" {
                text = "Needs Accessibility permission."
                tone = .warn
            } else {
                let detail = conveniences.fnDetail ?? ""
                text = "Failed: \(detail.isEmpty ? "unknown reason" : detail)."
                tone = .bad
            }
            list.foot(id: "fn.status", SettingsFootLine(text: text, tone: tone))
        }
        if model.bool("convFnCommand") {
            list.foot(id: "fn.manage", SettingsManagePermissions(navigate: navigate))
        }
        return list.rows
    }

    /// The Windows notch's Auto Quit card: it has no `conveniences`, so the lists come from `autoQuit`, or
    /// from the saved ids alone.
    @MainActor
    static func windowsAutoQuitRows(model: SettingsPageModel) -> [SettingsRowSpec] {
        var list = SettingsRowList()
        let rows = SettingsRows(model: model)
        let saved = model.list("convAutoQuitApps")
        let apps = model.state?.autoQuit?.autoQuitApps ?? saved.map { SettingsAppRef(id: $0, name: $0) }
        list.add(rows.toggle("convAutoQuit"))
        list.add(
            autoQuitRows(
                model: model, enabled: model.bool("convAutoQuit"), apps: apps,
                running: model.state?.autoQuit?.runningApps ?? []))
        return list.rows
    }

    /// The Conveniences card (Mac).
    @MainActor
    static func conveniencesRows(model: SettingsPageModel, conveniences c: SettingsConveniences, navigate: HubNavigateAction) -> [SettingsRowSpec] {
        var list = SettingsRowList()
        let rows = SettingsRows(model: model)
        let accessibility = c.accessibility == true
        list.add(
            id: "accessibility",
            text: "Accessibility \(accessibility ? "Allowed." : "Needs Accessibility permission. Until it is granted these stay off.")",
            SettingsButtonRow(
                title: "Accessibility",
                note: accessibility ? "Allowed." : "Needs Accessibility permission. Until it is granted these stay off.",
                button: "Permissions"
            ) {
                navigate("permissions")
            })
        list.add(rows.toggle("convFinderCutPaste", always: true))
        if model.bool("convFinderCutPaste") && accessibility && c.inputMonitoring != true {
            list.foot(
                id: "cutpaste.input",
                SettingsFootLine(text: "Key shortcuts may also need Input Monitoring (System Settings, Privacy and Security).", tone: .warn))
        }
        if let results = c.cutPasteResults, !results.isEmpty {
            let line = results.map { "\($0.name) — \($0.ok ? $0.detail : "failed: \($0.detail)")" }.joined(separator: "; ")
            list.foot(id: "cutpaste.results", SettingsFootLine(text: "Last paste: \(line)"))
        }
        // The page's own `toggle` here does not check that the notch published the key, so each row shows.
        list.add(rows.toggle("finderPathField", always: true))
        list.add(rows.toggle("convWindowMaximizer", always: true))
        list.add(rows.toggle("convDockClickMinimize", always: true))
        list.add(rows.toggle("convDiskImageInstaller", always: true))
        if model.bool("convDiskImageInstaller") || model.searching {
            list.add(rows.toggle("convDiskImageAuto", always: true))
            list.add(rows.toggle("convDiskImageAutoUpdate", always: true))
            list.add(rows.toggle("convDiskImageTrashDownload", always: true))
        }
        list.add(rows.toggle("finderMenuCut"))
        list.add(rows.toggle("finderMenuCopyPath"))
        list.add(rows.toggle("finderMenuTerminal"))
        list.add(rows.toggle("convAutoQuit", always: true))
        list.add(autoQuitRows(model: model, enabled: model.bool("convAutoQuit"), apps: c.autoQuitApps ?? [], running: c.runningApps ?? []))
        return list.rows
    }

    /// The Window management card: a switch, and while it is on one row per action, grouped.
    @MainActor
    static func windowManagementRows(model: SettingsPageModel, management: SettingsWindowManagement, accessibility: Bool) -> [SettingsRowSpec] {
        var list = SettingsRowList()
        let rows = SettingsRows(model: model)
        let windows = model.isWindows
        let on = model.bool("windowManagementEnabled")
        list.add(rows.toggle("windowManagementEnabled", title: "Move windows with shortcuts", always: true))
        if on && !accessibility {
            list.foot(id: "wm.access", SettingsFootLine(text: "Shortcuts do nothing until the app has Accessibility permission.", tone: .warn))
        }
        // The action rows show while a search is under way even with the switch off, so they can be found.
        if on || model.searching {
            let parentNote: String? = on ? nil : "Does nothing until \"Move windows with shortcuts\" is on."
            var order: [String] = []
            var titles: [String: String] = [:]
            var byGroup: [String: [SettingsWindowShortcut]] = [:]
            for action in management.actions ?? [] {
                let group = action.group ?? ""
                if byGroup[group] == nil {
                    order.append(group)
                    titles[group] = action.groupTitle ?? group
                }
                byGroup[group, default: []].append(action)
            }
            for group in order {
                list.foot(id: "wm.head.\(group)", SettingsSubhead(text: titles[group] ?? group))
                for action in byGroup[group] ?? [] {
                    list.add(
                        id: "wm.\(action.id)", text: "\(action.title) \(parentNote ?? "")",
                        SettingsWindowShortcutRow(model: model, action: action, note: parentNote))
                }
            }
            let resetNote = windows
                ? "Recording needs at least one of Ctrl, Alt, Shift or the Windows key. Escape cancels."
                : "Recording needs at least one of Control, Option, Shift or Command. Escape cancels."
            list.add(
                id: "wm.reset", text: "Shortcuts \(resetNote)",
                SettingsButtonRow(
                    title: "Shortcuts",
                    note: resetNote,
                    button: "Reset to defaults"
                ) {
                    let current = model
                    let reset: SettingsJSON = windows ? .array([]) : .object([:])
                    Task { await current.setValue("windowHotkeys", reset) }
                })
        }
        return list.rows
    }
}

/// "Manage permissions": opens the Permissions section.
struct SettingsManagePermissions: View {
    let navigate: HubNavigateAction
    @Environment(\.hubTheme) private var theme

    var body: some View {
        HStack {
            SettingsButton("Manage permissions", kind: .ghost) { navigate("permissions") }
            Spacer()
        }
        .padding(.horizontal, theme.spacing.md + 2)
        .padding(.bottom, theme.spacing.row)
    }
}
#endif

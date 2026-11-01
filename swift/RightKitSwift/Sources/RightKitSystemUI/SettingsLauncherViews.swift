#if os(macOS)
import RightKitHub
import SwiftUI

// General > Launcher and its clipboard rows: port of the Launcher block in Settings.tsx and of
// hub/src/views/LauncherSettings.tsx.

/// The launcher's user-made items. The notch stores them as one JSON string (`launcherConfig`).
struct SettingsLauncherConfig: Codable, Equatable, Sendable {
    struct AppHotkey: Codable, Equatable, Sendable {
        var path = ""
        var hotkey = ""

        init() {}

        init(from decoder: any Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            path = (try? container.decodeIfPresent(String.self, forKey: .path)) ?? ""
            hotkey = (try? container.decodeIfPresent(String.self, forKey: .hotkey)) ?? ""
        }
    }

    struct Quicklink: Codable, Equatable, Sendable {
        var name = ""
        var keyword = ""
        var template = ""

        init() {}

        init(from decoder: any Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            name = (try? container.decodeIfPresent(String.self, forKey: .name)) ?? ""
            keyword = (try? container.decodeIfPresent(String.self, forKey: .keyword)) ?? ""
            template = (try? container.decodeIfPresent(String.self, forKey: .template)) ?? ""
        }
    }

    struct Snippet: Codable, Equatable, Sendable {
        var name = ""
        var keyword = ""
        var body = ""

        init() {}

        init(from decoder: any Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            name = (try? container.decodeIfPresent(String.self, forKey: .name)) ?? ""
            keyword = (try? container.decodeIfPresent(String.self, forKey: .keyword)) ?? ""
            body = (try? container.decodeIfPresent(String.self, forKey: .body)) ?? ""
        }
    }

    struct Command: Codable, Equatable, Sendable {
        var name = ""
        var keyword = ""
        var command = ""
        var hotkey = ""

        init() {}

        init(from decoder: any Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            name = (try? container.decodeIfPresent(String.self, forKey: .name)) ?? ""
            keyword = (try? container.decodeIfPresent(String.self, forKey: .keyword)) ?? ""
            command = (try? container.decodeIfPresent(String.self, forKey: .command)) ?? ""
            hotkey = (try? container.decodeIfPresent(String.self, forKey: .hotkey)) ?? ""
        }
    }

    struct Alias: Codable, Equatable, Sendable {
        /// The shortcut's name, as the Shortcuts app shows it.
        var shortcut = ""
        /// 1 to 16 letters or digits, unique without regard to case.
        var alias = ""

        init() {}

        init(shortcut: String, alias: String) {
            self.shortcut = shortcut
            self.alias = alias
        }

        init(from decoder: any Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            shortcut = (try? container.decodeIfPresent(String.self, forKey: .shortcut)) ?? ""
            alias = (try? container.decodeIfPresent(String.self, forKey: .alias)) ?? ""
        }
    }

    var pinnedApps: [String] = []
    var fileFolders: [String] = []
    var appHotkeys: [AppHotkey] = []
    var quicklinks: [Quicklink] = []
    var snippets: [Snippet] = []
    var commands: [Command] = []
    var notesEnabled = true
    var shortcutAliases: [Alias] = []

    init() {}

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        pinnedApps = (try? container.decodeIfPresent([String].self, forKey: .pinnedApps)) ?? []
        fileFolders = (try? container.decodeIfPresent([String].self, forKey: .fileFolders)) ?? []
        appHotkeys = (try? container.decodeIfPresent([AppHotkey].self, forKey: .appHotkeys)) ?? []
        quicklinks = (try? container.decodeIfPresent([Quicklink].self, forKey: .quicklinks)) ?? []
        snippets = (try? container.decodeIfPresent([Snippet].self, forKey: .snippets)) ?? []
        commands = (try? container.decodeIfPresent([Command].self, forKey: .commands)) ?? []
        notesEnabled = (try? container.decodeIfPresent(Bool.self, forKey: .notesEnabled)) ?? true
        shortcutAliases = (try? container.decodeIfPresent([Alias].self, forKey: .shortcutAliases)) ?? []
    }

    static func parse(_ raw: String) -> SettingsLauncherConfig {
        if raw.isEmpty { return SettingsLauncherConfig() }
        return (try? JSONDecoder().decode(SettingsLauncherConfig.self, from: Data(raw.utf8))) ?? SettingsLauncherConfig()
    }

    func json() -> String {
        guard let data = try? JSONEncoder().encode(self) else { return "{}" }
        return String(decoding: data, as: UTF8.self)
    }

    /// The problem with the alias at `index`, or nil when it is usable (`aliasProblem`).
    static func aliasProblem(_ list: [Alias], _ index: Int) -> String? {
        let shortcut = list[index].shortcut.trimmingCharacters(in: .whitespaces)
        let alias = list[index].alias.trimmingCharacters(in: .whitespaces)
        if shortcut.isEmpty || alias.isEmpty { return "Enter the shortcut name and an alias." }
        let allowed = alias.count <= 16 && alias.allSatisfy { $0.isASCII && ($0.isLetter || $0.isNumber) }
        if !allowed { return "The alias is 1 to 16 letters or digits." }
        let taken = list.enumerated().contains { other in
            other.offset != index && other.element.alias.trimmingCharacters(in: .whitespaces).lowercased() == alias.lowercased()
        }
        return taken ? "Another alias already uses this." : nil
    }
}

enum SettingsLauncher {
    /// The Launcher card: the switch, then (while it is on) its shortcut and sources and the lists, then
    /// Clipboard history and, while that is on, the clipboard rows.
    @MainActor
    static func rows(model: SettingsPageModel) -> [SettingsRowSpec] {
        var list = SettingsRowList()
        let rows = SettingsRows(model: model)
        let windows = model.isWindows
        let searching = model.searching
        list.add(rows.toggle("launcherEnabled"))
        // While a search is under way the rows that need the switch on show anyway, so they can be found.
        if model.bool("launcherEnabled") || (searching && model.has("launcherEnabled")) {
            list.add(rows.choice("launcherHotkey", segmented: false))
            if !windows {
                list.foot(
                    id: "launcherHotkey.note",
                    SettingsFootLine(text: "Command space only works after Spotlight's shortcut is turned off in System Settings."))
            }
            if let status = model.state?.launcherStatus, !status.isEmpty {
                list.foot(id: "launcherStatus", alert: true, SettingsErrorLine(text: status))
            }
            list.add(rows.choice("launcherHotkeyConflict", segmented: false))
            list.add(rows.choice("launcherAskTarget", segmented: false))
            list.add(rows.toggle("launcherWebSearch"))
            list.add(rows.choice("launcherWebSearchEngine"))
            list.add(rows.toggle("launcherDictionary"))
            list.add(rows.toggle("launcherShortcuts"))
            list.add(rows.toggle("launcherCalculator"))
            list.add(rows.toggle("launcherPulseCommands"))
            if !windows {
                list.add(rows.toggle("launcherSnippetExpansion"))
            }
            list.add(rows.toggle("launcherCurrency"))
            if model.has("launcherConfig") {
                list.add(
                    id: "launcherLists",
                    text: "Folders for file search Pinned apps App hotkeys Quicklinks Snippets Shortcut aliases Notes Custom commands",
                    SettingsLauncherLists(model: model, raw: model.text("launcherConfig")))
            }
        }
        list.add(rows.toggle("launcherClipboard"))
        if model.bool("launcherClipboard") || (searching && model.has("launcherClipboard")) {
            list.add(clipboardRows(model: model, rows: rows))
        }
        return list.rows
    }

    @MainActor
    private static func clipboardRows(model: SettingsPageModel, rows: SettingsRows) -> [SettingsRowSpec] {
        var list = SettingsRowList()
        if !model.searching {
            list.foot(id: "clipboard.head", SettingsSubhead(text: "Clipboard"))
        }
        list.add(rows.toggle("clipboardStripEnabled"))
        if model.has("clipboardHotkey") {
            list.add(id: "clipboardHotkey", text: "Clipboard strip shortcut", SettingsClipboardHotkeyRow(model: model))
            if let status = model.state?.clipboardStatus, !status.isEmpty {
                list.foot(id: "clipboardHotkey.status", alert: true, SettingsErrorLine(text: status))
            }
        }
        list.add(rows.choice("clipboardCardSize"))
        list.add(rows.choice("clipboardStripPlacement"))
        list.add(rows.toggle("clipboardShowNumbers"))
        list.add(rows.choice("clipboardPasteMode"))
        list.add(rows.toggle("clipboardPlainDefault"))
        list.add(rows.toggle("clipboardWriteBackExcluded"))
        list.add(rows.toggle("clipboardCaptureImages"))
        list.add(rows.toggle("clipboardCaptureFiles"))
        list.add(rows.toggle("clipboardCaptureRich"))
        list.add(rows.number("clipboardMaxItems", min: 50, max: 5000))
        list.add(rows.number("clipboardMaxMB", min: 20, max: 4096))
        list.add(rows.number("clipboardMaxAgeDays", min: 0, max: 365))
        list.add(rows.number("clipboardScreenshotHours", min: 0, max: 720))
        list.add(rows.toggle("clipboardSkipAutoGenerated"))
        list.add(rows.toggle("clipboardSkipRemote"))
        list.add(rows.toggle("clipboardSecretFilter"))
        list.add(rows.list("clipboardExcludedApps", placeholder: "com.example.passwords"))
        list.add(rows.list("clipboardIgnoredTypes", placeholder: "org.nspasteboard.ConcealedType"))
        list.add(rows.list("clipboardShiftPasteApps", placeholder: "Windows Terminal"))
        list.add(rows.toggle("clipboardRememberBoard"))
        list.add(
            id: "clipboardClear", text: "Clear clipboard history Deletes every clipboard item",
            SettingsClipboardClearRow(model: model, count: model.state?.clipboardCount, bytes: model.state?.clipboardBytes))
        return list.rows
    }
}

/// A small heading inside a card (`.ck-subhead`).
struct SettingsSubhead: View {
    let text: String
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        Text(verbatim: text)
            .font(Font.system(size: 12, weight: .semibold))
            .foregroundStyle(palette.ink2.color)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, theme.spacing.md + 2)
            .padding(.top, theme.spacing.sm)
            .padding(.bottom, 2)
    }
}

/// The clipboard strip's shortcut. Its value uses the notch's grammar (alt for Option, no Windows key).
struct SettingsClipboardHotkeyRow: View {
    let model: SettingsPageModel

    var body: some View {
        let windows = model.isWindows
        let raw = model.text("clipboardHotkey")
        let note = SettingsNotes.note("clipboardHotkey", model.noteContext)
        SettingsRowFrame(title: "Clipboard strip shortcut", note: note) {
            SettingsShortcutField(
                display: SettingsShortcut.clipboardDisplay(raw, windows: windows), raw: raw,
                optionName: "alt", allowCommand: !windows
            ) { value in
                let current = model
                Task { await current.setValue("clipboardHotkey", .string(value)) }
            }
        }
    }
}

/// "Clear clipboard history": a confirm, then the notch's `clipboardClear`. Counts only; the hub never
/// reads the items.
struct SettingsClipboardClearRow: View {
    let model: SettingsPageModel
    let count: Double?
    let bytes: Double?
    @State private var confirm = false

    private var items: String {
        guard let count else { return "all items" }
        let whole = Int(count)
        return "\(whole) item\(whole == 1 ? "" : "s")"
    }

    private var size: String {
        guard let bytes else { return "" }
        let megabyte = 1024.0 * 1024.0
        if bytes < megabyte && bytes > 0 { return ", under 1 MB" }
        let value = bytes / megabyte
        let rounded = bytes < 10 * megabyte ? ((value * 10).rounded() / 10) : value.rounded()
        let text = bytes < 10 * megabyte ? "\(Int(rounded)).\(Int((rounded * 10).rounded()) % 10)" : "\(Int(rounded))"
        return ", \(text) MB"
    }

    var body: some View {
        let lead = count == nil ? "Deletes every clipboard item" : "\(items)\(size)."
        SettingsRowFrame(
            title: "Clear clipboard history",
            note: "\(lead) Deletes every item, including pinned and board items; boards stay. Nothing is wiped securely."
        ) {
            SettingsButton("Clear…", enabled: count != 0) { confirm = true }
        }
        .hubConfirm(
            "Clear clipboard history?",
            message: "Delete \(items)? This can't be undone. Pinned and board items are deleted too; the boards stay.",
            confirmLabel: "Delete",
            isPresented: $confirm
        ) {
            let current = model
            Task { await current.send("clipboardClear") }
        }
    }
}

/// The launcher's lists: folders for file search, pinned apps, app hotkeys, quicklinks, snippets, shortcut
/// aliases, notes and custom commands. Changes are held until Save, then sent as one
/// `launcherConfig` value.
struct SettingsLauncherLists: View {
    let model: SettingsPageModel
    let raw: String
    @State private var draft: SettingsLauncherConfig
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    init(model: SettingsPageModel, raw: String) {
        self.model = model
        self.raw = raw
        _draft = State(initialValue: SettingsLauncherConfig.parse(raw))
    }

    private var palette: HubPalette { theme.palette(scheme) }

    private var dirty: Bool { draft != SettingsLauncherConfig.parse(raw) }

    private var aliasesUsable: Bool {
        for index in draft.shortcutAliases.indices where SettingsLauncherConfig.aliasProblem(draft.shortcutAliases, index) != nil {
            return false
        }
        return true
    }

    private func save() {
        var next = draft
        next.shortcutAliases = next.shortcutAliases.map {
            SettingsLauncherConfig.Alias(
                shortcut: $0.shortcut.trimmingCharacters(in: .whitespaces),
                alias: $0.alias.trimmingCharacters(in: .whitespaces))
        }
        let current = model
        let text = next.json()
        Task { await current.setValue("launcherConfig", .string(text)) }
    }

    /// A text binding into `draft` that reads and writes nothing once its row is gone.
    private func field(
        _ read: @escaping @Sendable (SettingsLauncherConfig) -> String,
        _ write: @escaping @Sendable (inout SettingsLauncherConfig, String) -> Void
    ) -> Binding<String> {
        let source = $draft
        return Binding(
            get: { read(source.wrappedValue) },
            set: { value in
                var copy = source.wrappedValue
                write(&copy, value)
                source.wrappedValue = copy
            }
        )
    }

    private func input(_ placeholder: String, _ binding: Binding<String>) -> some View {
        TextField(placeholder, text: binding)
            .textFieldStyle(.roundedBorder)
            .accessibilityLabel(placeholder)
    }

    var body: some View {
        let windows = model.isWindows
        let hotkeyNote = "Needs a modifier: for example opt+1, cmd+shift+k or ctrl+opt+d."
        VStack(alignment: .leading, spacing: 0) {
            Divider()
            SettingsStackedFrame(
                title: "Folders for file search",
                note: windows
                    ? "The launcher searches file names only inside these folders. Empty turns file search off."
                    : "the app searches file names itself, with no Spotlight index, only inside these folders. Empty turns file search off. Command space stays taken until Spotlight's shortcut is turned off in System Settings."
            ) {
                VStack(alignment: .leading, spacing: 6) {
                    ForEach(Array(draft.fileFolders.enumerated()), id: \.offset) { entry in
                        SettingsPathLine(text: entry.element) { draft.fileFolders.remove(at: entry.offset) }
                    }
                    SettingsAddLine(
                        placeholder: windows ? "C:\\Users\\you\\Documents" : "/Users/you/Documents",
                        choosePrompt: "Add a folder for file search"
                    ) { value in
                        draft.fileFolders.append(value)
                    }
                }
            }
            Divider()
            SettingsStackedFrame(
                title: "Pinned apps",
                note: windows
                    ? "Pin an app in the launcher with Alt+P. Pinned apps show first when nothing is typed."
                    : "Pin an app in the launcher with Command-P. Pinned apps show first when nothing is typed."
            ) {
                VStack(alignment: .leading, spacing: 6) {
                    ForEach(Array(draft.pinnedApps.enumerated()), id: \.offset) { entry in
                        SettingsPathLine(text: entry.element) { draft.pinnedApps.remove(at: entry.offset) }
                    }
                    SettingsAddLine(placeholder: windows ? "C:\\Program Files\\App\\app.exe" : "/Applications/Safari.app") { value in
                        draft.pinnedApps.append(value)
                    }
                }
            }
            if !windows {
                Divider()
                appHotkeys(note: "Press to show the app, or hide it when it is in front. \(hotkeyNote)")
            }
            Divider()
            quicklinks
            Divider()
            snippets
            if !windows {
                Divider()
                aliases
            }
            Divider()
            notes(windows: windows)
            Divider()
            commands(windows: windows)
            Divider()
            saveBar
        }
        .task(id: raw) {
            draft = SettingsLauncherConfig.parse(raw)
        }
    }

    private func appHotkeys(note: String) -> some View {
        SettingsStackedFrame(title: "App hotkeys", note: note) {
            VStack(alignment: .leading, spacing: 6) {
                ForEach(Array(draft.appHotkeys.indices), id: \.self) { i in
                    HStack(spacing: theme.spacing.sm) {
                        input("/Applications/Safari.app", field({ i < $0.appHotkeys.count ? $0.appHotkeys[i].path : "" }, { if i < $0.appHotkeys.count { $0.appHotkeys[i].path = $1 } }))
                        input("opt+1", field({ i < $0.appHotkeys.count ? $0.appHotkeys[i].hotkey : "" }, { if i < $0.appHotkeys.count { $0.appHotkeys[i].hotkey = $1 } }))
                            .frame(width: 120)
                        SettingsButton("Remove", kind: .ghost) { if i < draft.appHotkeys.count { draft.appHotkeys.remove(at: i) } }
                    }
                }
                SettingsButton("Add an app hotkey") { draft.appHotkeys.append(SettingsLauncherConfig.AppHotkey()) }
            }
        }
    }

    private var quicklinks: some View {
        SettingsStackedFrame(
            title: "Quicklinks",
            note: "{query} is replaced by the text after the keyword. {clipboard} and {date} are also filled in."
        ) {
            VStack(alignment: .leading, spacing: 6) {
                ForEach(Array(draft.quicklinks.indices), id: \.self) { i in
                    HStack(spacing: theme.spacing.sm) {
                        input("Name", field({ i < $0.quicklinks.count ? $0.quicklinks[i].name : "" }, { if i < $0.quicklinks.count { $0.quicklinks[i].name = $1 } }))
                            .frame(width: 110)
                        input("Keyword", field({ i < $0.quicklinks.count ? $0.quicklinks[i].keyword : "" }, { if i < $0.quicklinks.count { $0.quicklinks[i].keyword = $1 } }))
                            .frame(width: 110)
                        input("https://github.com/search?q={query}", field({ i < $0.quicklinks.count ? $0.quicklinks[i].template : "" }, { if i < $0.quicklinks.count { $0.quicklinks[i].template = $1 } }))
                        SettingsButton("Remove", kind: .ghost) { if i < draft.quicklinks.count { draft.quicklinks.remove(at: i) } }
                    }
                }
                SettingsButton("Add a quicklink") { draft.quicklinks.append(SettingsLauncherConfig.Quicklink()) }
            }
        }
    }

    private var snippets: some View {
        SettingsStackedFrame(
            title: "Snippets",
            note: "Markdown text pasted into the app you were using. {date}, {clipboard} and {argument} are filled in. {snippet:Name} includes another snippet by its name or keyword; a snippet that includes itself is left as written. To expand a snippet wherever you type, give it a keyword that starts with a symbol, such as ;sig, and turn on “Expand snippet keywords as you type” above."
        ) {
            VStack(alignment: .leading, spacing: 8) {
                ForEach(Array(draft.snippets.indices), id: \.self) { i in
                    VStack(alignment: .leading, spacing: 4) {
                        HStack(spacing: theme.spacing.sm) {
                            input("Name", field({ i < $0.snippets.count ? $0.snippets[i].name : "" }, { if i < $0.snippets.count { $0.snippets[i].name = $1 } }))
                            input("Keyword", field({ i < $0.snippets.count ? $0.snippets[i].keyword : "" }, { if i < $0.snippets.count { $0.snippets[i].keyword = $1 } }))
                            SettingsButton("Remove", kind: .ghost) { if i < draft.snippets.count { draft.snippets.remove(at: i) } }
                        }
                        TextField("Thanks, {clipboard}", text: field({ i < $0.snippets.count ? $0.snippets[i].body : "" }, { if i < $0.snippets.count { $0.snippets[i].body = $1 } }), axis: .vertical)
                            .textFieldStyle(.roundedBorder)
                            .lineLimit(4)
                            .accessibilityLabel("Snippet text")
                    }
                }
                SettingsButton("Add a snippet") { draft.snippets.append(SettingsLauncherConfig.Snippet()) }
            }
        }
    }

    private var aliases: some View {
        SettingsStackedFrame(
            title: "Shortcut aliases",
            note: "Type an alias in the launcher to put that Apple Shortcut first. Use the shortcut's exact name from the Shortcuts app. An alias is 1 to 16 letters or digits, and each is unique (case does not count). Global hotkeys for single shortcuts are not available yet."
        ) {
            VStack(alignment: .leading, spacing: 6) {
                ForEach(Array(draft.shortcutAliases.indices), id: \.self) { i in
                    VStack(alignment: .leading, spacing: 2) {
                        HStack(spacing: theme.spacing.sm) {
                            input("Shortcut name", field({ i < $0.shortcutAliases.count ? $0.shortcutAliases[i].shortcut : "" }, { if i < $0.shortcutAliases.count { $0.shortcutAliases[i].shortcut = $1 } }))
                            input("Alias", field({ i < $0.shortcutAliases.count ? $0.shortcutAliases[i].alias : "" }, { if i < $0.shortcutAliases.count { $0.shortcutAliases[i].alias = $1 } }))
                                .frame(width: 120)
                            SettingsButton("Remove", kind: .ghost) { if i < draft.shortcutAliases.count { draft.shortcutAliases.remove(at: i) } }
                        }
                        if i < draft.shortcutAliases.count, let problem = SettingsLauncherConfig.aliasProblem(draft.shortcutAliases, i) {
                            Text(verbatim: problem)
                                .font(theme.type.caption.font)
                                .foregroundStyle(palette.ink2.color)
                        }
                    }
                }
                SettingsAliasAdd(existing: draft.shortcutAliases) { value in
                    draft.shortcutAliases.append(value)
                }
            }
        }
    }

    private func notes(windows: Bool) -> some View {
        SettingsStackedFrame(
            title: "Notes",
            note: windows
                ? "Type “notes” in the launcher to list or search your notes, “note text” to start one with that text, and “notes trash” for deleted ones, which are kept for 30 days. Each note is a Markdown file on this PC and opens in the program Windows uses for it; Alt+D deletes the selected note."
                : "Type “notes” in the launcher to list, search or start a note; “note text” starts one with that text. Notes are kept on this Mac only, and deleted ones can be restored for 30 days with “notes trash”."
        ) {
            Toggle("Notes in the launcher", isOn: $draft.notesEnabled)
        }
    }

    private func commands(windows: Bool) -> some View {
        SettingsStackedFrame(
            title: "Custom commands",
            note: windows
                ? "Run with cmd.exe /C; the output shows in the launcher. A command is stopped after 30 seconds."
                : "Run with /bin/zsh -lc; the output shows in the launcher. Optional hotkey runs it from anywhere."
        ) {
            VStack(alignment: .leading, spacing: 6) {
                ForEach(Array(draft.commands.indices), id: \.self) { i in
                    HStack(spacing: theme.spacing.sm) {
                        input("Name", field({ i < $0.commands.count ? $0.commands[i].name : "" }, { if i < $0.commands.count { $0.commands[i].name = $1 } }))
                            .frame(width: 100)
                        input("Keyword", field({ i < $0.commands.count ? $0.commands[i].keyword : "" }, { if i < $0.commands.count { $0.commands[i].keyword = $1 } }))
                            .frame(width: 100)
                        input(windows ? "ipconfig" : "df -h /", field({ i < $0.commands.count ? $0.commands[i].command : "" }, { if i < $0.commands.count { $0.commands[i].command = $1 } }))
                        if !windows {
                            input("hotkey", field({ i < $0.commands.count ? $0.commands[i].hotkey : "" }, { if i < $0.commands.count { $0.commands[i].hotkey = $1 } }))
                                .frame(width: 100)
                        }
                        SettingsButton("Remove", kind: .ghost) { if i < draft.commands.count { draft.commands.remove(at: i) } }
                    }
                }
                SettingsButton("Add a command") { draft.commands.append(SettingsLauncherConfig.Command()) }
            }
        }
    }

    private var saveBar: some View {
        HStack {
            Text(verbatim: dirty ? "Not saved yet." : "Launcher lists are saved.")
                .font(theme.type.caption.font)
                .foregroundStyle(palette.ink2.color)
            Spacer()
            SettingsButton("Save launcher lists", enabled: dirty && aliasesUsable) { save() }
        }
        .padding(.horizontal, theme.spacing.md + 2)
        .padding(.vertical, theme.spacing.sm)
        .frame(minHeight: 48)
    }
}

/// Two fields and an Add button for a new shortcut alias; it adds only a usable alias.
struct SettingsAliasAdd: View {
    let existing: [SettingsLauncherConfig.Alias]
    let onAdd: @MainActor (SettingsLauncherConfig.Alias) -> Void
    @State private var shortcut = ""
    @State private var alias = ""
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    private var typed: Bool {
        !shortcut.trimmingCharacters(in: .whitespaces).isEmpty || !alias.trimmingCharacters(in: .whitespaces).isEmpty
    }

    private var problem: String? {
        if !typed { return nil }
        let candidate = existing + [SettingsLauncherConfig.Alias(shortcut: shortcut, alias: alias)]
        return SettingsLauncherConfig.aliasProblem(candidate, candidate.count - 1)
    }

    private func add() {
        if problem != nil || !typed { return }
        onAdd(SettingsLauncherConfig.Alias(
            shortcut: shortcut.trimmingCharacters(in: .whitespaces),
            alias: alias.trimmingCharacters(in: .whitespaces)))
        shortcut = ""
        alias = ""
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: theme.spacing.sm) {
                TextField("Toggle Speakers", text: $shortcut)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityLabel("Shortcut name")
                TextField("ts", text: $alias)
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 120)
                    .accessibilityLabel("Alias")
                    .onSubmit { add() }
                SettingsButton("Add", enabled: problem == nil && typed) { add() }
            }
            if let problem {
                Text(verbatim: problem)
                    .font(theme.type.caption.font)
                    .foregroundStyle(palette.ink2.color)
            }
        }
    }
}
#endif

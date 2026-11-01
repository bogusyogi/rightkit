#if os(macOS)
import RightKitHub
import SwiftUI

// The building blocks of the Settings pages. They stand in for the page's `settingsKit.tsx` (Group,
// Row, Item) and its controls, drawn only from the controls the hub rules allow: a SegmentedControl
// becomes a row of Buttons or a `HubChoiceList`, a range slider becomes a stepper, a text area becomes
// a list of rows, and a key recorder becomes a text field in the notch's shortcut grammar.

// MARK: - Rows in a card

/// One row of a group card. A foot is a note that belongs to the row above it (no divider between).
/// `text` is what the page's search matches (the row's name and note); a pinned row stays while searching.
/// A foot that is an alert (an error) stays while searching; any other foot goes.
struct SettingsRowSpec: Identifiable {
    let id: String
    let isFoot: Bool
    let text: String
    let isPinned: Bool
    let isAlert: Bool
    let view: AnyView

    init<Content: View>(
        id: String, isFoot: Bool = false, text: String = "", isPinned: Bool = false, isAlert: Bool = false,
        view: Content
    ) {
        self.id = id
        self.isFoot = isFoot
        self.text = text
        self.isPinned = isPinned
        self.isAlert = isAlert
        self.view = AnyView(view)
    }
}

/// Collects the rows of a card, skipping those a page leaves out.
struct SettingsRowList {
    private(set) var rows: [SettingsRowSpec] = []

    mutating func add(_ spec: SettingsRowSpec?) {
        if let spec { rows.append(spec) }
    }

    mutating func add(_ specs: [SettingsRowSpec]) {
        rows.append(contentsOf: specs)
    }

    mutating func add<Content: View>(id: String, text: String = "", pinned: Bool = false, _ view: Content) {
        rows.append(SettingsRowSpec(id: id, text: text, isPinned: pinned, view: view))
    }

    mutating func foot<Content: View>(id: String, alert: Bool = false, _ view: Content) {
        rows.append(SettingsRowSpec(id: id, isFoot: true, isAlert: alert, view: view))
    }

    var isEmpty: Bool { rows.isEmpty }
}

/// A heading above a card of rows (`.ck-sgroup`).
struct SettingsGroupCard: View {
    let title: String
    let rows: [SettingsRowSpec]
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        VStack(alignment: .leading, spacing: theme.spacing.sm - 2) {
            Text(verbatim: title)
                .font(Font.system(size: 13, weight: .semibold))
                .foregroundStyle(palette.ink.color)
                .padding(.leading, 2)
            HubCard(padding: 0) {
                // The Tauri card keeps its rows 12 pt in from the border (so each divider is inset) and
                // 8 pt below the top and above the bottom.
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(rows) { row in
                        if row.id != rows.first?.id && !row.isFoot {
                            Divider()
                        }
                        row.view
                    }
                }
                .padding(.horizontal, theme.spacing.md)
                .padding(.vertical, theme.spacing.sm)
            }
        }
    }
}

/// The page: a scrolling column of groups (`.ck-settings`).
struct SettingsPageFrame<Content: View>: View {
    private let content: Content
    @Environment(\.hubTheme) private var theme

    init(@ViewBuilder content: () -> Content) {
        self.content = content()
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: theme.spacing.lg - 2) {
                content
            }
            .padding(.horizontal, theme.spacing.xl - 2)
            .padding(.vertical, theme.spacing.lg - 2)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

/// A row: the name and a note on the leading side, a control on the trailing side (`.ck-set`).
struct SettingsRowFrame<Trailing: View>: View {
    private let title: String
    private let note: String?
    private let trailing: Trailing
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    init(title: String, note: String? = nil, @ViewBuilder trailing: () -> Trailing) {
        self.title = title
        self.note = note
        self.trailing = trailing()
    }

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        HStack(alignment: .center, spacing: theme.spacing.lg) {
            SettingsRowText(title: title, note: note)
                .frame(maxWidth: .infinity, alignment: .leading)
            // The controls keep their width on Windows' wider font ("Choose…", "Save" never wrap);
            // the row's text gives way first.
            trailing
                .layoutPriority(2)
        }
        .padding(.horizontal, theme.spacing.md + 2)
        .padding(.vertical, theme.spacing.sm)
        .frame(minHeight: 48)
    }
}

/// A row whose controls sit under its text (lists, button groups).
struct SettingsStackedFrame<Content: View>: View {
    private let title: String
    private let note: String?
    private let content: Content
    @Environment(\.hubTheme) private var theme

    init(title: String, note: String? = nil, @ViewBuilder content: () -> Content) {
        self.title = title
        self.note = note
        self.content = content()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: theme.spacing.sm) {
            SettingsRowText(title: title, note: note)
            content
        }
        .padding(.horizontal, theme.spacing.md + 2)
        .padding(.vertical, theme.spacing.sm + 2)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

struct SettingsRowText: View {
    let title: String
    let note: String?
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(verbatim: title)
                .font(Font.system(size: 13, weight: .medium))
                .foregroundStyle(palette.ink.color)
            if let note, !note.isEmpty {
                Text(verbatim: note)
                    .font(theme.type.caption.font)
                    .foregroundStyle(palette.ink2.color)
            }
        }
    }
}

/// A note or a message under a row (`.ck-foot`), in a tone.
struct SettingsFootLine: View {
    let text: String
    var tone: HubTone = .neutral
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        Text(verbatim: text)
            .font(theme.type.caption.font)
            .foregroundStyle(tone == .neutral ? palette.ink2.color : tone.color(in: palette).color)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, theme.spacing.md + 2)
            .padding(.top, 2)
            .padding(.bottom, theme.spacing.row)
    }
}

/// An error line in the bad colour.
struct SettingsErrorLine: View {
    let text: String

    var body: some View {
        SettingsFootLine(text: text, tone: .bad)
    }
}

/// A small status word in a rounded chip (`.ck-status`); colour only reinforces the text.
struct SettingsBadge: View {
    let text: String
    var tone: HubTone = .neutral
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    private var fill: HubColor {
        switch tone {
        case .ok: return palette.okSoft
        case .warn: return palette.warnSoft
        case .bad: return palette.badSoft
        case .accent: return palette.accentSoft
        case .neutral: return palette.fill2
        }
    }

    var body: some View {
        HubLayered {
            RoundedRectangle(cornerRadius: theme.radii.control).fill(fill.color)
            Text(verbatim: text)
                .font(theme.type.caption.font)
                .foregroundStyle(tone == .neutral ? palette.ink2.color : tone.color(in: palette).color)
                .lineLimit(1)
                .padding(.horizontal, theme.spacing.sm)
                .padding(.vertical, 2)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(text)
    }
}

/// The one-line summary at the top of Permissions (`.ck-lead`).
struct SettingsLead: View {
    let text: String
    let tone: HubTone
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        HubLayered {
            RoundedRectangle(cornerRadius: theme.radii.item).fill((tone == .warn ? palette.warnSoft : palette.okSoft).color)
            Text(verbatim: text)
                .font(Font.system(size: 12, weight: .medium))
                .foregroundStyle(tone.color(in: palette).color)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, theme.spacing.md)
                .padding(.vertical, theme.spacing.sm)
        }
        .padding(.horizontal, theme.spacing.md + 2)
        .padding(.vertical, theme.spacing.row)
    }
}

/// The page's status or error line above the groups (`.ck-sub` / `.error`).
struct SettingsStatusLine: View {
    let text: String
    var tone: HubTone = .neutral
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        if tone == .bad {
            HubBanner(text, tone: .bad)
        } else {
            Text(verbatim: text)
                .font(theme.type.caption.font)
                .foregroundStyle(palette.ink2.color)
        }
    }
}

// MARK: - Setting rows (over the kit's model)

@MainActor
enum SettingsText {
    /// The line under a setting: the caller's own, else the Tauri page's (`SettingsNotes`), else none.
    static func note(_ setting: HubSetting, model: SettingsPageModel, override: String? = nil) -> String? {
        if let override { return override }
        return SettingsNotes.note(setting.key, model.noteContext)
    }

    /// What a search matches for a setting's row: its name and its note.
    static func searchText(_ setting: HubSetting, model: SettingsPageModel, override: String? = nil) -> String {
        "\(setting.title) \(note(setting, model: model, override: override) ?? "")"
    }
}

/// An on/off setting: a switch that reads and writes through the kit's model.
struct SettingsToggleRow: View {
    let setting: HubSetting
    let model: SettingsPageModel
    var note: String?
    /// The switch's accessible name when it differs from the row's title (Tauri names the Nearby
    /// switch "Nearby sharing" under the row "Send and receive files nearby").
    var switchName: String?

    private var binding: Binding<Bool> {
        let current = model
        let item = setting
        return Binding(
            get: { MainActor.assumeIsolated { current.kit.isOn(item) } },
            set: { on in
                Task { await current.kit.set(item, .bool(on)) }
            }
        )
    }

    var body: some View {
        SettingsRowFrame(title: setting.title, note: SettingsText.note(setting, model: model, override: note)) {
            Toggle("", isOn: binding)
                .labelsHidden()
                .toggleStyle(.switch)
                .accessibilityLabel(switchName ?? setting.title)
        }
    }
}

/// A one-of-few setting: up to four options side by side (the page's SegmentedControl), a select
/// (a button that opens the list of options under the row) otherwise.
struct SettingsChoiceRow: View {
    let setting: HubSetting
    let model: SettingsPageModel
    var segmented = true
    var note: String?

    private var binding: Binding<String> {
        let current = model
        let item = setting
        return Binding(
            get: { MainActor.assumeIsolated { current.kit.text(for: item) } },
            set: { value in
                Task { await current.kit.set(item, .string(value)) }
            }
        )
    }

    var body: some View {
        if case .choice(let choices) = setting.kind {
            let line = SettingsText.note(setting, model: model, override: note)
            if segmented && choices.count <= 4 {
                SettingsRowFrame(title: setting.title, note: line) {
                    SettingsSegmented(label: setting.title, options: choices, selection: binding)
                }
            } else {
                SettingsSelectRow(title: setting.title, note: line, options: choices, selection: binding)
            }
        }
    }
}

/// A select (the page's `<select class="ck-select">`): one button named for the setting that shows the
/// chosen option; pressing it lists the options under the row, and choosing one closes the list.
struct SettingsSelectRow: View {
    let title: String
    var note: String?
    let options: [HubChoice]
    let selection: Binding<String>
    @State private var open = false
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    private var chosen: String {
        options.first(where: { $0.value == selection.wrappedValue })?.title ?? selection.wrappedValue
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            SettingsRowFrame(title: title, note: note) {
                Button {
                    open.toggle()
                } label: {
                    HubLayered {
                        RoundedRectangle(cornerRadius: theme.radii.item).fill(palette.plane.color)
                        RoundedRectangle(cornerRadius: theme.radii.item).stroke(palette.line.color, lineWidth: 1)
                        HStack(spacing: 6) {
                            Text(verbatim: chosen)
                                .font(Font.system(size: 13))
                                .foregroundStyle(palette.ink.color)
                                .lineLimit(1)
                                // Served before the chevron, or an equal share cuts it at half (Fit.swift).
                                .layoutPriority(Fit.priority(windows: 1))
                            Image(systemName: open ? "chevron.up" : "chevron.down")
                                .foregroundStyle(palette.ink2.color)
                        }
                        .padding(.horizontal, theme.spacing.row)
                        .padding(.vertical, 6)
                        .frame(minHeight: 32)
                    }
                }
                .buttonStyle(.plain)
                .accessibilityLabel(title)
                .accessibilityValue(chosen)
                .accessibilityAddTraits(open ? .isSelected : [])
            }
            if open {
                HubChoiceList(
                    options: options.map { HubChoiceOption(value: $0.value, title: $0.title, detail: $0.detail) },
                    selection: pick
                )
                .padding(.horizontal, theme.spacing.md + 2)
                .padding(.bottom, theme.spacing.sm)
            }
        }
    }

    /// Choosing an option sets it and closes the list.
    private var pick: Binding<String> {
        let source = selection
        let flag = $open
        return Binding(
            get: { source.wrappedValue },
            set: { value in
                source.wrappedValue = value
                flag.wrappedValue = false
            }
        )
    }
}

/// Up to four options side by side in a grey track, the chosen one raised (the page's SegmentedControl).
struct SettingsSegmented: View {
    let label: String
    let options: [HubChoice]
    let selection: Binding<String>
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        HubLayered {
            RoundedRectangle(cornerRadius: theme.radii.item).fill(palette.fill2.color)
            HStack(spacing: 2) {
                ForEach(options) { option in
                    segment(option, selected: option.value == selection.wrappedValue)
                }
            }
            .padding(3)
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(label)
    }

    private func segment(_ option: HubChoice, selected: Bool) -> some View {
        Button {
            selection.wrappedValue = option.value
        } label: {
            HubLayered {
                RoundedRectangle(cornerRadius: theme.radii.control)
                    .fill(selected ? palette.elevated.color : Color.clear)
                Text(verbatim: option.title)
                    .font(Font.system(size: 12.5, weight: selected ? .semibold : .medium))
                    .foregroundStyle(selected ? palette.ink.color : palette.ink2.color)
                    .lineLimit(1)
                    .padding(.horizontal, theme.spacing.md + 2)
                    .padding(.vertical, 5)
            }
        }
        .buttonStyle(.plain)
        // Windows serves the longest option first, so an equal share does not cut it ("Right" to "Rig…");
        // the Mac keeps priority 1 (Fit.swift).
        .layoutPriority(Fit.priority(windows: Double(option.title.count), other: 1))
        // Named for the option alone, as the Tauri radio is ("Right", "Always show").
        .accessibilityLabel(option.title)
        .accessibilityAddTraits(selected ? .isSelected : [])
    }
}

/// A square icon button with a hairline outline (`.ck-mini`: the order arrows, the stepper's minus and plus).
struct SettingsMiniButton: View {
    let symbol: String
    let label: String
    var enabled = true
    let action: @MainActor () -> Void
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        Button {
            action()
        } label: {
            HubLayered {
                RoundedRectangle(cornerRadius: theme.radii.item).fill(palette.plane.color)
                RoundedRectangle(cornerRadius: theme.radii.item).stroke(palette.line.color, lineWidth: 1)
                Image(systemName: symbol)
                    .foregroundStyle(palette.ink.color)
                    .frame(width: 32, height: 32)
            }
            .opacity(enabled ? 1 : 0.4)
        }
        .buttonStyle(.plain)
        .disabled(!enabled)
        .accessibilityLabel(label)
        .layoutPriority(2)
    }
}

/// A number the notch publishes between two limits, changed in steps with a minus and a plus
/// (the page's range slider).
struct SettingsStepperRow: View {
    let setting: HubSetting
    let model: SettingsPageModel
    let min: Double
    let max: Double
    let step: Double
    var percent = false
    var title: String?
    var note: String?

    private func format(_ value: Double) -> String {
        percent ? SettingsFormat.percent(value) : "\(SettingsFormat.twoDecimals(value))×"
    }

    private func nudged(_ value: Double, by direction: Double) -> Double {
        let stepped = ((value / step).rounded() + direction) * step
        let clamped = Swift.min(max, Swift.max(min, stepped))
        return (clamped * 100).rounded() / 100
    }

    var body: some View {
        if let value = model.number(setting.key) {
            let name = title ?? setting.title
            let current = model
            let key = setting.key
            let lower = nudged(value, by: -1)
            let higher = nudged(value, by: 1)
            SettingsRowFrame(title: name, note: note) {
                HStack(spacing: 8) {
                    SettingsMiniButton(symbol: "minus", label: "Decrease \(name)", enabled: value > min + 0.0001) {
                        Task { await current.setValue(key, .number(lower)) }
                    }
                    Text(verbatim: format(value))
                        .font(theme.type.caption.font)
                        .frame(width: 48)
                    SettingsMiniButton(symbol: "plus", label: "Increase \(name)", enabled: value < max - 0.0001) {
                        Task { await current.setValue(key, .number(higher)) }
                    }
                }
                .accessibilityElement(children: .contain)
                .accessibilityLabel(name)
                .accessibilityValue(format(value))
            }
        }
    }

    @Environment(\.hubTheme) private var theme
}

/// A number the notch publishes between two limits, set with a slider (the page's range slider),
/// saved when the drag ends or a step key moves it.
struct SettingsSliderRow: View {
    let setting: HubSetting
    let model: SettingsPageModel
    let min: Double
    let max: Double
    let step: Double
    var percent = false
    @State private var draft: Double?
    @Environment(\.hubTheme) private var theme

    private func format(_ value: Double) -> String {
        percent ? SettingsFormat.percent(value) : "\(SettingsFormat.twoDecimals(value))×"
    }

    var body: some View {
        if let published = model.number(setting.key) {
            let current = model
            let key = setting.key
            let shown = draft ?? published
            SettingsRowFrame(title: setting.title, note: nil) {
                HStack(spacing: 8) {
                    Slider(
                        value: Binding(get: { draft ?? published }, set: { draft = $0 }),
                        in: min...max, step: step
                    ) { editing in
                        if !editing, let value = draft {
                            let rounded = (value * 100).rounded() / 100
                            draft = nil
                            if abs(rounded - published) > 0.0001 {
                                Task { await current.setValue(key, .number(rounded)) }
                            }
                        }
                    }
                    .frame(width: 160)
                    .accessibilityLabel(setting.title)
                    .accessibilityValue(format(shown))
                    Text(verbatim: format(shown))
                        .font(theme.type.caption.font)
                        .frame(width: 48)
                        .accessibilityHidden(true)
                }
            }
        }
    }
}

/// A whole-number setting (the clipboard caps), saved on Return or Save; a value out of range is
/// brought back inside it, as the page does.
struct SettingsNumberRow: View {
    let setting: HubSetting
    let model: SettingsPageModel
    let min: Int
    let max: Int
    @State private var draft = ""

    private func commit(_ published: Double) {
        let text = draft.trimmingCharacters(in: .whitespaces)
        guard !text.isEmpty, let parsed = Double(text), parsed.isFinite else {
            draft = SettingsFormat.number(published)
            return
        }
        let next = Swift.min(Double(max), Swift.max(Double(min), parsed.rounded()))
        draft = SettingsFormat.number(next)
        if next != published {
            let current = model
            let key = setting.key
            Task { await current.setValue(key, .number(next)) }
        }
    }

    var body: some View {
        if let published = model.number(setting.key) {
            SettingsRowFrame(title: setting.title, note: SettingsText.note(setting, model: model)) {
                HStack(spacing: 8) {
                    TextField("", text: $draft)
                        .textFieldStyle(.roundedBorder)
                        .frame(width: 84)
                        .accessibilityLabel(setting.title)
                        .onSubmit { commit(published) }
                    SettingsButton("Save", enabled: draft != SettingsFormat.number(published)) { commit(published) }
                }
            }
            .task(id: published) {
                draft = SettingsFormat.number(published)
            }
        }
    }
}

/// A list of strings kept by the notch as an array or as JSON text: each item with Remove, and an
/// Add line (the page's one-per-line text area).
struct SettingsListRow: View {
    let setting: HubSetting
    let model: SettingsPageModel
    var addPlaceholder = "Add an item"

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
                    SettingsAddLine(placeholder: addPlaceholder) { value in
                        let next = items + [value]
                        Task { await current.setValue(key, SettingsListRow.encode(next, asText: asText)) }
                    }
                }
            }
        }
    }

    /// Sent in the shape the notch published (array or JSON text).
    static func encode(_ list: [String], asText: Bool) -> SettingsJSON {
        if asText {
            let data = (try? JSONEncoder().encode(list)) ?? Data("[]".utf8)
            return .string(String(decoding: data, as: UTF8.self))
        }
        return SettingsJSON.strings(list)
    }
}

/// A path or name with a Remove button (`.ck-line` with `.ck-path`).
struct SettingsPathLine: View {
    let text: String
    var removeLabel = "Remove"
    let onRemove: @MainActor () -> Void
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        HStack(spacing: theme.spacing.sm) {
            Text(verbatim: text)
                .font(theme.type.caption.font)
                .foregroundStyle(palette.ink2.color)
                .frame(maxWidth: .infinity, alignment: .leading)
            SettingsButton("Remove", kind: .ghost, label: removeLabel) { onRemove() }
        }
    }
}

/// "Choose…": opens the platform's folder chooser (`HubPlatformServices.chooseFolder`, from the
/// environment) and hands the chosen folder's path to `onChoose`. Cancelling does nothing. When the
/// chooser cannot open (this platform has none, or it failed), `onProblem` gets one sentence to show
/// next to the typed-path field, which stays as the way to enter a path.
struct SettingsChooseFolderButton: View {
    let prompt: String
    /// The button's accessible name when it is not the prompt (Tauri names it "Choose…").
    var name: String?
    /// A folder to open the chooser in; empty or not a folder opens it at its default.
    var startingAt = ""
    let onChoose: @MainActor (String) -> Void
    let onProblem: @MainActor (String) -> Void
    @Environment(\.hubServices) private var services

    var body: some View {
        let service = services
        SettingsButton("Choose…", label: name ?? prompt) {
            Task { @MainActor in
                do {
                    let folder = try await service.chooseFolder(
                        prompt: prompt,
                        startingAt: startingAt.trimmingCharacters(in: .whitespaces).isEmpty ? nil : startingAt
                    )
                    if let folder { onChoose(folder) }
                } catch let failure as HubPlatformError {
                    if case .unsupported = failure {
                        onProblem("There is no folder chooser here. Type the folder's path instead.")
                    } else {
                        onProblem("The folder chooser could not open: \(failure)")
                    }
                } catch {
                    onProblem("The folder chooser could not open: \(error)")
                }
            }
        }
    }
}

/// A line that adds one value to a list (`AddLine`). With `choosePrompt` it also has "Choose…", which
/// adds the chosen folder at once.
struct SettingsAddLine: View {
    let placeholder: String
    var choosePrompt: String?
    let onAdd: @MainActor (String) -> Void
    @State private var text = ""
    @State private var problem: String?
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private func add() {
        let value = text.trimmingCharacters(in: .whitespaces)
        if value.isEmpty { return }
        onAdd(value)
        text = ""
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: theme.spacing.sm) {
                TextField(placeholder, text: $text)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityLabel(placeholder)
                    .onSubmit { add() }
                if let choosePrompt {
                    SettingsChooseFolderButton(
                        prompt: choosePrompt, startingAt: text,
                        onChoose: { folder in
                            problem = nil
                            onAdd(folder)
                        },
                        onProblem: { problem = $0 })
                }
                SettingsButton("Add", enabled: !text.trimmingCharacters(in: .whitespaces).isEmpty) { add() }
            }
            if let problem {
                Text(verbatim: problem)
                    .font(theme.type.caption.font)
                    .foregroundStyle(theme.palette(scheme).bad.color)
            }
        }
    }
}

/// A free-text setting saved on Return or Save (the Device name and save-folder rows).
struct SettingsTextRow: View {
    let setting: HubSetting
    let model: SettingsPageModel
    var placeholder: String
    var note: String?
    /// With a prompt the row also has "Choose…", which picks a folder and saves it at once; the text
    /// field stays for typing a path (and where the platform has no chooser).
    var choosePrompt: String?
    /// With a label the row also has a button that sets the value back to empty while it is not
    /// ("Use Downloads").
    var resetLabel: String?
    @State private var draft = ""
    @State private var problem: String?
    @Environment(\.hubTheme) private var theme

    private func commit(_ stored: String) {
        let trimmed = draft.trimmingCharacters(in: .whitespaces)
        if trimmed == stored.trimmingCharacters(in: .whitespaces) { return }
        let current = model
        let key = setting.key
        Task { await current.setValue(key, .string(trimmed)) }
    }

    var body: some View {
        let stored = model.text(setting.key)
        SettingsRowFrame(title: setting.title, note: problem ?? SettingsText.note(setting, model: model, override: note)) {
            HStack(spacing: theme.spacing.sm) {
                TextField(placeholder, text: $draft)
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 220)
                    .accessibilityLabel(setting.title)
                    .onSubmit { commit(stored) }
                if let choosePrompt {
                    SettingsChooseFolderButton(
                        prompt: choosePrompt, name: "Choose…", startingAt: draft.isEmpty ? stored : draft,
                        onChoose: { folder in
                            problem = nil
                            draft = folder
                            commit(stored)
                        },
                        onProblem: { problem = $0 })
                }
                SettingsButton(
                    "Save", enabled: draft.trimmingCharacters(in: .whitespaces) != stored.trimmingCharacters(in: .whitespaces)
                ) { commit(stored) }
                if let resetLabel, !stored.isEmpty {
                    SettingsButton(resetLabel, kind: .ghost) {
                        let current = model
                        let key = setting.key
                        Task { await current.setValue(key, .string("")) }
                    }
                }
            }
        }
        .task(id: stored) {
            draft = stored
        }
    }
}

/// A row with one button (Reset position, Refresh now, Check now).
struct SettingsButtonRow: View {
    let title: String
    var note: String?
    let button: String
    var disabled = false
    var kind: SettingsButtonKind = .secondary
    let action: @MainActor () -> Void

    var body: some View {
        SettingsRowFrame(title: title, note: note) {
            SettingsButton(button, kind: kind, enabled: !disabled) { action() }
        }
    }
}

/// How a page button looks: the Tauri `Button size="sm"` as "secondary" (a grey fill) or "ghost" (an
/// outline on the page).
enum SettingsButtonKind: Sendable {
    case secondary
    case ghost
}

/// A text button drawn like the Tauri page's small buttons, from the same pieces as the segmented control
/// (a plain Button over filled shapes). `label` and `hint` name it for VoiceOver when its text is not enough
/// ("Remove old@example.test"); `enabled` false dims it and ignores presses.
struct SettingsButton: View {
    let title: String
    var kind: SettingsButtonKind
    var enabled: Bool
    var label: String?
    var hint: String?
    let action: @MainActor () -> Void
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    init(
        _ title: String, kind: SettingsButtonKind = .secondary, enabled: Bool = true, label: String? = nil,
        hint: String? = nil, action: @escaping @MainActor () -> Void
    ) {
        self.title = title
        self.kind = kind
        self.enabled = enabled
        self.label = label
        self.hint = hint
        self.action = action
    }

    private var palette: HubPalette { theme.palette(scheme) }

    var body: some View {
        Button {
            action()
        } label: {
            HubLayered {
                RoundedRectangle(cornerRadius: theme.radii.item)
                    .fill(kind == .secondary ? palette.fill2.color : palette.elevated.color)
                RoundedRectangle(cornerRadius: theme.radii.item)
                    .stroke(kind == .ghost ? palette.line.color : Color.clear, lineWidth: 1)
                Text(verbatim: title)
                    .font(Font.system(size: 13, weight: .medium))
                    .foregroundStyle(palette.ink.color)
                    .lineLimit(1)
                    .padding(.horizontal, theme.spacing.md + 2)
                    .padding(.vertical, 6)
                    .frame(minHeight: 32)
            }
            .opacity(enabled ? 1 : 0.45)
        }
        .buttonStyle(.plain)
        .disabled(!enabled)
        .accessibilityLabel(label ?? title)
        .accessibilityHint(hint ?? "")
        .layoutPriority(2)
    }
}

// MARK: - Shortcuts

/// The notch's shortcut grammar (`windowShortcutFrom` in Settings.tsx): modifiers ctrl, opt, shift, cmd,
/// then a key, joined with "+", for example "ctrl+opt+left". The page recorded a key press; the hub
/// takes the same text, because key capture is not among the controls SwiftRight covers.
enum SettingsShortcut {
    static let namedKeys: Set<String> = [
        "left", "right", "up", "down", "return", "space", "delete", "tab", "-", "=", "[", "]", ",", ".", "/", ";", "'", "\\", "`",
    ]

    /// The normalised shortcut, or nil when it has no modifier or no key. `optionName` is "opt" for the
    /// Settings and window shortcuts and "alt" for the clipboard strip's.
    static func normalize(_ raw: String, optionName: String, allowCommand: Bool) -> String? {
        var mods: Set<String> = []
        var key = ""
        let separators = CharacterSet(charactersIn: "+ ,").union(.whitespaces)
        let parts = raw.lowercased().components(separatedBy: separators).filter { !$0.isEmpty }
        for part in parts {
            switch part {
            case "ctrl", "control", "ctl": mods.insert("ctrl")
            case "opt", "option", "alt": mods.insert("opt")
            case "shift": mods.insert("shift")
            case "cmd", "command": mods.insert("cmd")
            case "enter": key = "return"
            case "backspace": key = "delete"
            default:
                if !key.isEmpty { return nil }
                key = part
            }
        }
        if mods.isEmpty || key.isEmpty { return nil }
        if !allowCommand && mods.contains("cmd") { return nil }
        let single = key.count == 1 && (key.first?.isLetter == true || key.first?.isNumber == true)
        if !single && !namedKeys.contains(key) { return nil }
        var ordered: [String] = []
        for mod in ["ctrl", "opt", "shift", "cmd"] where mods.contains(mod) {
            ordered.append(mod == "opt" ? optionName : mod)
        }
        return (ordered + [key]).joined(separator: "+")
    }

    /// "Ctrl+Shift+V" from "ctrl+shift+v" (the clipboard strip's display, `clipboardHotkeyText`).
    static func clipboardDisplay(_ raw: String, windows: Bool) -> String {
        if raw.isEmpty { return "None" }
        let names: [String: String] = [
            "ctrl": windows ? "Ctrl" : "Control", "alt": windows ? "Alt" : "Option", "shift": "Shift", "cmd": "Command",
        ]
        let parts = raw.split(separator: "+").map { part -> String in
            let text = String(part)
            if let name = names[text] { return name }
            if text.count == 1 { return text.uppercased() }
            return String(text.prefix(1)).uppercased() + String(text.dropFirst())
        }
        return parts.joined(separator: windows ? "+" : " ")
    }

    static func hint(windows: Bool) -> String {
        windows
            ? "Needs at least one of Ctrl, Alt, Shift or the Windows key. Escape cancels."
            : "Needs at least one of Control, Option, Shift or Command. Escape cancels."
    }
}

/// Shows a shortcut with Set, Clear and (while editing) a field for the new one. `editing` is a binding so
/// a row can also open the field itself (turning a shortcut on that has no default).
struct SettingsShortcutControl: View {
    let display: String
    let raw: String
    var optionName = "opt"
    var allowCommand = true
    var canClear = false
    var setLabel = "Set"
    let editing: Binding<Bool>
    let onSet: @MainActor (String) -> Void
    var onClear: (@MainActor () -> Void)?
    @State private var draft = ""
    @State private var problem = false
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    private var palette: HubPalette { theme.palette(scheme) }

    private func save() {
        guard let value = SettingsShortcut.normalize(draft, optionName: optionName, allowCommand: allowCommand) else {
            problem = true
            return
        }
        problem = false
        editing.wrappedValue = false
        onSet(value)
    }

    var body: some View {
        VStack(alignment: .trailing, spacing: 4) {
            HStack(spacing: theme.spacing.sm) {
                if editing.wrappedValue {
                    TextField("ctrl+opt+d", text: $draft)
                        .textFieldStyle(.roundedBorder)
                        .frame(width: 150)
                        .accessibilityLabel("New shortcut")
                        .onSubmit { save() }
                        .task { draft = raw }
                    SettingsButton("Save") { save() }
                    SettingsButton("Cancel", kind: .ghost) {
                        editing.wrappedValue = false
                        problem = false
                    }
                } else {
                    Text(verbatim: display.isEmpty ? "None" : display)
                        .font(theme.type.caption.font)
                        .foregroundStyle(palette.ink2.color)
                        .frame(minWidth: 96, alignment: .trailing)
                    SettingsButton(setLabel, kind: .ghost) {
                        draft = raw
                        problem = false
                        editing.wrappedValue = true
                    }
                    if canClear, !raw.isEmpty, let onClear {
                        SettingsButton("Clear", kind: .ghost) { onClear() }
                    }
                }
            }
            if problem {
                Text(verbatim: "Use modifiers and one key, for example ctrl+opt+left.")
                    .font(theme.type.caption.font)
                    .foregroundStyle(palette.bad.color)
            }
        }
    }
}

/// A shortcut control that keeps its own editing state.
struct SettingsShortcutField: View {
    let display: String
    let raw: String
    var optionName = "opt"
    var allowCommand = true
    var canClear = false
    var setLabel = "Set"
    let onSet: @MainActor (String) -> Void
    var onClear: (@MainActor () -> Void)?
    @State private var editing = false

    var body: some View {
        SettingsShortcutControl(
            display: display, raw: raw, optionName: optionName, allowCommand: allowCommand,
            canClear: canClear, setLabel: setLabel, editing: $editing, onSet: onSet, onClear: onClear)
    }
}

/// A switch whose value lives outside the notch's `settings` (the sharing service, a list kept in one
/// setting): the caller says what is on and what to do on change.
struct SettingsValueToggleRow: View {
    let title: String
    let note: String
    let isOn: Bool
    var disabled = false
    /// The switch's accessible name when it differs from the title ("Snip tool" under "Snip").
    var switchName: String?
    let onChange: @MainActor (Bool) -> Void

    private var binding: Binding<Bool> {
        let on = isOn
        let change = onChange
        return Binding(
            get: { on },
            set: { value in
                Task { @MainActor in change(value) }
            }
        )
    }

    var body: some View {
        SettingsRowFrame(title: title, note: note) {
            Toggle("", isOn: binding)
                .labelsHidden()
                .toggleStyle(.switch)
                .disabled(disabled)
                .accessibilityLabel(switchName ?? title)
        }
    }
}
#endif

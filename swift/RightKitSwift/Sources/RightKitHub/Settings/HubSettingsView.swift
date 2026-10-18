import SwiftUI

/// Renders settings groups as cards of rows: a Toggle, a TextField or a `HubChoiceList` per setting,
/// each with its documented default in the description. Values go through a `HubSettingsStore`.
///
/// Built only from controls SwiftRight covers (no Form, Section or Picker).
public struct HubSettingsView: View {
    private let groups: [HubSettingsGroup]
    private let query: String
    @State private var model: HubSettingsModel
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    /// - Parameters:
    ///   - groups: What to show, usually a section's `settings`.
    ///   - store: Where the values live.
    ///   - query: Narrows the groups with `HubSettingsSearch`. Empty shows everything.
    public init(groups: [HubSettingsGroup], store: any HubSettingsStore, query: String = "") {
        self.groups = groups
        self.query = query
        _model = State(initialValue: HubSettingsModel(store: store, groups: groups))
    }

    private var palette: HubPalette { theme.palette(scheme) }

    private var visible: [HubSettingsGroup] {
        HubSettingsSearch.filter(groups, query: query)
    }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: theme.spacing.xl) {
                if let message = model.lastError {
                    Text(verbatim: message)
                        .font(theme.type.caption.font)
                        .foregroundStyle(palette.bad.color)
                }
                ForEach(visible) { group in
                    groupView(group)
                }
                if visible.isEmpty {
                    HubEmptyState(
                        symbol: "magnifyingglass",
                        title: "No settings match",
                        message: "Try a different word."
                    )
                }
            }
            .padding(theme.spacing.xl)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .task {
            await model.start()
        }
    }

    private func groupView(_ group: HubSettingsGroup) -> some View {
        VStack(alignment: .leading, spacing: theme.spacing.sm) {
            Text(verbatim: group.title)
                .font(theme.type.label.font)
                .foregroundStyle(palette.ink2.color)
            if let detail = group.detail {
                Text(verbatim: detail)
                    .font(theme.type.caption.font)
                    .foregroundStyle(palette.ink3.color)
            }
            HubCard(padding: 0) {
                ForEach(group.settings) { setting in
                    settingRow(setting)
                    if setting.key != group.settings.last?.key {
                        Divider()
                    }
                }
            }
        }
    }

    @ViewBuilder
    private func settingRow(_ setting: HubSetting) -> some View {
        switch setting.kind {
        case .toggle:
            HubRow(title: setting.title, detail: describe(setting)) {
                Toggle("", isOn: toggleBinding(setting))
                    .labelsHidden()
                    .toggleStyle(.switch)
                    .accessibilityLabel(setting.title)
            }
        case .choice(let choices):
            VStack(alignment: .leading, spacing: theme.spacing.xs) {
                Text(verbatim: setting.title)
                    .font(theme.type.body.font)
                    .foregroundStyle(palette.ink.color)
                Text(verbatim: describe(setting))
                    .font(theme.type.caption.font)
                    .foregroundStyle(palette.ink3.color)
                HubChoiceList(
                    options: choices.map { HubChoiceOption(value: $0.value, title: $0.title, detail: $0.detail) },
                    selection: textBinding(setting, commitsOnChange: true)
                )
            }
            .padding(.horizontal, theme.spacing.md)
            .padding(.vertical, theme.spacing.row)
            .frame(maxWidth: .infinity, alignment: .leading)
        case .text(let placeholder):
            VStack(alignment: .leading, spacing: theme.spacing.xs) {
                Text(verbatim: setting.title)
                    .font(theme.type.body.font)
                    .foregroundStyle(palette.ink.color)
                Text(verbatim: describe(setting))
                    .font(theme.type.caption.font)
                    .foregroundStyle(palette.ink3.color)
                TextField(placeholder, text: textBinding(setting, commitsOnChange: false))
                    .hubRoundedField()
                    .onSubmit {
                        let current = self.model
                        Task { await current.commit(setting) }
                    }
            }
            .padding(.horizontal, theme.spacing.md)
            .padding(.vertical, theme.spacing.row)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    /// The description with the documented default.
    private func describe(_ setting: HubSetting) -> String {
        "\(setting.detail) Default: \(setting.defaultDescription)."
    }

    private func toggleBinding(_ setting: HubSetting) -> Binding<Bool> {
        let current = self.model
        return Binding(
            get: { MainActor.assumeIsolated { current.isOn(setting) } },
            set: { on in
                Task { await current.set(setting, .bool(on)) }
            }
        )
    }

    /// A string binding. A choice stores on every change; text stores on submit.
    private func textBinding(_ setting: HubSetting, commitsOnChange: Bool) -> Binding<String> {
        let current = self.model
        return Binding(
            get: { MainActor.assumeIsolated { current.text(for: setting) } },
            set: { text in
                if commitsOnChange {
                    Task { await current.set(setting, .string(text)) }
                } else {
                    MainActor.assumeIsolated { current.edit(setting, text: text) }
                }
            }
        )
    }
}

private extension View {
    /// `.roundedBorder` does not exist on watchOS; the system field style is used there.
    @ViewBuilder func hubRoundedField() -> some View {
        #if os(watchOS)
        self
        #else
        textFieldStyle(.roundedBorder)
        #endif
    }
}

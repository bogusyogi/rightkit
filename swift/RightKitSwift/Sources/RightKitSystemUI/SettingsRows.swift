#if os(macOS)
import RightKitHub
import SwiftUI

/// Makes the row of a setting from the notch's published state. Each method returns nil when the
/// notch does not publish the key (the page's `has(key)` rule), so a Windows notch, which publishes
/// only part of the Mac's settings, shows only those rows. Each row carries the text its page's
/// search matches: the name and the note.
@MainActor
struct SettingsRows {
    let model: SettingsPageModel

    /// A switch. `always` shows it although the notch does not publish its key (the Conveniences group's
    /// own `toggle` in the Tauri page does not check); `switchName` is the switch's accessible name when
    /// it is not the title.
    func toggle(
        _ key: String, title: String? = nil, note: String? = nil, always: Bool = false, switchName: String? = nil
    ) -> SettingsRowSpec? {
        guard always || model.has(key), var setting = model.setting(key) else { return nil }
        if let title { setting.title = title }
        return SettingsRowSpec(
            id: key, text: SettingsText.searchText(setting, model: model, override: note),
            view: SettingsToggleRow(setting: setting, model: model, note: note, switchName: switchName))
    }

    func choice(_ key: String, segmented: Bool = true, note: String? = nil) -> SettingsRowSpec? {
        guard !model.options(key).isEmpty, let setting = model.setting(key) else { return nil }
        return SettingsRowSpec(
            id: key, text: SettingsText.searchText(setting, model: model, override: note),
            view: SettingsChoiceRow(setting: setting, model: model, segmented: segmented, note: note))
    }

    func stepper(_ key: String, min: Double, max: Double, step: Double, percent: Bool = false, title: String? = nil) -> SettingsRowSpec? {
        guard model.number(key) != nil, let setting = model.setting(key) else { return nil }
        return SettingsRowSpec(
            id: key, text: title ?? setting.title,
            view: SettingsStepperRow(
                setting: setting, model: model, min: min, max: max, step: step, percent: percent,
                title: title, note: nil))
    }

    func slider(_ key: String, min: Double, max: Double, step: Double, percent: Bool = false) -> SettingsRowSpec? {
        guard model.number(key) != nil, let setting = model.setting(key) else { return nil }
        return SettingsRowSpec(
            id: key, text: SettingsText.searchText(setting, model: model),
            view: SettingsSliderRow(setting: setting, model: model, min: min, max: max, step: step, percent: percent))
    }

    func number(_ key: String, min: Int, max: Int) -> SettingsRowSpec? {
        guard model.number(key) != nil, let setting = model.setting(key) else { return nil }
        return SettingsRowSpec(
            id: key, text: SettingsText.searchText(setting, model: model),
            view: SettingsNumberRow(setting: setting, model: model, min: min, max: max))
    }

    func list(_ key: String, placeholder: String) -> SettingsRowSpec? {
        guard model.has(key), let setting = model.setting(key) else { return nil }
        return SettingsRowSpec(
            id: key, text: SettingsText.searchText(setting, model: model),
            view: SettingsListRow(setting: setting, model: model, addPlaceholder: placeholder))
    }
}
#endif

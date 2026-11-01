#if os(macOS)
import RightKitHub
import SwiftUI

/// The five settings sections of the Tauri hub (`hub/src/views/Settings.tsx`): Permissions, Accounts,
/// Appearance, Notifications, General. Each is a page of grouped rows over the notch state (`notch.state()`),
/// with changes sent as the same `notch_command` objects the page sent and re-read through
/// `notch.stateUpdates()`. Every setting is also registered with the kit (`HubSettingsGroup`), so the hub's
/// global search finds it, and each page has its own "Search settings" box that filters the page's rows.
enum SettingsSections {
    static func all(notch: any NotchBridge) -> [any HubSection] {
        SettingsPageKind.allCases.map { SettingsSection(kind: $0, notch: notch) }
    }
}

struct SettingsSection: PulseSection {
    let kind: SettingsPageKind
    let notch: any NotchBridge

    var id: String { kind.rawValue }
    var title: String { kind.title }
    var symbol: String { kind.symbol }
    var order: Int { kind.order }
    /// The sidebar heading over these five pages (the Tauri sidebar's "Settings" group).
    var group: String? { "Settings" }

    var settings: [HubSettingsGroup] {
        SettingsCatalog.groups(kind, SettingsCatalogContext())
    }

    var searchEntries: [HubSearchEntry] {
        SettingsCatalog.entries(kind)
    }

    @MainActor func makeView(_ ctx: HubContext) -> AnyView {
        let model = SettingsPageModel(kind: kind, notch: notch, engine: ctx.engine, live: true)
        return AnyView(SettingsPageRoot(model: model, live: true))
    }

    @MainActor func preparedView(_ ctx: HubContext) async -> AnyView {
        let model = SettingsPageModel(kind: kind, notch: notch, engine: ctx.engine, live: false)
        await model.prepare()
        return AnyView(SettingsPageRoot(model: model, live: false))
    }
}
#endif

#if os(macOS)
import RightKitHub
import SwiftUI

/// One Settings page on screen: a scrolling column of groups. `live` pages keep themselves current
/// while shown; a prepared page (the off-screen renderer) shows what was loaded and does nothing more.
struct SettingsPageRoot: View {
    let live: Bool
    @State private var model: SettingsPageModel

    init(model: SettingsPageModel, live: Bool) {
        self.live = live
        _model = State(initialValue: model)
    }

    var body: some View {
        // The search box stays at the top while the page scrolls under it (the Tauri page's sticky
        // `.ck-search`).
        VStack(alignment: .leading, spacing: 0) {
            SettingsSearchBar(model: model)
            SettingsPageFrame {
                if let message = model.commandError {
                    SettingsStatusLine(text: message, tone: .bad)
                }
                content
            }
        }
        .task {
            if live { await model.run() }
        }
    }

    @ViewBuilder
    private var content: some View {
        if model.kind == .permissions && model.isWindows {
            // Windows reads its Permissions rows from the system; no notch state is needed for them.
            SettingsPermissionsPage(model: model)
        } else if model.state == nil {
            SettingsStatusLine(text: "Reading the notch's settings…")
        } else {
            switch model.kind {
            case .permissions: SettingsPermissionsPage(model: model)
            case .accounts: SettingsAccountsPage(model: model)
            case .appearance: SettingsAppearancePage(model: model)
            case .notifications: SettingsNotificationsPage(model: model)
            case .general: SettingsGeneralPage(model: model)
            }
        }
    }
}
#endif

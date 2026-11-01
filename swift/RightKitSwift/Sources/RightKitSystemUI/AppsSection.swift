#if os(macOS)
import RightKitHub
import SwiftUI

/// Apps: port of `hub/src/views/Apps.tsx`. The installed apps largest first with their last use and
/// updates, each app's leftovers and the uninstall, and the login items that start by themselves.
/// The files are in `Sections/Apps/`.
struct AppsSection: PulseSection {
    let id = "apps"
    let title = "Apps"
    let symbol = "square.grid.3x3"
    let order = 50
    var searchEntries: [HubSearchEntry] {
        [
            HubSearchEntry(
                id: "apps",
                title: "Apps",
                detail: "Installed apps, uninstall and leftovers",
                keywords: ["uninstall", "applications", "leftovers", "updates", "unused", "starts by itself", "login items"]
            )
        ]
    }

    @MainActor func makeView(_ ctx: HubContext) -> AnyView {
        AnyView(AppsRootView(model: AppsModel(engine: ctx.engine), loadsOnAppear: true))
    }

    @MainActor func preparedView(_ ctx: HubContext) async -> AnyView {
        let model = AppsModel(engine: ctx.engine)
        await model.load(live: false)
        // Decode the icons now: the picture is taken once, with no time for each row's own decode.
        for icon in model.icons.values { await icon.preload(services: ctx.services) }
        return AnyView(AppsRootView(model: model, loadsOnAppear: false))
    }
}
#endif

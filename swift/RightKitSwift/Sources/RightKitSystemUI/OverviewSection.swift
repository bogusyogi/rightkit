#if os(macOS)
import RightKitHub
import SwiftUI

/// Overview: port of `hub/src/views/Overview.tsx`. A status line and six cards (Performance,
/// Network and swap, AI usage, Storage, Cleanup, Apps); each card header opens its section through
/// the shell's `hubNavigate` action. Notch state (accounts, permissions, network) comes through the
/// `notch` bridge; the rest is saved or live engine reads.
struct OverviewSection: PulseSection {
    let notch: any NotchBridge

    let id = "overview"
    let title = "Overview"
    let symbol = "square.grid.2x2"
    let order = 0
    var searchEntries: [HubSearchEntry] {
        [HubSearchEntry(id: "overview", title: "Overview", keywords: ["summary", "home", "dashboard"])]
    }

    @MainActor func makeView(_ ctx: HubContext) -> AnyView {
        AnyView(OverviewView(model: OverviewModel(engine: ctx.engine, notch: notch), initialLoad: true))
    }

    /// The same view with its data already read, for the off-screen renderer.
    @MainActor func preparedView(_ ctx: HubContext) async -> AnyView {
        let model = OverviewModel(engine: ctx.engine, notch: notch)
        await model.load()
        return AnyView(OverviewView(model: model, initialLoad: false))
    }
}
#endif

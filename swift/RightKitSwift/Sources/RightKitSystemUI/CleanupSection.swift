#if os(macOS)
import RightKitHub
import SwiftUI

/// Cleanup: port of `hub/src/views/Cleanup.tsx`. Findings from the rule scan, review and select,
/// move to the Trash with a confirmation, and the history with Restore. The files are in
/// `Sections/Cleanup/`. `notch` supplies `hubAutoScan` ("Scan when Storage or Cleanup opens").
struct CleanupSection: PulseSection {
    let notch: any NotchBridge

    let id = "cleanup"
    let title = "Cleanup"
    let symbol = "sparkles"
    let order = 30
    var searchEntries: [HubSearchEntry] {
        [
            HubSearchEntry(
                id: "cleanup",
                title: "Cleanup",
                detail: "Findings, move to the Trash, history and restore",
                keywords: ["trash", "cache", "clean", "restore", "history", "free space"]
            )
        ]
    }

    @MainActor func makeView(_ ctx: HubContext) -> AnyView {
        AnyView(CleanupRootView(model: CleanupModel(engine: ctx.engine, notch: notch), loadsOnAppear: true))
    }

    @MainActor func preparedView(_ ctx: HubContext) async -> AnyView {
        let model = CleanupModel(engine: ctx.engine, notch: notch)
        await model.load(scan: false)
        return AnyView(CleanupRootView(model: model, loadsOnAppear: false))
    }
}
#endif

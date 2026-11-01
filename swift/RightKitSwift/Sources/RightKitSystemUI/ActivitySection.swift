#if os(macOS)
import RightKitHub
import SwiftUI

/// Activity: port of `hub/src/views/Activity.tsx`. What Pulse did and saw, newest first, read only.
///
/// The model lives in `ActivityModelHolder`, so rows already loaded and the filter survive the
/// shell rebuilding the view (the shell calls `makeView` from its body).
struct ActivitySection: PulseSection {
    let id = "activity"
    let title = "Activity"
    let symbol = "clock.arrow.circlepath"
    let order = 10
    var searchEntries: [HubSearchEntry] {
        [HubSearchEntry(id: "activity", title: "Activity", keywords: ["timeline", "history", "log", "recent"])]
    }

    private let holder = ActivityModelHolder()

    @MainActor func makeView(_ ctx: HubContext) -> AnyView {
        AnyView(ActivityView(model: holder.model(ctx), loadsOnAppear: true))
    }

    @MainActor func preparedView(_ ctx: HubContext) async -> AnyView {
        let model = holder.model(ctx)
        await model.load()
        return AnyView(ActivityView(model: model, loadsOnAppear: false))
    }
}

/// Owns the section's one `ActivityModel`, created on first use.
@MainActor
final class ActivityModelHolder {
    private var created: ActivityModel?

    nonisolated init() {}

    func model(_ ctx: HubContext) -> ActivityModel {
        if let created { return created }
        let model = ActivityModel(engine: ctx.engine)
        created = model
        return model
    }
}
#endif

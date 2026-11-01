#if os(macOS)
import RightKitHub
import SwiftUI

/// Membrane: port of `hub/src/views/Membrane.tsx`. Status with Restart Membrane (confirmed), graph
/// freshness, and memory, context and logs read on request. The model and view live in `Membrane/`.
struct MembraneSection: PulseSection {
    let id = "membrane"
    let title = "Membrane"
    let symbol = "brain.head.profile"
    let order = 45
    var searchEntries: [HubSearchEntry] {
        [
            HubSearchEntry(
                id: "membrane",
                title: "Membrane",
                keywords: ["memory", "context", "freshness", "graph", "health", "logs", "repository"]
            )
        ]
    }

    @MainActor func makeView(_ ctx: HubContext) -> AnyView {
        AnyView(MembraneView(model: MembraneModel(engine: ctx.engine), initialLoad: true))
    }

    /// The same view with its status and freshness already read, for the off-screen renderer.
    @MainActor func preparedView(_ ctx: HubContext) async -> AnyView {
        let model = MembraneModel(engine: ctx.engine)
        await model.load()
        return AnyView(MembraneView(model: model, initialLoad: false))
    }
}
#endif

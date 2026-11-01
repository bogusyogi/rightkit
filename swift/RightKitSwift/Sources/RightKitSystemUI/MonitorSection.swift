#if os(macOS)
import RightKitHub
import SwiftUI

/// Monitor: port of `hub/src/views/Monitor.tsx`. Memory pressure, 30 minutes of CPU, memory, swap
/// and network history as bar charts, the Windows sensors, and the process table with Quit, Force
/// Quit (confirmed) and Inspect. `notch` supplies the notch's System readings and carries the
/// `watchSensors` command the Windows notch needs while the page is open.
struct MonitorSection: PulseSection {
    let notch: any NotchBridge

    let id = "monitor"
    let title = "Monitor"
    let symbol = "gauge.with.dots.needle.33percent"
    let order = 40
    var searchEntries: [HubSearchEntry] {
        [HubSearchEntry(id: "monitor", title: "Monitor", keywords: ["cpu", "memory"])]
    }

    @MainActor func makeView(_ ctx: HubContext) -> AnyView {
        AnyView(MonitorView(model: MonitorModel(engine: ctx.engine, notch: notch), initialLoad: true))
    }

    /// The same view with its data already read, for the off-screen renderer.
    @MainActor func preparedView(_ ctx: HubContext) async -> AnyView {
        let model = MonitorModel(engine: ctx.engine, notch: notch)
        await model.load()
        return AnyView(MonitorView(model: model, initialLoad: false))
    }
}
#endif

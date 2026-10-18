import Foundation
import RightKitHub
import SwiftUI

/// Fixtures and wiring for QA runs: a context over scripted services, and sections that exercise
/// every component and the settings kit. Release builds never link this target.
@MainActor
public enum HubQA {
    /// The environment variable that picks the section selected at launch.
    public static let sectionEnvironmentKey = "RIGHTKIT_HUB_QA_SECTION"

    /// A context over a scripted engine, recording platform services and a fresh toast sink.
    public static func context(
        engine: any EngineClient = QAEngineClient(),
        services: any HubPlatformServices = RecordingPlatformServices()
    ) -> HubContext {
        HubContext(engine: engine, services: services, theme: .standard, toasts: ToastSink())
    }

    /// The gallery and settings sections.
    public static func sections() -> [any HubSection] {
        [HubQAGallerySection(), HubQASettingsSection()]
    }

    /// The section id named by `RIGHTKIT_HUB_QA_SECTION`, or nil.
    public static func initialSelection(
        environment: [String: String] = ProcessInfo.processInfo.environment
    ) -> String? {
        environment[sectionEnvironmentKey]
    }

    /// A deterministic series with gaps, for charts: a slow wave over 120 slots, every 17th a gap.
    public static func sampleSeries(capacity: Int = 120, count: Int = 90) -> HubSeries {
        var series = HubSeries(capacity: capacity)
        let start = Date(timeIntervalSince1970: 1_700_000_000)
        for index in 0..<count {
            let value: Double? = index % 17 == 16 ? nil : 50 + 40 * sin(Double(index) / 8)
            series.append(value, at: start.addingTimeInterval(Double(index)))
        }
        return series
    }
}

/// A section showing every component.
public struct HubQAGallerySection: HubSection {
    public let id = "qa-gallery"
    public let title = "Components"
    public let symbol = "square.grid.2x2"
    public let order = 1000

    public init() {}

    public var searchEntries: [HubSearchEntry] {
        [
            HubSearchEntry(id: "chart", title: "CPU chart", detail: "Bar chart with gaps", keywords: ["monitor", "graph"]),
            HubSearchEntry(id: "progress", title: "Disk usage", detail: "Progress bar", keywords: ["storage"]),
        ]
    }

    @MainActor
    public func makeView(_ ctx: HubContext) -> AnyView {
        AnyView(HubQAGalleryView(context: ctx))
    }
}

/// A section showing the settings kit over an in-memory store.
public struct HubQASettingsSection: HubSection {
    public let id = "qa-settings"
    public let title = "Settings"
    public let symbol = "gearshape"
    public let order = 1010

    private let store = InMemorySettingsStore()

    public init() {}

    public var settings: [HubSettingsGroup] {
        [
            HubSettingsGroup(
                id: "general",
                title: "General",
                settings: [
                    .toggle(
                        key: "qa.launchAtLogin",
                        title: "Open at login",
                        detail: "Starts the app when you sign in.",
                        defaultValue: false,
                        keywords: ["startup", "autostart"]
                    ),
                    .choice(
                        key: "qa.density",
                        title: "Density",
                        detail: "How tightly rows are packed.",
                        defaultValue: "comfortable",
                        choices: [
                            HubChoice(value: "comfortable", title: "Comfortable"),
                            HubChoice(value: "compact", title: "Compact"),
                        ]
                    ),
                    .text(
                        key: "qa.name",
                        title: "Device name",
                        detail: "How this computer appears to others.",
                        defaultValue: "",
                        placeholder: "Name"
                    ),
                ]
            ),
        ]
    }

    @MainActor
    public func makeView(_ ctx: HubContext) -> AnyView {
        AnyView(HubSettingsView(groups: settings, store: store))
    }
}

struct HubQAGalleryView: View {
    let context: HubContext
    @State private var series = HubQA.sampleSeries()
    @State private var density = "comfortable"
    @State private var confirming = false
    @Environment(\.hubTheme) private var theme

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: theme.spacing.xl) {
                HStack(spacing: theme.spacing.md) {
                    HubStatTile(title: "Memory", value: "8.2 GB", detail: "of 16 GB")
                    HubStatTile(title: "Disk", value: "412 GB", detail: "free", tone: .ok)
                    HubStatTile(title: "Reclaimable", value: "Unknown", detail: "not measured", tone: .warn)
                }
                HubCard {
                    VStack(alignment: .leading, spacing: theme.spacing.md) {
                        HubBarChart(
                            series: series,
                            range: 0...100,
                            label: "CPU",
                            format: { String(format: "%.0f%%", $0) },
                            width: 320
                        )
                        HubProgressBar(value: 0.64)
                    }
                }
                HubCard(padding: 0) {
                    HubRow(symbol: "internaldrive", title: "Macintosh HD", detail: "APFS, 412 GB free") {
                        HubValueTrailing("64%")
                    }
                    Divider()
                    HubRow(symbol: "externaldrive", title: "Backup", detail: "Not mounted")
                }
                HubCard {
                    HubChoiceList(
                        options: [
                            HubChoiceOption(value: "comfortable", title: "Comfortable"),
                            HubChoiceOption(value: "compact", title: "Compact", detail: "Smaller rows"),
                        ],
                        selection: $density
                    )
                }
                HStack(spacing: theme.spacing.md) {
                    Button("Add sample") {
                        series.append(Double.random(in: 0...100))
                    }
                    Button("Post toast") {
                        context.toasts.show("Saved", kind: .success, detail: "qa.toast")
                    }
                    Button("Delete") {
                        confirming = true
                    }
                }
            }
            .padding(theme.spacing.xl)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .hubConfirm(
            "Delete this item?",
            message: "It moves to the Trash.",
            confirmLabel: "Delete",
            isPresented: $confirming
        ) {
            context.toasts.show("Moved to the Trash", kind: .info)
        }
    }
}

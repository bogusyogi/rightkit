import SwiftUI

/// One entry a section adds to the hub's global search, beyond its settings.
public struct HubSearchEntry: Sendable, Equatable, Hashable, Identifiable {
    /// Stable within its section.
    public let id: String
    public var title: String
    /// A short line shown under the title in results.
    public var detail: String
    /// Extra words that find this entry.
    public var keywords: [String]

    public init(id: String, title: String, detail: String = "", keywords: [String] = []) {
        self.id = id
        self.title = title
        self.detail = detail
        self.keywords = keywords
    }
}

/// One section of the hub: a sidebar row and the view it shows. Apps register their sections with
/// `HubShell`; the shell never knows a section's content.
///
/// All members are get-only so a section can be a plain struct. Only `makeView` runs on the main actor.
public protocol HubSection: Sendable {
    /// Stable id; this is what `HubShell`'s `selection` holds.
    var id: String { get }
    /// The sidebar title and the content header.
    var title: String { get }
    /// An SF Symbol name for the sidebar row.
    var symbol: String { get }
    /// Sort key in the sidebar; lower comes first. Ties sort by title.
    var order: Int { get }
    /// The name of the sidebar group this section belongs to, or nil for none. The sidebar shows the
    /// name as a small heading before the first section of each run of that group, and puts a gap
    /// wherever the group changes. Sections are still ordered by `order` alone, so keep a group's
    /// sections next to each other in `order`.
    var group: String? { get }
    /// Builds the section's view.
    @MainActor func makeView(_ ctx: HubContext) -> AnyView
    /// The section's settings, grouped. They feed global search and `HubSettingsView`.
    var settings: [HubSettingsGroup] { get }
    /// Extra entries for global search.
    var searchEntries: [HubSearchEntry] { get }
}

public extension HubSection {
    /// A section in no named group.
    var group: String? { nil }
    /// A section with no settings of its own.
    var settings: [HubSettingsGroup] { [] }
    /// A section with no extra search entries.
    var searchEntries: [HubSearchEntry] { [] }
}

/// What a section's view is given. The same context is passed to every section.
public struct HubContext: Sendable {
    /// The engine every command goes through.
    public let engine: any EngineClient
    /// Everything the hub needs from the operating system.
    public let services: any HubPlatformServices
    /// The design tokens.
    public let theme: HubTheme
    /// Where a section posts toasts.
    public let toasts: ToastSink

    public init(
        engine: any EngineClient,
        services: any HubPlatformServices,
        theme: HubTheme = .standard,
        toasts: ToastSink
    ) {
        self.engine = engine
        self.services = services
        self.theme = theme
        self.toasts = toasts
    }
}

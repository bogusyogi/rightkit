import SwiftUI

/// The hub frame: a sidebar of sections on the left, and on the right a header above the selected
/// section's view. It also draws the empty, loading and error states, global search and toasts.
///
/// The window is drawn on the theme's chrome colour: the sidebar and the header sit on it, and the
/// content is a panel (the theme's plane colour) with a rounded top-left corner, so the selected
/// sidebar row reads as joined to the content.
///
/// The hub is an HStack (not NavigationSplitView). The app owns `selection`; the notch drives it.
/// Sections show only while the engine is `ready` or `recovering`; `booting` shows a loading state and
/// `down` an error state.
public struct HubShell<Trailing: View, Footer: View>: View {
    private let sections: [any HubSection]
    private let selection: Binding<String?>
    private let context: HubContext
    private let trailing: (String?) -> Trailing
    private let footer: Footer
    private let hasFooter: Bool
    @State private var query = ""
    @State private var lifecycle: EngineLifecycle
    @Environment(\.colorScheme) private var scheme

    /// - Parameters:
    ///   - sections: The hub's sections; the sidebar sorts them by `order`, then title.
    ///   - selection: The selected section id, or nil for none.
    ///   - context: What every section's view is given.
    ///   - initialLifecycle: The engine state to show until the engine's lifecycle stream reports
    ///     one. A host that has already seen the engine ready passes `.ready`, so the pane does not
    ///     flash "Starting the engine".
    ///   - trailing: Actions for the header, given the selected section id.
    ///   - footer: A view pinned at the bottom of the sidebar, under a hairline.
    public init(
        sections: [any HubSection],
        selection: Binding<String?>,
        context: HubContext,
        initialLifecycle: EngineLifecycle = .booting,
        @ViewBuilder trailing: @escaping (String?) -> Trailing,
        @ViewBuilder footer: () -> Footer
    ) {
        _lifecycle = State(initialValue: initialLifecycle)
        self.sections = sections
        self.selection = selection
        self.context = context
        self.trailing = trailing
        self.footer = footer()
        self.hasFooter = true
    }

    fileprivate init(
        sections: [any HubSection],
        selection: Binding<String?>,
        context: HubContext,
        initialLifecycle: EngineLifecycle,
        trailing: @escaping (String?) -> Trailing,
        noFooter: Footer
    ) {
        _lifecycle = State(initialValue: initialLifecycle)
        self.sections = sections
        self.selection = selection
        self.context = context
        self.trailing = trailing
        self.footer = noFooter
        self.hasFooter = false
    }

    private var theme: HubTheme { context.theme }
    private var palette: HubPalette { theme.palette(scheme) }

    private var ordered: [any HubSection] {
        sections.sorted { ($0.order, $0.title) < ($1.order, $1.title) }
    }

    private var current: (any HubSection)? {
        guard let id = selection.wrappedValue else { return nil }
        return sections.first { $0.id == id }
    }

    private var isSearching: Bool {
        !HubSettingsSearch.terms(query).isEmpty
    }

    private var headerTitle: String {
        isSearching ? "Search" : (current?.title ?? "")
    }

    public var body: some View {
        HubLayered {
            Rectangle().fill(palette.chrome.color)
            HStack(spacing: 0) {
                sidebar
                    .frame(width: theme.layout.sidebarWidth)
                pane
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .overlay {
            HubToastLayer(sink: context.toasts)
        }
        .environment(\.hubTheme, theme)
        .environment(\.hubServices, context.services)
        .environment(\.hubNavigate, HubNavigateAction { [selection] id in selection.wrappedValue = id })
        .task {
            for await state in context.engine.lifecycle {
                lifecycle = state
            }
        }
    }

    private var sidebarItems: [HubSidebarItem] {
        ordered.map { HubSidebarItem(id: $0.id, title: $0.title, symbol: $0.symbol, group: $0.group) }
    }

    @ViewBuilder
    private var sidebar: some View {
        if hasFooter {
            HubSidebar(items: sidebarItems, selection: selection, query: $query) { footer }
        } else {
            HubSidebar(items: sidebarItems, selection: selection, query: $query)
        }
    }

    /// The header on the chrome, then the content panel: the plane colour with a rounded top-left
    /// corner. The corner is a chrome square with a plane disc over it, behind the content, so the
    /// content itself is never clipped (and no `.clipShape` is needed).
    private var pane: some View {
        let radius = theme.radii.plane
        return VStack(spacing: 0) {
            HubHeader(title: headerTitle) {
                trailing(selection.wrappedValue)
            }
            HubLayered {
                Rectangle().fill(palette.plane.color)
                ZStack(alignment: .topLeading) {
                    Rectangle().fill(palette.chrome.color).frame(width: radius, height: radius)
                    RoundedRectangle(cornerRadius: radius).fill(palette.plane.color)
                        .frame(width: 2 * radius, height: 2 * radius)
                }
                .frame(width: 2 * radius, height: 2 * radius, alignment: .topLeading)
                content
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    @ViewBuilder
    private var content: some View {
        if isSearching {
            HubSearchResults(results: HubSearchIndex(sections: ordered).search(query)) { result in
                selection.wrappedValue = result.sectionID
                query = ""
            }
        } else {
            switch lifecycle {
            case .booting:
                HubLoadingState(message: "Starting the engine")
            case .down:
                HubErrorState(
                    title: "The engine is not running",
                    message: "The hub can't reach its engine. Quit and reopen the app."
                )
            case .ready, .recovering:
                VStack(spacing: 0) {
                    if lifecycle == .recovering {
                        HubBanner("The engine stopped and is restarting.")
                    }
                    sectionContent
                }
            }
        }
    }

    @ViewBuilder
    private var sectionContent: some View {
        if let section = current {
            // One identity per section: two sections that return the same view type would
            // otherwise keep the previous section's view, its @State model and its tasks.
            section.makeView(context).id(section.id)
        } else if sections.isEmpty {
            HubEmptyState(symbol: "square.dashed", title: "No sections")
        } else {
            HubEmptyState(symbol: "sidebar.left", title: "Choose a section")
        }
    }
}

public extension HubShell where Footer == HubNoTrailing {
    /// A hub with header actions and no sidebar footer.
    init(
        sections: [any HubSection],
        selection: Binding<String?>,
        context: HubContext,
        initialLifecycle: EngineLifecycle = .booting,
        @ViewBuilder trailing: @escaping (String?) -> Trailing
    ) {
        self.init(
            sections: sections, selection: selection, context: context, initialLifecycle: initialLifecycle,
            trailing: trailing, noFooter: HubNoTrailing())
    }
}

public extension HubShell where Trailing == HubNoTrailing, Footer == HubNoTrailing {
    /// A hub with no header actions and no sidebar footer.
    init(
        sections: [any HubSection],
        selection: Binding<String?>,
        context: HubContext,
        initialLifecycle: EngineLifecycle = .booting
    ) {
        self.init(
            sections: sections, selection: selection, context: context, initialLifecycle: initialLifecycle,
            trailing: { _ in HubNoTrailing() }, noFooter: HubNoTrailing())
    }
}

public extension HubShell where Trailing == HubNoTrailing {
    /// A hub with no header actions and a sidebar footer. The footer is passed as a view value
    /// (not a closure), so a trailing closure is never ambiguous with the header-actions form.
    init(
        sections: [any HubSection],
        selection: Binding<String?>,
        context: HubContext,
        initialLifecycle: EngineLifecycle = .booting,
        footer: Footer
    ) {
        self.init(
            sections: sections, selection: selection, context: context, initialLifecycle: initialLifecycle,
            trailing: { _ in HubNoTrailing() }, footer: { footer })
    }
}

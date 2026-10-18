import SwiftUI

/// One sidebar row.
public struct HubSidebarItem: Identifiable, Hashable, Sendable {
    public let id: String
    public let title: String
    /// An SF Symbol name.
    public let symbol: String
    /// The group this row belongs to (a small heading above its first row), or nil.
    public let group: String?

    public init(id: String, title: String, symbol: String, group: String? = nil) {
        self.id = id
        self.title = title
        self.symbol = symbol
        self.group = group
    }
}

/// The sidebar, drawn on the window's chrome colour: the global search box in the header band, then
/// the rows, then an optional footer.
///
/// Rows are Buttons in a VStack in a ScrollView (no List, so no separators). Each run of rows in a
/// named group starts with a small heading, and a gap separates runs. The selected row is a tab in
/// the content panel's colour that runs to the sidebar's edge and curves into the panel above and
/// below, so it reads as joined to the content. The tab and its two concave curves are drawn from
/// rounded rectangles behind the rows (no `.background`, `.clipShape` or `.offset`), at positions
/// computed from the fixed row, heading and gap heights.
public struct HubSidebar<Footer: View>: View {
    private let items: [HubSidebarItem]
    private let selection: Binding<String?>
    private let query: Binding<String>
    private let footer: Footer
    private let hasFooter: Bool
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    /// - Parameter footer: A view pinned under the rows (a wordmark and a button, say). The sidebar
    ///   draws the hairline above it and gives it the footer height.
    public init(
        items: [HubSidebarItem],
        selection: Binding<String?>,
        query: Binding<String>,
        @ViewBuilder footer: () -> Footer
    ) {
        self.items = items
        self.selection = selection
        self.query = query
        self.footer = footer()
        self.hasFooter = true
    }

    fileprivate init(items: [HubSidebarItem], selection: Binding<String?>, query: Binding<String>, none: Footer) {
        self.items = items
        self.selection = selection
        self.query = query
        self.footer = none
        self.hasFooter = false
    }

    private var palette: HubPalette { theme.palette(scheme) }

    // MARK: Layout

    /// The fixed vertical gap between runs of rows (`.rk-group` margin).
    private static var groupGap: CGFloat { 14 }
    /// The height of a group heading, including the space under it.
    private static var headingHeight: CGFloat { 18 }
    /// The radius of the tab's left corners and of the two concave curves.
    private static var fuse: CGFloat { 12 }

    private enum Entry {
        case gap
        case heading(String)
        case row(HubSidebarItem)
    }

    private var entries: [Entry] {
        var result: [Entry] = []
        var lastGroup: String?
        var started = false
        for item in items {
            if !started || lastGroup != item.group {
                result.append(.gap)
                if let group = item.group { result.append(.heading(group)) }
            }
            started = true
            lastGroup = item.group
            result.append(.row(item))
        }
        return result
    }

    /// The distance from the top of the rows to the top of the selected row, or nil for none.
    private func selectedTop(_ entries: [Entry]) -> CGFloat? {
        var y: CGFloat = 0
        for entry in entries {
            switch entry {
            case .gap: y += Self.groupGap
            case .heading: y += Self.headingHeight
            case .row(let item):
                if item.id == selection.wrappedValue { return y }
                y += theme.layout.sidebarRowHeight
            }
        }
        return nil
    }

    // MARK: Body

    public var body: some View {
        VStack(spacing: 0) {
            HubSearchField(text: query)
                .padding(.horizontal, theme.spacing.row)
                .frame(height: theme.layout.headerHeight)
            ScrollView {
                nav
                    .padding(.top, theme.layout.sidebarTopInset)
                    .padding(.bottom, theme.spacing.lg)
            }
            if hasFooter {
                Rectangle()
                    .fill(palette.hairline.color)
                    .frame(height: 0.5)
                footer
                    .padding(.leading, theme.spacing.lg)
                    .padding(.trailing, theme.spacing.row)
                    .frame(height: theme.layout.sidebarFooterHeight)
            }
        }
    }

    private var nav: some View {
        let list = entries
        let width = theme.layout.sidebarWidth
        return HubLayered {
            if let top = selectedTop(list) {
                selectionShapes(top: top, width: width)
            }
            VStack(alignment: .leading, spacing: 0) {
                ForEach(Array(list.enumerated()), id: \.offset) { entry in
                    view(for: entry.element)
                }
            }
            .frame(width: width, alignment: .leading)
        }
        .frame(width: width, alignment: .topLeading)
    }

    @ViewBuilder
    private func view(for entry: Entry) -> some View {
        switch entry {
        case .gap:
            Color.clear.frame(width: 1, height: Self.groupGap)
        case .heading(let title):
            Text(verbatim: title)
                .font(theme.type.micro.font)
                .foregroundStyle(palette.ink3.color)
                .lineLimit(1)
                .padding(.leading, theme.spacing.sm + theme.spacing.row)
                .padding(.bottom, 3)
                .frame(maxWidth: .infinity, alignment: .bottomLeading)
                .frame(height: Self.headingHeight, alignment: .bottomLeading)
        case .row(let item):
            row(item)
        }
    }

    private func row(_ item: HubSidebarItem) -> some View {
        let selected = item.id == selection.wrappedValue
        return Button {
            selection.wrappedValue = item.id
        } label: {
            HubLayered {
                HubHitArea()
                HStack(spacing: theme.spacing.sm) {
                    Image(systemName: item.symbol)
                        .foregroundStyle(palette.accent.color)
                        .frame(width: 16)
                    Text(verbatim: item.title)
                        .font(theme.type.body.font)
                        .foregroundStyle(palette.ink.color)
                        .lineLimit(1)
                    Spacer(minLength: 0)
                }
                .padding(.leading, theme.spacing.row)
                .padding(.trailing, theme.spacing.sm)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .frame(height: theme.layout.sidebarRowHeight)
        }
        .buttonStyle(.plain)
        .padding(.leading, theme.spacing.sm)
        .padding(.trailing, selected ? 0 : theme.spacing.sm)
        .accessibilityLabel(item.title)
        .accessibilityValue(selected ? "selected" : "")
    }

    /// The tab and its two curves, in nav coordinates (origin at the top-left of the first row).
    @ViewBuilder
    private func selectionShapes(top: CGFloat, width: CGFloat) -> some View {
        let r = Self.fuse
        let rowHeight = theme.layout.sidebarRowHeight
        let inset = theme.spacing.sm
        ZStack(alignment: .topLeading) {
            // The tab: rounded on the left, square where it meets the content panel.
            ZStack(alignment: .trailing) {
                RoundedRectangle(cornerRadius: r).fill(palette.plane.color)
                Rectangle().fill(palette.plane.color).frame(width: r)
            }
            .frame(width: width - inset, height: rowHeight)
            .padding(.leading, inset)
            .padding(.top, top)
            // Above the tab: a square of panel colour with a chrome disc cut from its top-left
            // corner. The disc is twice the square, so it overhangs upward and to the left.
            ZStack(alignment: .bottomTrailing) {
                Rectangle().fill(palette.plane.color).frame(width: r, height: r)
                RoundedRectangle(cornerRadius: r).fill(palette.chrome.color).frame(width: 2 * r, height: 2 * r)
            }
            .frame(width: 2 * r, height: r, alignment: .bottomTrailing)
            .padding(.leading, width - 2 * r)
            .padding(.top, top - r)
            // Below the tab: the same, with the disc cut from its bottom-left corner and
            // overhanging downward.
            ZStack(alignment: .topTrailing) {
                Rectangle().fill(palette.plane.color).frame(width: r, height: r)
                RoundedRectangle(cornerRadius: r).fill(palette.chrome.color).frame(width: 2 * r, height: 2 * r)
            }
            .frame(width: 2 * r, height: r, alignment: .topTrailing)
            .padding(.leading, width - 2 * r)
            .padding(.top, top + rowHeight)
        }
    }
}

public extension HubSidebar where Footer == HubNoTrailing {
    /// A sidebar with no footer.
    init(items: [HubSidebarItem], selection: Binding<String?>, query: Binding<String>) {
        self.init(items: items, selection: selection, query: query, none: HubNoTrailing())
    }
}

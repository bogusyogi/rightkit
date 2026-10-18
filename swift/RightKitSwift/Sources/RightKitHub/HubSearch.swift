import SwiftUI

/// What a search result points at.
public enum HubSearchResultKind: String, Sendable, Equatable {
    /// A section itself.
    case section
    /// An entry a section added with `searchEntries`.
    case entry
    /// A setting from a section's `settings`.
    case setting
}

/// One global search result. Choosing it selects `sectionID`.
public struct HubSearchResult: Identifiable, Sendable, Equatable {
    /// Unique across all sections.
    public let id: String
    public let sectionID: String
    public let sectionTitle: String
    public let kind: HubSearchResultKind
    public let title: String
    public let detail: String
    /// An SF Symbol name: the section's for sections and entries, a switch for settings.
    public let symbol: String

    public init(
        id: String,
        sectionID: String,
        sectionTitle: String,
        kind: HubSearchResultKind,
        title: String,
        detail: String,
        symbol: String
    ) {
        self.id = id
        self.sectionID = sectionID
        self.sectionTitle = sectionTitle
        self.kind = kind
        self.title = title
        self.detail = detail
        self.symbol = symbol
    }
}

/// Global search over every section's title, `searchEntries` and `settings`.
/// Every term of the query must match (the settings-ui rule); results keep the sidebar order.
public struct HubSearchIndex: Sendable {
    private struct Item: Sendable {
        let result: HubSearchResult
        let haystack: String
    }

    private let items: [Item]

    /// The most results `search` returns.
    public static let resultLimit = 50

    /// Builds the index. Call it again when the sections change; it is cheap.
    public init(sections: [any HubSection]) {
        var items: [Item] = []
        for section in sections {
            items.append(Item(
                result: HubSearchResult(
                    id: "section:\(section.id)",
                    sectionID: section.id,
                    sectionTitle: section.title,
                    kind: .section,
                    title: section.title,
                    detail: "",
                    symbol: section.symbol
                ),
                haystack: section.title.lowercased()
            ))
            for entry in section.searchEntries {
                let text = ([section.title, entry.title, entry.detail] + entry.keywords)
                    .joined(separator: " ")
                    .lowercased()
                items.append(Item(
                    result: HubSearchResult(
                        id: "entry:\(section.id):\(entry.id)",
                        sectionID: section.id,
                        sectionTitle: section.title,
                        kind: .entry,
                        title: entry.title,
                        detail: entry.detail,
                        symbol: section.symbol
                    ),
                    haystack: text
                ))
            }
            for group in section.settings {
                for setting in group.settings {
                    items.append(Item(
                        result: HubSearchResult(
                            id: "setting:\(section.id):\(setting.key)",
                            sectionID: section.id,
                            sectionTitle: section.title,
                            kind: .setting,
                            title: setting.title,
                            detail: setting.detail,
                            symbol: "switch.2"
                        ),
                        haystack: HubSettingsSearch.haystack(setting, in: group) + " " + section.title.lowercased()
                    ))
                }
            }
        }
        self.items = items
    }

    /// The results for a query, empty when the query has no terms.
    public func search(_ query: String) -> [HubSearchResult] {
        let terms = HubSettingsSearch.terms(query)
        if terms.isEmpty { return [] }
        var out: [HubSearchResult] = []
        for item in items where terms.allSatisfy({ item.haystack.contains($0) }) {
            out.append(item.result)
            if out.count >= Self.resultLimit { break }
        }
        return out
    }
}

/// The search box: a magnifier, a TextField and a clear button.
public struct HubSearchField: View {
    private let text: Binding<String>
    private let placeholder: String
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    public init(text: Binding<String>, placeholder: String = "Search") {
        self.text = text
        self.placeholder = placeholder
    }

    private var palette: HubPalette { theme.palette(scheme) }

    public var body: some View {
        HubLayered {
            RoundedRectangle(cornerRadius: theme.radii.field).fill(palette.fill2.color)
            HStack(spacing: theme.spacing.sm - 2) {
                Image(systemName: "magnifyingglass")
                    .foregroundStyle(palette.ink3.color)
                TextField(placeholder, text: text)
                    .textFieldStyle(.plain)
                    .font(theme.type.body.font)
                    .foregroundStyle(palette.ink.color)
                    // Named by its placeholder, so screen readers and UI automation find it.
                    .accessibilityLabel(placeholder)
                if !text.wrappedValue.isEmpty {
                    Button {
                        text.wrappedValue = ""
                    } label: {
                        Image(systemName: "xmark.circle.fill")
                            .foregroundStyle(palette.ink3.color)
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Clear search")
                }
            }
            .padding(.horizontal, theme.spacing.sm)
            .frame(height: theme.layout.searchHeight)
        }
    }
}

/// The result list: one tappable row per result. Choosing a row calls `onChoose`.
public struct HubSearchResults: View {
    private let results: [HubSearchResult]
    private let onChoose: @MainActor (HubSearchResult) -> Void
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    public init(results: [HubSearchResult], onChoose: @escaping @MainActor (HubSearchResult) -> Void) {
        self.results = results
        self.onChoose = onChoose
    }

    private var palette: HubPalette { theme.palette(scheme) }

    public var body: some View {
        if results.isEmpty {
            HubEmptyState(symbol: "magnifyingglass", title: "No results", message: "Try a different word.")
        } else {
            List {
                ForEach(results) { result in
                    Button {
                        onChoose(result)
                    } label: {
                        row(result)
                    }
                    .buttonStyle(.plain)
                }
            }
        }
    }

    private func row(_ result: HubSearchResult) -> some View {
        HubLayered {
            HubHitArea()
            HStack(spacing: theme.spacing.row) {
                Image(systemName: result.symbol)
                    .foregroundStyle(palette.ink2.color)
                    .frame(width: 20)
                VStack(alignment: .leading, spacing: 2) {
                    Text(verbatim: result.title)
                        .font(theme.type.body.font)
                        .foregroundStyle(palette.ink.color)
                        .lineLimit(1)
                    if !result.detail.isEmpty {
                        Text(verbatim: result.detail)
                            .font(theme.type.caption.font)
                            .foregroundStyle(palette.ink3.color)
                            .lineLimit(1)
                    }
                }
                Spacer()
                if result.kind != .section {
                    Text(verbatim: result.sectionTitle)
                        .font(theme.type.small.font)
                        .foregroundStyle(palette.ink3.color)
                        .lineLimit(1)
                }
            }
            .padding(.vertical, theme.spacing.xs)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

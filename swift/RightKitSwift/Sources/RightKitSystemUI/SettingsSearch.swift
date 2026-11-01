#if os(macOS)
import Foundation
import RightKitHub
import SwiftUI

// The page's own "Search settings" box (the Tauri page's `SearchBar`, Settings.tsx). Typing keeps only the
// rows whose name, note, group title or page title contain every typed word, and the groups that still
// have a row. The Tauri page also lists the other sections' matches under their headings; here the hub's
// global search (the sidebar's Search) does that, and this box filters the page it is on.

/// One group of a page: its heading and its rows.
struct SettingsCardSpec: Identifiable {
    let id: String
    let title: String
    let rows: [SettingsRowSpec]
}

/// Collects the groups of a page, skipping those that have no row.
struct SettingsCardList {
    private(set) var cards: [SettingsCardSpec] = []

    mutating func add(_ title: String, _ rows: [SettingsRowSpec]) {
        if rows.isEmpty { return }
        cards.append(SettingsCardSpec(id: "\(cards.count).\(title)", title: title, rows: rows))
    }
}

enum SettingsSearch {
    /// The typed words, lower-cased.
    static func words(_ query: String) -> [String] {
        query.lowercased().split(whereSeparator: { $0 == " " || $0 == "\t" || $0 == "\n" }).map { String($0) }
    }

    static func matches(_ text: String, _ words: [String]) -> Bool {
        let haystack = text.lowercased()
        for word in words where !haystack.contains(word) {
            return false
        }
        return true
    }

    /// The rows of `card` that stay for `words`, with the rows that belong to them: a foot follows the row
    /// above it, and an error foot always stays with a group that still shows; a note or heading foot goes.
    static func filter(_ card: SettingsCardSpec, page: String, words: [String]) -> [SettingsRowSpec] {
        if words.isEmpty { return card.rows }
        var kept: [SettingsRowSpec] = []
        var anchor = true
        for row in card.rows {
            if row.isFoot {
                if row.isAlert && anchor { kept.append(row) }
            } else {
                let shown = row.isPinned || matches("\(row.text) \(card.title) \(page)", words)
                anchor = shown
                if shown { kept.append(row) }
            }
        }
        return kept
    }

    /// True when the filtered rows hold a row a search found (a pinned row does not count).
    static func hasResult(_ rows: [SettingsRowSpec]) -> Bool {
        rows.contains { !$0.isFoot && !$0.isPinned }
    }
}

/// The groups of a page, filtered by the page's search; "No setting matches" when nothing is left.
struct SettingsCardStack: View {
    let model: SettingsPageModel
    let cards: [SettingsCardSpec]
    @Environment(\.hubTheme) private var theme
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        let words = SettingsSearch.words(model.query)
        let page = model.kind.title
        let shown = cards.compactMap { card -> SettingsCardSpec? in
            let rows = SettingsSearch.filter(card, page: page, words: words)
            if !words.isEmpty && !SettingsSearch.hasResult(rows) { return nil }
            return SettingsCardSpec(id: card.id, title: card.title, rows: rows)
        }
        VStack(alignment: .leading, spacing: theme.spacing.lg - 2) {
            if !words.isEmpty && shown.isEmpty {
                Text(verbatim: "No setting matches \u{201C}\(model.query.trimmingCharacters(in: .whitespaces))\u{201D}.")
                    .font(theme.type.caption.font)
                    .foregroundStyle(theme.palette(scheme).ink2.color)
                    .padding(.horizontal, 2)
                    .padding(.vertical, 4)
            }
            ForEach(shown) { card in
                SettingsGroupCard(title: card.title, rows: card.rows)
            }
        }
    }
}

/// The search box over the page (`.ck-search`): a field named "Search settings" and, once it has text,
/// a Clear button. It stays put while the page scrolls under it.
struct SettingsSearchBar: View {
    let model: SettingsPageModel
    @Environment(\.hubTheme) private var theme

    private var binding: Binding<String> {
        let current = model
        return Binding(
            get: { MainActor.assumeIsolated { current.query } },
            set: { value in
                MainActor.assumeIsolated { current.query = value }
            }
        )
    }

    var body: some View {
        HStack(spacing: theme.spacing.sm) {
            TextField("Search settings", text: binding)
                .textFieldStyle(.roundedBorder)
                .accessibilityLabel("Search settings")
            if !model.query.isEmpty {
                SettingsButton("Clear", kind: .ghost) { model.query = "" }
            }
        }
        .padding(.horizontal, theme.spacing.xl - 2)
        .padding(.top, theme.spacing.lg - 2)
        .padding(.bottom, theme.spacing.sm)
    }
}
#endif

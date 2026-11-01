import SwiftUI

/// Search across local session titles and message content (Atom 13). Reads from the same
/// `LocalSessionStore` already in the environment; the index is rebuilt each time the query
/// or session list changes, which stays cheap because everything is already in memory.
public struct LocalSessionSearchView: View {
    @Environment(LocalSessionStore.self) private var sessions
    @State private var query = ""
    /// Set by the caller to navigate to a session when a result is tapped.
    public var onSelect: (String) -> Void

    public init(onSelect: @escaping (String) -> Void = { _ in }) {
        self.onSelect = onSelect
    }

    private var results: [LocalSessionSearchResult] {
        LocalSessionSearchIndex(sessions: sessions.sessions).search(query)
    }

    public var body: some View {
        List {
            ForEach(results) { result in
                Button {
                    onSelect(result.sessionId)
                } label: {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(result.title)
                            .font(.body.weight(.medium))
                            .foregroundStyle(Theme.text)
                        Text(result.snippet)
                            .font(.caption)
                            .foregroundStyle(Theme.muted)
                            .lineLimit(2)
                    }
                }
                .buttonStyle(.plain)
            }
        }
        .listStyle(.plain)
        .scrollContentBackground(.hidden)
        .background(Theme.background)
        .overlay {
            if !query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && results.isEmpty {
                ContentUnavailableView.search(text: query)
            }
        }
        .searchable(text: $query, prompt: "Search sessions")
        .navigationTitle("Search")
    }
}

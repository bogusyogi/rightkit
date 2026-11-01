import Foundation

/// One search hit: which session matched, and a coarse relevance score for ranking.
public struct LocalSessionSearchResult: Identifiable, Equatable, Sendable {
    public var id: String { sessionId }
    public let sessionId: String
    public let title: String
    /// The best-matching snippet (a message's text, or the title) shown under the title.
    public let snippet: String
    /// Higher is more relevant. Title matches outrank message-body matches, and more
    /// query-token hits outrank fewer.
    public let score: Int
}

/// In-memory local index over `LocalSessionRecord` titles + message text (Atom 13).
///
/// Deliberately hand-rolled rather than a dependency: sessions live entirely on-device
/// already (`LocalSessionStore`), so this only needs cheap, ranked substring/token matching
/// over data already in memory — no full-text engine, no persistence of its own. Rebuilding
/// the index from a session list is O(n) over messages and cheap enough to do on every
/// search-view appearance rather than maintaining incremental update plumbing.
public struct LocalSessionSearchIndex {
    private struct Document {
        let sessionId: String
        let title: String
        let titleTokens: [String]
        /// Each message's lowercase text, kept separate from title so a match there scores
        /// lower than a title match.
        let bodyTokens: [[String]]
        let bodyText: [String]
    }

    private let documents: [Document]

    public init(sessions: [LocalSessionRecord]) {
        documents = sessions.map { session in
            Document(
                sessionId: session.id,
                title: session.title,
                titleTokens: Self.tokenize(session.title),
                bodyTokens: session.messages.map { Self.tokenize($0.text) },
                bodyText: session.messages.map(\.text)
            )
        }
    }

    /// Split on whitespace/punctuation, lowercased, empty tokens dropped.
    private static func tokenize(_ text: String) -> [String] {
        text.lowercased()
            .components(separatedBy: CharacterSet.alphanumerics.inverted)
            .filter { !$0.isEmpty }
    }

    /// Ranked matches for `query`, best first. Empty or whitespace-only query returns no
    /// results rather than the whole session list, so an empty search field renders an
    /// empty (not "everything") state.
    public func search(_ query: String) -> [LocalSessionSearchResult] {
        let queryTokens = Self.tokenize(query)
        guard !queryTokens.isEmpty else { return [] }

        var results: [LocalSessionSearchResult] = []
        for document in documents {
            var bestScore = 0
            var bestSnippet: String?

            let titleMatches = queryTokens.filter { token in
                document.titleTokens.contains { $0.contains(token) }
            }.count
            if titleMatches > 0 {
                bestScore = max(bestScore, titleMatches * 10)
                bestSnippet = document.title
            }

            for (index, tokens) in document.bodyTokens.enumerated() {
                let matches = queryTokens.filter { token in
                    tokens.contains { $0.contains(token) }
                }.count
                guard matches > 0 else { continue }
                let score = matches * 3
                if score > bestScore {
                    bestScore = score
                    bestSnippet = document.bodyText[index]
                }
            }

            guard bestScore > 0 else { continue }
            let snippet = (bestSnippet ?? document.title)
                .trimmingCharacters(in: .whitespacesAndNewlines)
            results.append(
                LocalSessionSearchResult(
                    sessionId: document.sessionId,
                    title: document.title,
                    snippet: String(snippet.prefix(140)),
                    score: bestScore
                )
            )
        }

        return results.sorted { lhs, rhs in
            lhs.score == rhs.score ? lhs.title < rhs.title : lhs.score > rhs.score
        }
    }
}

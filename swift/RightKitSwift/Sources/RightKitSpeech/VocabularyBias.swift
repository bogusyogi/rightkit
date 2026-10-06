import Foundation

/// Contextual biasing for the beam decode. Parity port of macOS's
/// `ContextBias` (`heardright-parakeet-core/src/bias.rs`): phrases are stored as
/// token-id sequences encoded against this bundle's vocabulary, not as flat
/// per-piece logit boosts. A hypothesis that has already emitted a prefix of
/// a bias phrase earns `score` on the token that continues the match — this
/// is what lets a beam width > 1 keep a two-token phrase alive long enough to
/// outscore a locally-better single-token guess, which a flat per-token nudge
/// on a single greedy path can never do.
struct ASRVocabularyBias {
    private let phraseTokenIDs: [[Int]]
    private let score: Float

    /// Direct constructor mirroring Rust's `ContextBias::new` / test helpers —
    /// encodes each raw phrase string against `pieces`.
    init(phrases: [String], pieces: [String], score: Float) {
        guard score.isFinite, score > 0, !phrases.isEmpty else {
            phraseTokenIDs = []
            self.score = 0
            return
        }
        phraseTokenIDs = phrases
            .compactMap { Self.encodePhraseGreedy($0, pieces: pieces) }
            .filter { !$0.isEmpty }
        self.score = score
    }

    /// Construct directly from already-encoded phrase token-id sequences —
    /// used by tests that drive the bonus arithmetic without a vocabulary.
    init(phraseTokenIDs: [[Int]], score: Float) {
        self.phraseTokenIDs = phraseTokenIDs
        self.score = score
    }

    var isEmpty: Bool {
        phraseTokenIDs.isEmpty || score == 0
    }

    /// For each phrase, if the emitted-token suffix matches a prefix of the
    /// phrase, the next expected token earns `score` (max across phrases).
    /// Mirrors Rust's `ContextBias::next_token_bonuses`.
    func nextTokenBonuses(emitted: [Int]) -> [Int: Float] {
        var bonuses: [Int: Float] = [:]
        guard !isEmpty else { return bonuses }
        for phrase in phraseTokenIDs {
            let matched = Self.matchingPrefixLen(emitted: emitted, phrase: phrase)
            guard matched < phrase.count else { continue }
            let token = phrase[matched]
            bonuses[token] = max(bonuses[token] ?? -Float.infinity, score)
        }
        return bonuses
    }

    /// Longest suffix-of-`emitted` that is a prefix-of-`phrase`, capped so it
    /// can never equal the full phrase (there is always at least one more
    /// token left to bias toward). Mirrors Rust's `matching_prefix_len`.
    static func matchingPrefixLen(emitted: [Int], phrase: [Int]) -> Int {
        let maxLen = min(emitted.count, max(phrase.count - 1, 0))
        var len = maxLen
        while len > 0 {
            if Array(emitted.suffix(len)) == Array(phrase.prefix(len)) {
                return len
            }
            len -= 1
        }
        return 0
    }

    /// Trim (and strip zero-width spaces), drop empties, de-duplicate ignoring case and diacritics,
    /// cap at `cap` phrases. Same rule as HeardRight's `UserTextRules.normalizedVocabulary`.
    static func normalizedVocabulary(_ terms: [String], cap: Int = 2_000) -> [String] {
        var seen: Set<String> = []
        var out: [String] = []
        for term in terms {
            let trimmed = term
                .replacingOccurrences(of: "\u{200B}", with: "")
                .trimmingCharacters(in: .whitespacesAndNewlines)
            let key = trimmed.folding(options: [.caseInsensitive, .diacriticInsensitive], locale: nil)
            guard !trimmed.isEmpty, !seen.contains(key) else { continue }
            seen.insert(key)
            out.append(trimmed)
        }
        return Array(out.prefix(max(0, cap)))
    }

    /// Lowercase; keep `[a-z0-9']`; every other character becomes a space;
    /// collapse whitespace runs; trim. Mirrors Rust's
    /// `normalize_phrase_for_bias` exactly (ASCII-only alphanumeric check —
    /// any non-ASCII character is dropped to a space regardless of case).
    static func normalizePhraseForBias(_ phrase: String) -> String {
        var mapped = ""
        mapped.reserveCapacity(phrase.count)
        for scalar in phrase.lowercased().unicodeScalars {
            if ("a"..."z").contains(scalar) || ("0"..."9").contains(scalar) || scalar == "'" {
                mapped.unicodeScalars.append(scalar)
            } else {
                mapped.append(" ")
            }
        }
        return mapped
            .split(whereSeparator: { $0 == " " })
            .joined(separator: " ")
    }

    /// Greedy longest-match SentencePiece encode of a phrase against
    /// `pieces` — identical to Rust's `encode_phrase_greedy`: normalize, join
    /// `▁`-prefixed words, then repeatedly take the LONGEST vocab piece
    /// (skipping empty/`<...>` pieces) that prefixes what's left. `nil` if
    /// any position has no match.
    static func encodePhraseGreedy(_ phrase: String, pieces: [String]) -> [Int]? {
        let normalized = normalizePhraseForBias(phrase)
        guard !normalized.isEmpty else { return nil }

        var pieceText = ""
        for word in normalized.split(separator: " ") {
            pieceText.append("\u{2581}")
            pieceText.append(contentsOf: word)
        }

        var remaining = Substring(pieceText)
        var ids: [Int] = []
        while !remaining.isEmpty {
            var best: (id: Int, length: Int)?
            for (id, token) in pieces.enumerated() {
                guard !token.isEmpty, !token.hasPrefix("<") else { continue }
                guard remaining.hasPrefix(token) else { continue }
                let length = token.count
                if best.map({ length > $0.length }) ?? true {
                    best = (id, length)
                }
            }
            guard let (id, length) = best else { return nil }
            ids.append(id)
            remaining = remaining.dropFirst(length)
        }
        return ids
    }
}

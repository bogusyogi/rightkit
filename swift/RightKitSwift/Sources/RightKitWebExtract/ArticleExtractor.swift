#if os(iOS)
import Foundation

enum ArticleError: LocalizedError {
    case invalidURL
    case fetchFailed(Int)
    case notText

    var errorDescription: String? {
        switch self {
        case .invalidURL: return "That doesn't look like a valid URL."
        case .fetchFailed(let status): return "The site refused the request (HTTP \(status))."
        case .notText: return "The response wasn't readable text."
        }
    }
}

/// Readability-lite article extraction, two rungs of the desktop's ladder:
/// 1. plain fetch with browser headers → prefer the <article>/<main> region,
///    strip boilerplate, decode entities;
/// 2. WKWebView render when the fetch yields a thin/empty body — the iOS
///    stand-in for the desktop's headless browser. This is what makes
///    JS-rendered pages (Threads posts, app-shell sites) extractable.
/// The desktop's TLS-fingerprint rung has no iOS equivalent.
enum ArticleExtractor {
    /// Below this many characters, a "successful" fetch is really an app
    /// shell — the content is rendered client-side. Escalate to the WebView.
    private static let thinBodyThreshold = 600

    /// Upper bound on extracted text, applied in `readableText` so the fetch
    /// path and the WebView path are capped identically.
    static let maxTextCharacters = 500_000

    /// The fallback trigger: no fetched text (the fetch threw) or fewer than
    /// `thinBodyThreshold` characters after readability extraction.
    static func needsRenderFallback(fetchedText: String?) -> Bool {
        (fetchedText?.count ?? 0) < thinBodyThreshold
    }

    struct Output: Sendable {
        var title: String
        var text: String
    }

    static func extract(urlString: String) async throws -> Output {
        guard let url = URL(string: urlString), FetchURLPolicy.isAllowed(url) else {
            throw ArticleError.invalidURL
        }

        var fetchError: Error?
        var fetched: (title: String?, text: String)?
        do {
            let html = try await fetchHTML(url: url)
            fetched = (extractTitle(from: html), readableText(from: html))
        } catch {
            fetchError = error
        }

        if let fetched, !needsRenderFallback(fetchedText: fetched.text) {
            return Output(title: fetched.title ?? url.host ?? urlString, text: fetched.text)
        }

        // Thin body, or a fetch the site refused: render it like a browser.
        // `!text.isEmpty` is the guard that matters: when the fetch threw,
        // `fetched` is nil, so a bare `text.count >= 0` accepted an EMPTY
        // render and returned it as a successful artifact — a job reporting
        // "done" with nothing to show, which is exactly the failure the queue
        // is supposed to make impossible.
        try Task.checkCancellation()
        if let rendered = await WebViewRenderer.renderedHTML(of: url) {
            try Task.checkCancellation()
            let text = readableText(from: rendered)
            if !text.isEmpty, text.count >= (fetched?.text.count ?? 0) {
                return Output(title: extractTitle(from: rendered) ?? url.host ?? urlString, text: text)
            }
        }
        // A render cancelled with the job returns nil, not an error: without
        // this the thin fetch below would be saved as a finished artifact.
        try Task.checkCancellation()

        if let fetched, !fetched.text.isEmpty {
            return Output(title: fetched.title ?? url.host ?? urlString, text: fetched.text)
        }
        throw fetchError ?? ArticleError.notText
    }

    private static func fetchHTML(url: URL) async throws -> String {
        var request = URLRequest(url: url, timeoutInterval: 30)
        request.setValue(
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1",
            forHTTPHeaderField: "User-Agent"
        )
        request.setValue("text/html,application/xhtml+xml", forHTTPHeaderField: "Accept")

        let (data, response) = try await URLSession.shared.data(for: request, delegate: PolicyRedirectGuard())
        if let http = response as? HTTPURLResponse, !(200..<300).contains(http.statusCode) {
            throw ArticleError.fetchFailed(http.statusCode)
        }
        guard let html = String(data: data, encoding: .utf8)
                ?? String(data: data, encoding: .isoLatin1) else {
            throw ArticleError.notText
        }
        return html
    }

    // MARK: - HTML → text

    static func extractTitle(from html: String) -> String? {
        guard let match = firstMatch("<title[^>]*>(.*?)</title>", in: html) else { return nil }
        let title = decodeEntities(match).trimmingCharacters(in: .whitespacesAndNewlines)
        return title.isEmpty ? nil : title
    }

    static func readableText(from html: String) -> String {
        // Prefer the semantic article region when the page declares one.
        var scope = firstMatch("<article[^>]*>(.*?)</article>", in: html)
            ?? firstMatch("<main[^>]*>(.*?)</main>", in: html)
            ?? html

        for tag in ["script", "style", "noscript", "svg", "nav", "header", "footer", "aside", "form"] {
            scope = replacing("<\(tag)[^>]*>.*?</\(tag)>", in: scope, with: " ")
        }
        scope = replacing("<!--.*?-->", in: scope, with: " ")
        // Block-level closers become paragraph breaks before tags are dropped.
        scope = replacing("</(p|div|section|h[1-6]|li|blockquote|tr|br)>|<br\\s*/?>", in: scope, with: "\n")
        scope = replacing("<[^>]+>", in: scope, with: " ")
        scope = decodeEntities(scope)
        scope = replacing("[ \\t]+", in: scope, with: " ")
        scope = replacing(" ?\\n ?", in: scope, with: "\n")
        scope = replacing("\\n{3,}", in: scope, with: "\n\n")
        let trimmed = scope.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.count > maxTextCharacters ? String(trimmed.prefix(maxTextCharacters)) : trimmed
    }

    static func decodeEntities(_ text: String) -> String {
        var output = text
        // Numeric entities first — sites escape ordinary characters this way
        // (Threads ships "@" as "&#064;"), and a named-only table leaves the
        // raw escape sitting in the extracted text.
        output = replacingMatches(of: "&#([0-9]{1,7});", in: output) { digits in
            UInt32(digits).flatMap(Unicode.Scalar.init).map(String.init)
        }
        output = replacingMatches(of: "&#[xX]([0-9a-fA-F]{1,6});", in: output) { hex in
            UInt32(hex, radix: 16).flatMap(Unicode.Scalar.init).map(String.init)
        }

        let entities: [(String, String)] = [
            ("&nbsp;", " "), ("&lt;", "<"), ("&gt;", ">"),
            ("&quot;", "\""), ("&apos;", "'"), ("&rsquo;", "\u{2019}"),
            ("&lsquo;", "\u{2018}"), ("&rdquo;", "\u{201D}"), ("&ldquo;", "\u{201C}"),
            ("&mdash;", "\u{2014}"), ("&ndash;", "\u{2013}"), ("&hellip;", "\u{2026}"),
            // &amp; last: decoding it earlier would turn "&amp;lt;" into "<".
            ("&amp;", "&"),
        ]
        for (entity, character) in entities {
            output = output.replacingOccurrences(of: entity, with: character)
        }
        return output
    }

    /// Regex replace where the replacement is computed from capture group 1.
    private static func replacingMatches(
        of pattern: String,
        in text: String,
        transform: (String) -> String?
    ) -> String {
        guard let regex = regex(pattern) else { return text }
        var output = text
        let matches = regex.matches(in: text, range: NSRange(text.startIndex..., in: text))
        // Reverse order keeps earlier ranges valid as we splice.
        for match in matches.reversed() {
            guard match.numberOfRanges > 1,
                  let full = Range(match.range, in: output),
                  let captured = Range(match.range(at: 1), in: output),
                  let replacement = transform(String(output[captured])) else { continue }
            output.replaceSubrange(full, with: replacement)
        }
        return output
    }

    // MARK: - Regex helpers (case-insensitive, dot matches newlines)

    private static func regex(_ pattern: String) -> NSRegularExpression? {
        try? NSRegularExpression(pattern: pattern, options: [.caseInsensitive, .dotMatchesLineSeparators])
    }

    private static func firstMatch(_ pattern: String, in text: String) -> String? {
        guard let regex = regex(pattern),
              let match = regex.firstMatch(in: text, range: NSRange(text.startIndex..., in: text)),
              match.numberOfRanges > 1,
              let range = Range(match.range(at: 1), in: text) else { return nil }
        return String(text[range])
    }

    private static func replacing(_ pattern: String, in text: String, with replacement: String) -> String {
        guard let regex = regex(pattern) else { return text }
        return regex.stringByReplacingMatches(
            in: text,
            range: NSRange(text.startIndex..., in: text),
            withTemplate: replacement
        )
    }
}
#endif

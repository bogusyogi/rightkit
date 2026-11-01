import Foundation

/// What a queue row is called before its job finishes.
///
/// A job has no title until it completes, and falling back to the raw input
/// meant rows rendered as full URLs — wrapping to two lines and impossible to
/// scan. This derives something human from the URL instead; the full input is
/// still shown on the result screen.
enum JobDisplay {
    /// Path segments that carry no meaning as a label because the identifying
    /// part lives elsewhere — YouTube keeps the video id in the query string,
    /// so `/watch` has "watch" as its last path component. For these the bare
    /// host reads better than "youtube.com — watch".
    private static let uninformativeSlugs: Set<String> = [
        "watch", "index", "p", "reel", "reels", "status", "video", "post", "share", "embed", "v",
    ]

    static func label(for job: Job) -> String {
        if let title = job.title, !title.isEmpty { return title }
        return label(forInput: job.input)
    }

    static func label(forInput input: String) -> String {
        guard let url = URL(string: input), let host = url.host, !host.isEmpty else { return input }
        let trimmedHost = host.hasPrefix("www.") ? String(host.dropFirst(4)) : host

        guard let slug = url.pathComponents.last(where: { $0 != "/" && !$0.isEmpty }),
              !uninformativeSlugs.contains(slug.lowercased()),
              slug.count > 2
        else { return trimmedHost }

        let readable = slug
            .replacingOccurrences(of: "_", with: " ")
            .replacingOccurrences(of: "-", with: " ")
        return "\(trimmedHost) — \(readable)"
    }
}

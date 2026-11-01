#if os(iOS)
import CryptoKit
import Foundation
import UIKit

enum CarouselError: LocalizedError {
    case invalidURL
    case loginWall
    case unavailable(String)
    case noSlides

    var errorDescription: String? {
        switch self {
        case .invalidURL:
            return "That doesn't look like a valid post URL."
        case .loginWall:
            return "This post is behind a login. Public posts work on iPhone; private ones need the desktop app's signed-in browser."
        case .unavailable(let reason):
            return reason
        case .noSlides:
            return "No slides could be captured from this post."
        }
    }
}

/// Public-post carousel capture — a port of the desktop's
/// `crates/engine/src/carousel.rs` loop: render the page, refuse a login wall,
/// then screenshot → click Next → screenshot until the frame stops changing,
/// and read the caption from `og:description`. OCR runs over each slide.
///
/// Cookies are the desktop's *fallback* for private/age-gated posts, not its
/// normal path; public posts need no auth, which is why this works on iOS
/// where a cookie'd browser profile doesn't exist. Login-walled posts are
/// detected and refused rather than worked around.
///
/// Deviation from desktop (deliberate): desktop writes one artifact per slide;
/// here a post is one artifact with slide-delimited sections, because a post is
/// one thing on a phone and the Library is a flat list.
enum CarouselExtractor {
    private static let maxSlides = 20
    private static let slideSettleNanoseconds: UInt64 = 900_000_000

    /// Desktop's selector ladder, in the same order. No selector-memory file
    /// on iOS — one process, short sessions, not worth the mechanism.
    private static let nextSelectors = [
        "[aria-label='Next']",
        "button[aria-label='Next']",
        "svg[aria-label='Next']",
        "[data-testid='nextButton']",
    ]

    struct Output: Sendable {
        var title: String
        var text: String
    }

    @MainActor
    static func extract(urlString: String) async throws -> Output {
        guard let url = URL(string: urlString), FetchURLPolicy.isAllowed(url) else {
            throw CarouselError.invalidURL
        }

        let session = WebViewSession()
        defer { session.close() }

        do {
            try await session.load(url)
        } catch {
            throw CarouselError.unavailable("The post didn't load.")
        }

        if await isLoginWall(session) { throw CarouselError.loginWall }
        if let gate = await contentGate(session) { throw CarouselError.unavailable(gate) }

        // Dismiss the "open in app" / dialog close button if one is present.
        _ = await session.click(selector: "svg[aria-label='Close']")

        let images = await captureSlides(session)
        guard !images.isEmpty else { throw CarouselError.noSlides }

        let caption = await session.eval(
            "document.querySelector('meta[property=\"og:description\"]')?.content || ''"
        ) as? String

        let ocrTexts: [String] = try await Task.detached(priority: .userInitiated) {
            try images.compactMap { image -> String? in
                guard let cgImage = image.cgImage else { return nil }
                return try OCRExtractor.recognizeText(in: cgImage)
            }
        }.value

        // Gate on what we CAPTURED, not just on what the DOM said before it
        // hydrated. Instagram renders its login wall and "Post isn't
        // available" page as an ordinary page, so screenshot+OCR will happily
        // turn an error screen into a confident-looking artifact. Checking the
        // pixels we're about to hand the user is the only reliable signal.
        if let wall = wallMessage(in: ocrTexts.joined(separator: "\n")) {
            throw wall
        }

        var sections: [String] = []
        if let caption, !caption.isEmpty {
            sections.append("Caption:\n\(caption)")
        }
        for (index, text) in ocrTexts.enumerated() where !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            sections.append("— Slide \(index + 1) —\n\(text)")
        }
        guard !sections.isEmpty else { throw CarouselError.noSlides }

        let title = caption.flatMap { $0.isEmpty ? nil : String($0.prefix(60)) }
            ?? url.lastPathComponent
        return Output(title: title, text: sections.joined(separator: "\n\n"))
    }

    /// Screenshot → advance → repeat. Stops when the frame repeats (the
    /// desktop's hash-dedupe: a Next click that changes nothing means the
    /// carousel ended) or nothing advances.
    @MainActor
    private static func captureSlides(_ session: WebViewSession) async -> [UIImage] {
        var images: [UIImage] = []
        var lastHash: String?

        for _ in 1...maxSlides {
            guard let image = await session.snapshot() else { break }
            let hash = digest(of: image)
            if let lastHash, hash == lastHash { break }
            lastHash = hash
            images.append(image)

            var advanced = false
            for selector in nextSelectors where await session.click(selector: selector) {
                advanced = true
                break
            }
            if !advanced { break }
            try? await Task.sleep(nanoseconds: slideSettleNanoseconds)
        }
        return images
    }

    /// Reads captured slide text for the wall/error screens Instagram renders
    /// as normal pages. Markers are deliberately paired ("log in" AND "sign
    /// up for instagram") so a caption that merely says "log in" isn't
    /// mistaken for a wall.
    static func wallMessage(in capturedText: String) -> CarouselError? {
        let text = capturedText.lowercased()
        if text.contains("isn't available") || text.contains("isnt available")
            || text.contains("page not found") {
            return .unavailable("That post isn't available — the link may be broken, private, or removed.")
        }
        if text.contains("this account is private") {
            return .unavailable("That account is private. The desktop app's signed-in browser can reach it; iPhone can't.")
        }
        if text.contains("age-restricted") || text.contains("confirm your age") {
            return .unavailable("That post is age-gated and needs the desktop app's signed-in browser.")
        }
        if text.contains("log in") || text.contains("login"),
           text.contains("sign up for instagram") || text.contains("create new account") {
            return .loginWall
        }
        return nil
    }

    private static func digest(of image: UIImage) -> String {
        guard let data = image.pngData() else { return UUID().uuidString }
        return SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
    }

    @MainActor
    private static func isLoginWall(_ session: WebViewSession) async -> Bool {
        var href = session.currentURL?.absoluteString
        if href == nil {
            href = await session.eval("location.href") as? String
        }
        guard let href else { return false }
        return href.contains("/accounts/login") || href.contains("/login")
    }

    /// Mirrors the desktop's `instagram_page_gate`: the page loaded, but it's
    /// telling us the content isn't there.
    @MainActor
    private static func contentGate(_ session: WebViewSession) async -> String? {
        let text = (await session.eval("document.body?.innerText || ''") as? String ?? "").lowercased()
        if text.contains("sorry, this page isn't available") {
            return "That post isn't available (removed, renamed, or private)."
        }
        if text.contains("this account is private") {
            return "That account is private — the desktop app's signed-in browser can reach it; iPhone can't."
        }
        if text.contains("age-restricted") || text.contains("confirm your age") {
            return "That post is age-gated and needs the desktop app's signed-in browser."
        }
        return nil
    }
}
#endif

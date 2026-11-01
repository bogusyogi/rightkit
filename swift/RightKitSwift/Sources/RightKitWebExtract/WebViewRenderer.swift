#if os(iOS)
import Foundation
import UIKit
import WebKit

enum WebViewError: LocalizedError {
    case loadTimedOut
    case blocked
    var errorDescription: String? {
        switch self {
        case .loadTimedOut: return "The page took too long to load."
        case .blocked: return "That page tried to open somewhere ScrapeRight won't go."
        }
    }
}

/// The iOS stand-in for the desktop's headless browser (`crates/engine/src/
/// browser.rs`): loads a URL in a real WebKit view, lets its JavaScript run,
/// then reads the settled DOM, clicks things, and snapshots the viewport.
///
/// It renders a public page the way Safari would and reads what the user could
/// see — no media download, no platform API, no credentials. The desktop's
/// cookie'd browser profile has no iOS counterpart, so login-walled content is
/// detected and refused rather than worked around.
@MainActor
final class WebViewSession: NSObject, WKNavigationDelegate, WKUIDelegate {
    /// Give scripts a moment past `didFinish` to hydrate; app shells paint
    /// their content in the first frames after load.
    static let settleNanoseconds: UInt64 = 1_200_000_000
    private static let loadTimeoutNanoseconds: UInt64 = 25_000_000_000

    private let webView: WKWebView
    private var continuation: CheckedContinuation<Void, Error>?

    override init() {
        let configuration = WKWebViewConfiguration()
        // Ephemeral: no cookies or cache persist. ScrapeRight iOS never signs
        // in to a platform and shouldn't look like it does.
        configuration.websiteDataStore = .nonPersistent()
        // Scripts run only because this view exists for one load; no pop-up
        // windows, no media that starts on its own.
        configuration.defaultWebpagePreferences.allowsContentJavaScript = true
        configuration.preferences.javaScriptCanOpenWindowsAutomatically = false
        configuration.mediaTypesRequiringUserActionForPlayback = .all
        configuration.defaultWebpagePreferences.preferredContentMode = .desktop
        webView = WKWebView(frame: CGRect(x: 0, y: 0, width: 1024, height: 1400), configuration: configuration)
        super.init()
        webView.navigationDelegate = self
        webView.uiDelegate = self
        webView.isUserInteractionEnabled = false
        attachOffscreen()
    }

    /// Getting an invisible-but-real render right has two traps, both of which
    /// silently produce garbage slides rather than errors:
    ///
    /// 1. WebKit only paints what's on screen — a detached view snapshots
    ///    blank. So the view goes in the window, scaled down by a transform:
    ///    the layout stays at 1024 CSS px (desktop layout is what puts real
    ///    Next arrows in Instagram's DOM instead of mobile swipe), the whole
    ///    thing is on-screen, and `takeSnapshot` still captures at the view's
    ///    own full resolution.
    /// 2. **Do not hide it with `alpha`.** View alpha is baked into
    ///    `takeSnapshot`, so `alpha = 0.01` yields a 1%-opacity ghost image
    ///    that OCR reads as fragments (this cost a debugging round: the text
    ///    was all there, just invisible). Hide it by z-order instead — index 0
    ///    puts it behind the app's opaque UI, where it still renders.
    private func attachOffscreen() {
        guard let window = UIApplication.shared.connectedScenes
            .compactMap({ $0 as? UIWindowScene })
            .flatMap(\.windows)
            .first(where: { $0.isKeyWindow }) else { return }
        let scale = min(
            window.bounds.width / webView.bounds.width,
            window.bounds.height / webView.bounds.height,
            1
        )
        webView.transform = CGAffineTransform(scaleX: scale, y: scale)
        // The scaled view must sit ENTIRELY within the window: WebKit doesn't
        // paint what's off-screen, and an unpainted region snapshots as blank
        // white — which reads as a truncated slide, not as an error.
        webView.center = CGPoint(x: window.bounds.midX, y: window.bounds.midY)
        window.insertSubview(webView, at: 0)
    }

    var currentURL: URL? { webView.url }

    func load(_ url: URL) async throws {
        try await awaitNavigation { [weak self] in
            self?.webView.load(URLRequest(url: url, timeoutInterval: 20))
        }
    }

    /// Test seam: drive the same render/snapshot path from a literal document,
    /// so the mechanism can be proven without depending on a live site.
    func loadHTML(_ html: String, baseURL: URL? = nil) async throws {
        try await awaitNavigation { [weak self] in
            self?.webView.loadHTMLString(html, baseURL: baseURL)
        }
    }

    /// Start a navigation and wait for the delegate, with a hard ceiling.
    ///
    /// The cancellation handler is load-bearing, not decoration. A plain
    /// `withCheckedThrowingContinuation` does NOT observe cancellation: when the
    /// timeout child won the race, the group cancelled the navigation child and
    /// then *awaited* it — and it was blocked on a continuation nothing would
    /// ever resume, so `load` hung forever instead of timing out. The timeout
    /// was defeated by its own implementation. Resuming the continuation on
    /// cancel is what makes the ceiling real. `finish` guards double-resume.
    private func awaitNavigation(_ start: @escaping @MainActor () -> Void) async throws {
        try await withThrowingTaskGroup(of: Void.self) { group in
            group.addTask { @MainActor [weak self] in
                try await withTaskCancellationHandler {
                    try await withCheckedThrowingContinuation { continuation in
                        self?.continuation = continuation
                        start()
                    }
                } onCancel: {
                    Task { @MainActor in self?.finish(.failure(CancellationError())) }
                }
            }
            group.addTask {
                try await Task.sleep(nanoseconds: Self.loadTimeoutNanoseconds)
                throw WebViewError.loadTimedOut
            }
            defer { group.cancelAll() }
            try await group.next()
        }
        try? await Task.sleep(nanoseconds: Self.settleNanoseconds)
    }

    func html() async -> String? {
        try? await webView.evaluateJavaScript("document.documentElement.outerHTML") as? String
    }

    @discardableResult
    func eval(_ javaScript: String) async -> Any? {
        try? await webView.evaluateJavaScript(javaScript)
    }

    /// True when the click landed on something. Mirrors the desktop's
    /// click-a-selector contract.
    func click(selector: String) async -> Bool {
        let script = """
        (() => { const el = document.querySelector(\(jsString(selector)));
                 if (!el) { return false; }
                 el.click(); return true; })()
        """
        return (await eval(script)) as? Bool ?? false
    }

    func snapshot() async -> UIImage? {
        let config = WKSnapshotConfiguration()
        config.rect = CGRect(origin: .zero, size: webView.bounds.size)
        return try? await webView.takeSnapshot(configuration: config)
    }

    func close() {
        webView.stopLoading()
        webView.navigationDelegate = nil
        webView.uiDelegate = nil
        webView.removeFromSuperview()
    }

    private func jsString(_ value: String) -> String {
        let data = try? JSONSerialization.data(withJSONObject: [value], options: [.fragmentsAllowed])
        let encoded = data.flatMap { String(data: $0, encoding: .utf8) } ?? "[\"\"]"
        return String(encoded.dropFirst().dropLast())
    }

    private func finish(_ result: Result<Void, Error>) {
        guard let continuation else { return }
        self.continuation = nil
        continuation.resume(with: result)
    }

    // MARK: - Navigation policy

    /// Every navigation — the first load, redirects, link/script navigations,
    /// frames — must pass `FetchURLPolicy` (http/https, non-local host).
    /// `about:blank` is allowed only because `loadHTML` uses it. Custom
    /// schemes, tel:, mailto:, app-store redirects and downloads are cancelled.
    func webView(
        _ webView: WKWebView,
        decidePolicyFor navigationAction: WKNavigationAction,
        decisionHandler: @escaping @MainActor @Sendable (WKNavigationActionPolicy) -> Void
    ) {
        let url = navigationAction.request.url
        let isBlank = url?.absoluteString == "about:blank"
        let allowed = !navigationAction.shouldPerformDownload
            && (isBlank || url.map(FetchURLPolicy.isAllowed) == true)
        // A cancelled navigation can end without any didFail callback, which
        // would leave the load waiting out its full timeout; fail it now.
        // A refused sub-frame (ad or embed) is only cancelled: the page still loads.
        let isMainFrame = navigationAction.targetFrame?.isMainFrame ?? true
        if !allowed && isMainFrame { finish(.failure(WebViewError.blocked)) }
        decisionHandler(allowed ? .allow : .cancel)
    }

    /// Anything WebKit can't show inline (or the server marks as an
    /// attachment) is a download; refuse it.
    func webView(
        _ webView: WKWebView,
        decidePolicyFor navigationResponse: WKNavigationResponse,
        decisionHandler: @escaping @MainActor @Sendable (WKNavigationResponsePolicy) -> Void
    ) {
        let disposition = (navigationResponse.response as? HTTPURLResponse)?
            .value(forHTTPHeaderField: "Content-Disposition")?.lowercased() ?? ""
        let isAttachment = disposition.hasPrefix("attachment")
        let allowed = navigationResponse.canShowMIMEType && !isAttachment
        if !allowed && navigationResponse.isForMainFrame { finish(.failure(WebViewError.blocked)) }
        decisionHandler(allowed ? .allow : .cancel)
    }

    /// No pop-ups: window.open / target=_blank never get a new view.
    func webView(
        _ webView: WKWebView,
        createWebViewWith configuration: WKWebViewConfiguration,
        for navigationAction: WKNavigationAction,
        windowFeatures: WKWindowFeatures
    ) -> WKWebView? { nil }

    nonisolated func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        Task { @MainActor in finish(.success(())) }
    }

    nonisolated func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        Task { @MainActor in finish(.failure(error)) }
    }

    nonisolated func webView(
        _ webView: WKWebView,
        didFailProvisionalNavigation navigation: WKNavigation!,
        withError error: Error
    ) {
        Task { @MainActor in finish(.failure(error)) }
    }
}

/// One-shot render, used by the article path when a plain fetch returns a
/// JS shell.
enum WebViewRenderer {
    @MainActor
    static func renderedHTML(of url: URL) async -> String? {
        let session = WebViewSession()
        defer { session.close() }
        do {
            try await session.load(url)
        } catch {
            return nil
        }
        return await session.html()
    }
}
#endif

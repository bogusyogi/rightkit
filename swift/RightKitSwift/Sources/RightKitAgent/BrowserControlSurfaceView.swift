#if os(iOS)
import SwiftUI
@preconcurrency import WebKit

/// ENG-98: the client-owned browser surface an agent drives through `browser_control.rs`'s
/// relay tools (`browser_control_navigate`/`_read`/`_click`/`_type`/`_screenshot`). Desktop
/// CodeRight drives a real, daemon-local Chrome over CDP (`engine/crates/tools/src/browser.rs`);
/// there is no such process on iOS, so the "browser" here is a `WKWebView` this view owns
/// directly — the daemon can request a step, but it can never touch this WebView's process,
/// cookie jar, or navigation stack except through the bounded actions relayed to it.
///
/// ENG-113 boundary: any navigation this view performs on the agent's behalf, or that the
/// embedded page itself requests (a link, a redirect, `window.open`), is classified by
/// `ExternalSchemeClassifier`. `http`/`https` loads inside this WebView normally.
/// Anything else — `mailto:`, `tel:`, a third-party app's custom scheme — is never opened
/// automatically: it is resolved through `ExternalSchemeConsentStore.resolve(for:origin:)`,
/// which blocks on a fresh human decision unless one is already remembered for that scheme.
/// Page content and agent tool calls can request such a target; neither can grant the consent
/// that opens it.
@MainActor
struct BrowserControlSurfaceView: View {
    let sessionID: String
    /// A daemon-created preview URL. Lane A supplies reachability; this view
    /// only loads raw http(s) bytes through its isolated WKWebView.
    let rawPreviewURL: URL?

    @Environment(ExternalSchemeConsentStore.self) private var consentStore
    @State private var controller = BrowserControlSurfaceController()
    @State private var currentTitle: String = ""
    @State private var isLoading = false
    @State private var previewAddress = ""
    @State private var previewError: String?

    init(sessionID: String, rawPreviewURL: URL? = nil) {
        self.sessionID = sessionID
        self.rawPreviewURL = rawPreviewURL
    }

    /// Connected/cloud tunnel tools return a bounded JSON result rather than a
    /// browser command. Keep result parsing here so callers can hand its
    /// exact content to this raw WKWebView surface without a proxy, rewrite,
    /// or daemon-owned browser process.
    init(sessionID: String, tunnelResultContent: String) {
        self.init(
            sessionID: sessionID,
            rawPreviewURL: TunnelPreviewResult.url(from: tunnelResultContent)
        )
    }

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider().background(Theme.border)
            BrowserControlWebView(controller: controller, consentStore: consentStore)
                .background(Theme.surface)
        }
        .background(Theme.background)
        .task(id: sessionID) {
            controller.attach(sessionID: sessionID)
            if let rawPreviewURL {
                previewAddress = rawPreviewURL.absoluteString
                controller.loadRawPreview(rawPreviewURL)
            }
        }
        .onReceive(controller.titlePublisher) { title in
            currentTitle = title
        }
        .onReceive(controller.loadingPublisher) { loading in
            isLoading = loading
        }
        .externalSchemeConsentPresenting()
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                Image(systemName: "safari")
                    .foregroundStyle(Theme.muted)
                Text(currentTitle.isEmpty ? "Dev-server preview" : currentTitle)
                    .font(.subheadline.weight(.medium))
                    .foregroundStyle(Theme.text)
                    .lineLimit(1)
                Spacer()
                if isLoading {
                    ProgressView()
                        .controlSize(.small)
                }
            }
            HStack(spacing: 8) {
                TextField("https://preview.example.com", text: $previewAddress)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .keyboardType(.URL)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityIdentifier("dev-server-preview-url")
                Button("Open") {
                    guard let url = Self.previewURL(from: previewAddress) else {
                        previewError = "Enter a complete http(s) preview URL."
                        return
                    }
                    previewError = nil
                    controller.loadRawPreview(url)
                }
                .buttonStyle(.borderedProminent)
                .accessibilityIdentifier("dev-server-preview-open")
            }
            if let previewError {
                Text(previewError)
                    .font(.caption)
                    .foregroundStyle(Theme.danger)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(Theme.raised)
    }

    /// Tunnel providers return a real origin. Keep it real: URL validation is
    /// all this client does before issuing WKWebView's plain request.
    static func previewURL(from text: String) -> URL? {
        let value = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let url = URL(string: value),
            let scheme = url.scheme?.lowercased(),
            scheme == "http" || scheme == "https",
            url.host != nil
        else { return nil }
        return url
    }
}

/// Decodes only successful tunnel-start results emitted by `tunnel` or
/// `dev_tunnel_proxy`. A tunnel hostname is DNS, not a URL, so this adapter
/// always creates its public HTTPS origin itself. It deliberately ignores
/// stop/status/error output, local targets, arbitrary text, & non-http(s)
/// values.
@MainActor
enum TunnelPreviewResult {
    private struct Payload: Decodable {
        let event: String
        let publicHostname: String?

        enum CodingKeys: String, CodingKey {
            case event
            case publicHostname = "public_hostname"
        }
    }

    static func url(from resultContent: String) -> URL? {
        guard let payload = try? JSONDecoder().decode(Payload.self, from: Data(resultContent.utf8)),
              payload.event == "tunnel_started" || payload.event == "dev_tunnel_started",
              let hostname = payload.publicHostname?.trimmingCharacters(in: .whitespacesAndNewlines),
              !hostname.isEmpty,
              !hostname.contains("://"),
              !hostname.contains("/"),
              !hostname.contains(where: { $0.isWhitespace })
        else { return nil }
        return BrowserControlSurfaceView.previewURL(from: "https://\(hostname)")
    }
}

/// Bridges `WKWebView` delegate callbacks (which arrive off the main-actor-isolated SwiftUI
/// world through Objective-C dynamism) into the `@Observable`/Combine surface the SwiftUI view
/// and, eventually, the `ClientBrowserRelay` transport can consume. Marked `@MainActor` because
/// every `WKWebView` call and every consent decision it makes has to happen on the main thread —
/// this is a UIKit/WebKit-touching type, not a plain data model.
@MainActor
@Observable
final class BrowserControlSurfaceController: NSObject {
    private(set) var sessionID: String = ""
    private(set) var lastObservedURL: URL?
    private weak var webView: WKWebView?

    let titlePublisher = NotificationCenter.default
        .publisher(for: BrowserControlSurfaceController.titleDidChange)
        .compactMap { $0.userInfo?["title"] as? String }
    let loadingPublisher = NotificationCenter.default
        .publisher(for: BrowserControlSurfaceController.loadingDidChange)
        .compactMap { $0.userInfo?["loading"] as? Bool }

    static let titleDidChange = Notification.Name("BrowserControlSurfaceController.titleDidChange")
    static let loadingDidChange = Notification.Name(
        "BrowserControlSurfaceController.loadingDidChange")

    func attach(sessionID: String) {
        self.sessionID = sessionID
    }

    func bind(webView: WKWebView) {
        self.webView = webView
    }

    /// Navigate to an http(s) URL. Anything else must go through
    /// `requestExternalHandoff(for:origin:consentStore:)` instead — this function refuses a
    /// non-http(s) scheme outright rather than silently rewriting the request.
    func navigate(to url: URL) {
        guard let scheme = url.scheme?.lowercased(), scheme == "http" || scheme == "https" else {
            return
        }
        webView?.load(URLRequest(url: url))
    }

    /// Preview transport is deliberately a plain URL request: it does not
    /// proxy, rewrite, inject into, or inspect response bytes. External
    /// non-http(s) schemes remain rejected by `navigate(to:)`.
    func loadRawPreview(_ url: URL) {
        navigate(to: url)
    }

    func readCurrentState() -> (url: URL?, title: String?) {
        (webView?.url, webView?.title)
    }

    func captureScreenshot() async -> Data? {
        guard let webView else { return nil }
        let config = WKSnapshotConfiguration()
        return await withCheckedContinuation { continuation in
            webView.takeSnapshot(with: config) { image, _ in
                continuation.resume(returning: image?.pngData())
            }
        }
    }

    /// The ENG-113 seam: called from the navigation delegate whenever the WebView itself (via
    /// a page-authored link, redirect, or `window.open`) or a relayed agent action would need
    /// to hand off to a non-http(s) scheme. Blocks on `consentStore.resolve` — which prompts
    /// the human unless a decision for this exact scheme is already remembered — and performs
    /// the actual `UIApplication.open` handoff only if that resolves to `true`. There is no
    /// path from "page requested it" or "agent asked for it" directly to `UIApplication.open`;
    /// every such path funnels through this one function.
    func requestExternalHandoff(
        for url: URL,
        origin: String?,
        consentStore: ExternalSchemeConsentStore
    ) async {
        guard ExternalSchemeClassifier.requiresExternalConsent(url) else { return }
        let allowed = await consentStore.resolve(for: url, origin: origin)
        guard allowed else { return }
        await UIApplication.shared.open(url)
    }

    fileprivate func notifyTitle(_ title: String) {
        NotificationCenter.default.post(
            name: Self.titleDidChange,
            object: self,
            userInfo: ["title": title]
        )
    }

    fileprivate func notifyLoading(_ loading: Bool) {
        NotificationCenter.default.post(
            name: Self.loadingDidChange,
            object: self,
            userInfo: ["loading": loading]
        )
    }
}

/// `UIViewRepresentable` wrapper. Kept deliberately thin: all policy (consent, scheme
/// classification) lives in `BrowserControlSurfaceController` and
/// `ExternalSchemeConsentStore`/`ExternalSchemeClassifier` so it is testable without a
/// live `WKWebView`.
@MainActor
struct BrowserControlWebView: UIViewRepresentable {
    let controller: BrowserControlSurfaceController
    let consentStore: ExternalSchemeConsentStore

    func makeUIView(context: Context) -> WKWebView {
        let configuration = WKWebViewConfiguration()
        // Every client-owned browser surface gets its own ephemeral data store — mirroring
        // `browser.rs`'s per-session throwaway Chrome profile — so an agent-driven page can
        // never read the user's real Safari/app cookies or session state.
        configuration.websiteDataStore = .nonPersistent()
        let webView = WKWebView(frame: .zero, configuration: configuration)
        webView.navigationDelegate = context.coordinator
        webView.uiDelegate = context.coordinator
        controller.bind(webView: webView)
        return webView
    }

    func updateUIView(_ uiView: WKWebView, context: Context) {
        // State flows one way: SwiftUI drives the WebView through
        // `BrowserControlSurfaceController.navigate(to:)`, not through `updateUIView`, so a
        // relayed `browser_control_navigate` call and a SwiftUI re-render can never race each
        // other into a double-navigation.
    }

    func makeCoordinator() -> Coordinator {
        Coordinator(controller: controller, consentStore: consentStore)
    }

    @MainActor
    final class Coordinator: NSObject, WKNavigationDelegate, WKUIDelegate {
        private let controller: BrowserControlSurfaceController
        private let consentStore: ExternalSchemeConsentStore

        init(controller: BrowserControlSurfaceController, consentStore: ExternalSchemeConsentStore) {
            self.controller = controller
            self.consentStore = consentStore
        }

        func webView(
            _ webView: WKWebView,
            decidePolicyFor navigationAction: WKNavigationAction,
            decisionHandler: @escaping (WKNavigationActionPolicy) -> Void
        ) {
            guard let url = navigationAction.request.url else {
                decisionHandler(.allow)
                return
            }
            guard ExternalSchemeClassifier.requiresExternalConsent(url) else {
                decisionHandler(.allow)
                return
            }
            // Cancel the in-WebView navigation immediately — a non-http(s) target can never
            // load inside this surface — and resolve the handoff asynchronously through
            // consent. The page never learns the outcome faster than the human decides it.
            decisionHandler(.cancel)
            let origin = webView.url?.host
            Task { @MainActor in
                await controller.requestExternalHandoff(
                    for: url, origin: origin, consentStore: consentStore)
            }
        }

        func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
            controller.notifyLoading(false)
            controller.notifyTitle(webView.title ?? "")
        }

        func webView(_ webView: WKWebView, didStartProvisionalNavigation navigation: WKNavigation!) {
            controller.notifyLoading(true)
        }

        func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
            controller.notifyLoading(false)
        }

        /// `window.open(...)` with no target frame arrives here instead of
        /// `decidePolicyFor:navigationAction:` on some WebKit versions. Route it through the
        /// same non-http(s) check so a page cannot bypass consent by using `window.open`
        /// instead of an anchor click.
        func webView(
            _ webView: WKWebView,
            createWebViewWith configuration: WKWebViewConfiguration,
            for navigationAction: WKNavigationAction,
            windowFeatures: WKWindowFeatures
        ) -> WKWebView? {
            if let url = navigationAction.request.url {
                if ExternalSchemeClassifier.requiresExternalConsent(url) {
                    let origin = webView.url?.host
                    Task { @MainActor in
                        await controller.requestExternalHandoff(
                            for: url, origin: origin, consentStore: consentStore)
                    }
                } else {
                    controller.navigate(to: url)
                }
            }
            return nil
        }
    }
}
#endif

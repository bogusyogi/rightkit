import MarkdownUI
import SwiftUI
@preconcurrency import WebKit

/// Renders one artifact's content inline — in the transcript or in
/// `ArtifactSurfaceView`'s detail sheet — chosen by `ArtifactRecord.kind`
/// (ENG-115's "agent-emitted sandboxed interactive inline previews").
///
/// NOTE (integration gap): `Models/TranscriptMessage.swift` has no field
/// carrying an artifact reference today (only `diff: String?` for the
/// existing unified-diff card), so nothing currently instantiates this view
/// from inside the live transcript — only `ArtifactSurfaceView`'s sheet
/// does today. Wiring an `ArtifactRef`-shaped case into `TranscriptMessage`
/// and `MessageBubbleView` so an `artifact_create`/`artifact_update` tool
/// result renders this inline automatically is outside this lane's file
/// allowlist (`Models/TranscriptMessage.swift`, `Views/MessageBubbleView.swift`)
/// and is left for the integration owner.
public struct ArtifactInlinePreviewView: View {
    let artifact: ArtifactRecord
    let content: String

    public init(artifact: ArtifactRecord, content: String) {
        self.artifact = artifact
        self.content = content
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            Divider()
            ScrollView {
                body(for: artifact.kind)
                    .padding(12)
            }
        }
        .background(Theme.surface)
    }

    private var header: some View {
        HStack(spacing: 8) {
            Image(systemName: ArtifactKindIcon.systemName(for: artifact.kind))
                .foregroundStyle(Theme.accent)
            VStack(alignment: .leading, spacing: 1) {
                Text(artifact.path)
                    .font(.subheadline.weight(.medium))
                    .foregroundStyle(Theme.text)
                Text("\(artifact.kind) · v\(artifact.version) · \(artifact.producer)")
                    .font(.caption2)
                    .foregroundStyle(Theme.muted)
            }
            Spacer()
        }
        .padding(10)
        .background(Theme.raised)
    }

    @ViewBuilder
    private func body(for kind: String) -> some View {
        switch kind {
        case "markdown", "plan":
            Markdown(content)
        case "diff":
            ArtifactDiffPreview(diffText: content)
        case "html-preview":
            ArtifactSandboxedHTMLPreview(html: content)
                .frame(minHeight: 240)
        default:
            Text(content)
                .font(.system(.footnote, design: .monospaced))
                .foregroundStyle(Theme.text)
                .textSelection(.enabled)
        }
    }
}

/// Colored +/- rendering for a unified-diff artifact, matching the
/// house-style diff treatment already used for tool-approval diffs
/// elsewhere in the transcript.
private struct ArtifactDiffPreview: View {
    let diffText: String

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            ForEach(Array(diffText.split(separator: "\n", omittingEmptySubsequences: false).enumerated()), id: \.offset) { _, line in
                Text(String(line))
                    .font(.system(.caption, design: .monospaced))
                    .foregroundStyle(color(for: line))
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    private func color(for line: Substring) -> Color {
        if line.hasPrefix("+") && !line.hasPrefix("+++") { return Theme.accent2 }
        if line.hasPrefix("-") && !line.hasPrefix("---") { return Theme.danger }
        if line.hasPrefix("@@") { return Theme.accent }
        return Theme.muted
    }
}

/// A `WKWebView` sandboxed the same way `BrowserControlSurfaceView` sandboxes
/// remote pages: no `baseURL`, so relative-path network requests have
/// nowhere to resolve, and JavaScript stays on because an inline artifact
/// preview is explicitly meant to be interactive (ENG-115). This is content
/// an agent produced, not a remote site, but it is still untrusted enough
/// (an agent can be prompt-injected) that it must not be able to navigate
/// the app or reach an arbitrary origin — `decidePolicyFor` below cancels
/// every navigation that is not the initial `loadHTMLString` load.
@MainActor
private struct ArtifactSandboxedHTMLPreview: UIViewRepresentable {
    let html: String

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeUIView(context: Context) -> WKWebView {
        let configuration = WKWebViewConfiguration()
        configuration.allowsInlineMediaPlayback = true
        let webView = WKWebView(frame: .zero, configuration: configuration)
        webView.navigationDelegate = context.coordinator
        webView.isOpaque = false
        webView.backgroundColor = .clear
        webView.scrollView.backgroundColor = .clear
        webView.loadHTMLString(html, baseURL: nil)
        return webView
    }

    func updateUIView(_ webView: WKWebView, context: Context) {
        // Content is immutable for the lifetime of one preview instance —
        // a new artifact version is a new `ArtifactRecord`/view identity,
        // not an update to this one, so there is nothing to reload here.
    }

    final class Coordinator: NSObject, WKNavigationDelegate {
        func webView(
            _ webView: WKWebView,
            decidePolicyFor navigationAction: WKNavigationAction,
            decisionHandler: @escaping (WKNavigationActionPolicy) -> Void
        ) {
            // `.other` is the initial `loadHTMLString` navigation. Anything
            // else — a link tap, a script-driven `location` change, a form
            // submit — is cancelled: this surface previews one static
            // artifact body, it does not browse.
            decisionHandler(navigationAction.navigationType == .other ? .allow : .cancel)
        }
    }
}

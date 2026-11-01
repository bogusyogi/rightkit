import MarkdownUI
import SwiftUI

/// Renders assistant/user text with the small amount of markdown that actually shows up in a
/// coding-agent transcript: fenced code blocks, plus inline emphasis/code.
///
/// Uses the shared MarkdownUI parser. Theme enters outside individual style overrides so
/// prose inherits its containing surface rather than GitHub’s opaque text background.
struct MarkdownText: View {
    let raw: String

    var body: some View {
        let rendered = MessageRenderPipeline.live.render(raw)
        Markdown(rendered.markdownContent)
            .tint(Theme.accent)
            .markdownTextStyle(\.text) {
                ForegroundColor(Theme.text)
                BackgroundColor(nil)
            }
            .markdownTextStyle(\.link) {
                ForegroundColor(Theme.accent)
                UnderlineStyle(.single)
            }
            .markdownTextStyle(\.code) {
                FontFamilyVariant(.monospaced)
                FontSize(.em(0.85))
                BackgroundColor(Theme.surface)
            }
            .markdownBlockStyle(\.codeBlock) { configuration in
                ScrollView(.horizontal) {
                    configuration.label
                        .fixedSize(horizontal: false, vertical: true)
                        .relativeLineSpacing(.em(0.225))
                        .markdownTextStyle {
                            FontFamilyVariant(.monospaced)
                            FontSize(.em(0.85))
                        }
                        .padding(12)
                }
                .background(Theme.surface)
                .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
                .overlay(
                    RoundedRectangle(cornerRadius: 8, style: .continuous)
                        .stroke(Theme.border, lineWidth: 1)
                )
                .markdownMargin(top: 0, bottom: 12)
            }
            .markdownBlockStyle(\.table) { configuration in
                configuration.label
                    .fixedSize(horizontal: false, vertical: true)
                    .markdownTableBorderStyle(.init(color: Theme.border))
                    .markdownTableBackgroundStyle(
                        .alternatingRows(Theme.surface, Theme.background)
                    )
                    .markdownMargin(top: 0, bottom: 12)
            }
            .markdownCodeSyntaxHighlighter(CodexCodeSyntaxHighlighter())
            .markdownTheme(.gitHub)
            .textSelection(.enabled)
    }

    enum Segment: Equatable {
        case prose(String)
        case code(String)
    }

    /// Splits on ``` fences. An unterminated fence renders as code to end-of-message rather
    /// than silently swallowing the rest of the reply.
    static func segments(from raw: String) -> [Segment] {
        var out: [Segment] = []
        var inCode = false
        var buffer: [String] = []

        func flush() {
            let joined = buffer.joined(separator: "\n").trimmingCharacters(in: .newlines)
            if !joined.isEmpty { out.append(inCode ? .code(joined) : .prose(joined)) }
            buffer.removeAll()
        }

        for line in raw.components(separatedBy: .newlines) {
            if line.trimmingCharacters(in: .whitespaces).hasPrefix("```") {
                flush()
                inCode.toggle()
                continue
            }
            buffer.append(line)
        }
        flush()
        return out
    }

    static func attributed(_ text: String) -> AttributedString {
        (try? AttributedString(
            markdown: text,
            options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace)
        )) ?? AttributedString(text)
    }
}

private struct CodeBlock: View {
    let code: String

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            Text(code)
                .font(.system(.footnote, design: .monospaced))
                .foregroundStyle(Theme.text)
                // Code must never hyphenate or wrap mid-token — on device this produced
                // "UIRe-sponder" and made the block unreadable.
                .fixedSize(horizontal: true, vertical: false)
                .textSelection(.enabled)
                .padding(10)
        }
        .background(Theme.background.opacity(0.6), in: RoundedRectangle(cornerRadius: 8, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .stroke(Theme.border, lineWidth: 1)
        )
    }
}

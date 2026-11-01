import RightKitNotchCore
import RightKitPresentation
import SwiftUI

/// Desktop launcher response tokens (docs/architecture/emblem-response-ui.md section 5.2).
enum ResponseStyle {
    /// Added to the launcher's own 16 pt padding to give the 20 pt answer gutter.
    static let gutterExtra: CGFloat = 4
    static let sectionGap: CGFloat = 20
    static let paragraphGap: CGFloat = 10
    static let headingGap: CGFloat = 8
    static let rowGap: CGFloat = 6
    static let groupPadding: CGFloat = 16
    static let body: CGFloat = 15
    static let heading: CGFloat = 18
    static let metadata: CGFloat = 12
    static let code: CGFloat = 13

    /// The answer's colours: stronger text and edges under Increase Contrast, and an opaque
    /// surface in place of the translucent one under Reduce Transparency.
    struct Palette {
        let ink: Color
        let secondary: Color
        let inset: Color
        let separator: Color

        init(highContrast: Bool, reduceTransparency: Bool) {
            ink = Color.white.opacity(highContrast ? 1 : 0.92)
            secondary = Color.white.opacity(highContrast ? 0.85 : 0.55)
            inset = reduceTransparency ? Color(white: highContrast ? 0.22 : 0.17)
                                       : Color.white.opacity(highContrast ? 0.14 : 0.07)
            separator = Color.white.opacity(highContrast ? 0.4 : 0.14)
        }
    }

    static let ink = Color.white.opacity(0.92)
    static let secondary = Color.white.opacity(0.55)
    static let inset = Color.white.opacity(0.07)
    static let separator = Color.white.opacity(0.14)
    static let link = Color(red: 0.55, green: 0.75, blue: 1.0)
}

/// An answer drawn as native structure: headings, paragraphs, lists, quotes, code and tables.
/// No card around the body, text stays selectable, and the caller's ScrollView scrolls it.
/// Links are ordinary SwiftUI links, so they open only through the system's user-initiated
/// handler on a click, as the launcher's answers did before. Until `document` exists (it is built
/// off the main thread) and for an empty document, the plain answer text is shown.
struct ResponseView: View {
    let plainText: String
    let document: ResponseDocument?

    @Environment(\.viewShotStill) private var still
    @Environment(\.colorSchemeContrast) private var contrast
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private var palette: ResponseStyle.Palette {
        ResponseStyle.Palette(highContrast: contrast == .increased, reduceTransparency: reduceTransparency)
    }
    private var hasDocument: Bool { document.map { !$0.blocks.isEmpty } ?? false }

    var body: some View {
        VStack(alignment: .leading, spacing: ResponseStyle.sectionGap) {
            if let document, !document.blocks.isEmpty {
                ForEach(document.blocks, id: \.id) { block in
                    blockView(block)
                }
            } else {
                Text(plainText)
                    .font(.system(size: ResponseStyle.body))
                    .foregroundStyle(palette.ink)
                    .lineSpacing(3)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .topLeading)
            }
        }
        .padding(.horizontal, ResponseStyle.gutterExtra)
        .frame(maxWidth: .infinity, alignment: .topLeading)
        // The plain text gives way to the document with a short fade (none under Reduce Motion).
        .animation(reduceMotion || still ? nil : .easeInOut(duration: 0.18), value: hasDocument)
    }

    @ViewBuilder
    private func blockView(_ block: ResponseBlock) -> some View {
        switch block.content {
        case .text(let text):
            VStack(alignment: .leading, spacing: ResponseStyle.paragraphGap) {
                ForEach(Array(text.elements.enumerated()), id: \.offset) { index, element in
                    elementView(element, first: index == 0)
                }
            }
            .frame(maxWidth: .infinity, alignment: .topLeading)
        default:
            // The launcher builds text documents only; any other content shows its plain text.
            Text(block.fallbackText)
                .font(.system(size: ResponseStyle.body))
                .foregroundStyle(palette.ink)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .topLeading)
        }
    }

    @ViewBuilder
    private func elementView(_ element: TextElement, first: Bool) -> some View {
        switch element {
        case .heading(let level, let inline):
            // A heading sits 20 from what is above (10 gap + 10) and 8 from what follows (10 - 2).
            Self.text(inline, size: Self.headingSize(level))
                .fontWeight(.semibold)
                .foregroundStyle(palette.ink)
                .textSelection(.enabled)
                .padding(.top, first ? 0 : ResponseStyle.sectionGap - ResponseStyle.paragraphGap)
                .padding(.bottom, ResponseStyle.headingGap - ResponseStyle.paragraphGap)
                .frame(maxWidth: .infinity, alignment: .topLeading)
                .accessibilityAddTraits(.isHeader)
        case .paragraph(let inline):
            Self.text(inline, size: ResponseStyle.body)
                .foregroundStyle(palette.ink)
                .lineSpacing(3)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .topLeading)
        case .list(let ordered, let start, let items):
            VStack(alignment: .leading, spacing: ResponseStyle.rowGap) {
                let markers = ListMarkers.of(ordered: ordered, start: start, items: items)
                ForEach(Array(items.enumerated()), id: \.offset) { offset, item in
                    listItem(item, marker: markers[offset] ?? "")
                }
            }
            .frame(maxWidth: .infinity, alignment: .topLeading)
        case .quote(let inline):
            HStack(alignment: .top, spacing: 12) {
                RoundedRectangle(cornerRadius: 1.5)
                    .fill(Color.white.opacity(0.25))
                    .frame(width: 3)
                Self.text(inline, size: ResponseStyle.body)
                    .foregroundStyle(palette.secondary)
                    .lineSpacing(3)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .topLeading)
            }
            .fixedSize(horizontal: false, vertical: true)
        case .code(let language, let text):
            codeBlock(language: language, text: text)
        case .table(let table):
            tableView(table)
        case .lineBreak:
            Color.clear.frame(height: ResponseStyle.paragraphGap)
        }
    }

    // MARK: - Lists

    /// Nested items arrive with an em-space indent and their own marker in the text; they are
    /// drawn indented without a second marker.
    @ViewBuilder
    private func listItem(_ runs: [InlineRun], marker: String) -> some View {
        let nesting = Self.leadingIndent(runs)
        if nesting.depth > 0 {
            Self.text(nesting.runs, size: ResponseStyle.body)
                .foregroundStyle(palette.ink)
                .lineSpacing(3)
                .textSelection(.enabled)
                .padding(.leading, 26 + CGFloat(nesting.depth) * 14)
                .frame(maxWidth: .infinity, alignment: .topLeading)
        } else {
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text(marker)
                    .font(.system(size: ResponseStyle.body).monospacedDigit())
                    .foregroundStyle(palette.secondary)
                    .frame(minWidth: 20, alignment: .trailing)
                Self.text(runs, size: ResponseStyle.body)
                    .foregroundStyle(palette.ink)
                    .lineSpacing(3)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .topLeading)
            }
        }
    }

    private static func leadingIndent(_ runs: [InlineRun]) -> (depth: Int, runs: [InlineRun]) {
        guard let first = runs.first, first.text.hasPrefix("\u{2003}") else { return (0, runs) }
        let depth = first.text.prefix { $0 == "\u{2003}" }.count
        var rest = runs
        rest[0] = InlineRun(String(first.text.dropFirst(depth)), emphasis: first.emphasis,
                            strong: first.strong, code: first.code, link: first.link)
        return (depth, rest)
    }

    // MARK: - Code

    private func codeBlock(language: String?, text: String) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            if let language, !language.isEmpty {
                Text(language)
                    .font(.system(size: ResponseStyle.metadata))
                    .foregroundStyle(palette.secondary)
            }
            codeText(text)
        }
        .padding(ResponseStyle.groupPadding)
        .frame(maxWidth: .infinity, alignment: .topLeading)
        .background(RoundedRectangle(cornerRadius: 16).fill(palette.inset))
    }

    /// Whitespace is kept; long lines scroll sideways rather than lose characters.
    @ViewBuilder
    private func codeText(_ text: String) -> some View {
        let label = Text(text.isEmpty ? " " : text)
            .font(.system(size: ResponseStyle.code, design: .monospaced))
            .foregroundStyle(palette.ink)
            .textSelection(.enabled)
        if still {
            label.frame(maxWidth: .infinity, alignment: .topLeading)
        } else {
            ScrollView(.horizontal, showsIndicators: false) {
                label.fixedSize(horizontal: true, vertical: true)
            }
        }
    }

    // MARK: - Tables

    @ViewBuilder
    private func tableView(_ table: DisplayTable) -> some View {
        if Self.fitsAsGrid(table) {
            Grid(alignment: .topLeading, horizontalSpacing: 18, verticalSpacing: ResponseStyle.rowGap) {
                GridRow {
                    ForEach(Array(table.headers.enumerated()), id: \.offset) { index, header in
                        Text(header.isEmpty ? "Column \(index + 1)" : header)
                            .font(.system(size: ResponseStyle.metadata, weight: .semibold))
                            .foregroundStyle(palette.secondary)
                            .textSelection(.enabled)
                    }
                }
                Divider().overlay(palette.separator)
                    .gridCellColumns(max(table.headers.count, 1))
                ForEach(Array(table.rows.enumerated()), id: \.offset) { _, row in
                    GridRow {
                        ForEach(Array(row.enumerated()), id: \.offset) { _, cell in
                            Text(cell.isEmpty ? "\u{2013}" : cell)
                                .font(.system(size: 14))
                                .foregroundStyle(palette.ink)
                                .textSelection(.enabled)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .topLeading)
        } else {
            // Narrow: one record per row, each cell under its own header.
            VStack(alignment: .leading, spacing: ResponseStyle.paragraphGap) {
                ForEach(Array(table.rows.enumerated()), id: \.offset) { _, row in
                    VStack(alignment: .leading, spacing: 4) {
                        ForEach(Array(row.enumerated()), id: \.offset) { column, cell in
                            VStack(alignment: .leading, spacing: 1) {
                                Text(column < table.headers.count && !table.headers[column].isEmpty
                                     ? table.headers[column] : "Column \(column + 1)")
                                    .font(.system(size: ResponseStyle.metadata, weight: .semibold))
                                    .foregroundStyle(palette.secondary)
                                Text(cell.isEmpty ? "\u{2013}" : cell)
                                    .font(.system(size: 14))
                                    .foregroundStyle(palette.ink)
                                    .textSelection(.enabled)
                            }
                        }
                    }
                    .padding(ResponseStyle.groupPadding)
                    .frame(maxWidth: .infinity, alignment: .topLeading)
                    .background(RoundedRectangle(cornerRadius: 16).fill(palette.inset))
                }
            }
        }
    }

    /// A grid is used when the columns can plausibly share the answer's width (about 560 pt);
    /// otherwise stacked records, which never drop a column. A fixed estimate, so the choice does
    /// not depend on a layout pass.
    private static func fitsAsGrid(_ table: DisplayTable) -> Bool {
        let columns = table.headers.count
        guard columns > 0 else { return false }
        var estimate: CGFloat = 0
        for column in 0..<columns {
            var longest = table.headers[column].count
            for row in table.rows where column < row.count { longest = max(longest, row[column].count) }
            estimate += CGFloat(min(longest, 28)) * 7.5 + 18
        }
        return columns <= 4 && estimate <= 560
    }

    // MARK: - Inline text

    private static func headingSize(_ level: Int) -> CGFloat {
        switch level {
        case ...1: return ResponseStyle.heading
        case 2: return 16.5
        default: return 15
        }
    }

    /// Runs as one `Text`: emphasis, strong and code by presentation intent, links only for the
    /// http, https and mailto links the parser kept.
    static func text(_ runs: [InlineRun], size: CGFloat) -> Text {
        var out = AttributedString()
        for run in runs {
            var piece = AttributedString(run.text)
            var intent: InlinePresentationIntent = []
            if run.emphasis { intent.insert(.emphasized) }
            if run.strong { intent.insert(.stronglyEmphasized) }
            if run.code {
                intent.insert(.code)
                piece.font = .system(size: size - 1.5, design: .monospaced)
            }
            if !intent.isEmpty { piece.inlinePresentationIntent = intent }
            if let url = run.link {
                piece.link = url
                piece.foregroundColor = ResponseStyle.link
                piece.underlineStyle = .single
            }
            out.append(piece)
        }
        return Text(out).font(.system(size: size))
    }
}

import RightKitUI
import SwiftUI
import RightKitPresentation

// Small native views for each display value. None of them carries an action beyond local expansion:
// a row is static, a receipt describes an outcome, a code block has no Run control.

// MARK: Text

struct ResponseTextView: View {
    let document: TextDocument
    /// Draw a short first paragraph (with more after it) as the lead.
    var allowLead = false

    private var leadsWithParagraph: Bool {
        guard allowLead, document.elements.count > 1, case .paragraph(let runs) = document.elements[0] else { return false }
        let text = runs.map(\.text).joined()
        return text.count <= ResponseStyle.maxLeadCharacters && !text.contains("\n")
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(Array(document.elements.enumerated()), id: \.offset) { index, element in
                TextElementView(element: element, lead: index == 0 && leadsWithParagraph)
                    .padding(.top, index == 0 ? 0 : gap(before: index))
            }
            if document.unformatted {
                Text("Shown without formatting.")
                    .font(ResponseStyle.metadataFont)
                    .foregroundStyle(ResponseStyle.metadata)
                    .padding(.top, ResponseStyle.paragraphGap)
            }
        }
    }

    /// A heading sticks to what follows it; other neighbours get the paragraph gap.
    private func gap(before index: Int) -> CGFloat {
        if case .heading = document.elements[index - 1] { return ResponseStyle.headingGap }
        if case .heading = document.elements[index] { return ResponseStyle.sectionGap - 4 }
        if case .lineBreak = document.elements[index] { return 0 }
        return ResponseStyle.paragraphGap
    }
}

private struct TextElementView: View {
    let element: TextElement
    var lead = false

    var body: some View {
        switch element {
        case .heading(let level, let runs):
            Text(InlineText.attributed(runs))
                .font(ResponseStyle.heading(level: level))
                .foregroundStyle(ResponseStyle.text)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
                .accessibilityAddTraits(.isHeader)
        case .paragraph(let runs):
            Text(InlineText.attributed(runs))
                .font(lead ? ResponseStyle.lead : ResponseStyle.body)
                .foregroundStyle(ResponseStyle.text)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
                .textSelection(.enabled)
        case .list(let ordered, let start, let items):
            VStack(alignment: .leading, spacing: ResponseStyle.rowGap) {
                let markers = ListMarkers.of(ordered: ordered, start: start, items: items)
                ForEach(Array(items.enumerated()), id: \.offset) { offset, runs in
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        Text(markers[offset] ?? "")
                            .font(ResponseStyle.body.monospacedDigit())
                            .foregroundStyle(ResponseStyle.secondary)
                            .frame(minWidth: 22, alignment: .trailing)
                            .accessibilityHidden(true)
                        Text(InlineText.attributed(runs))
                            .font(ResponseStyle.body)
                            .foregroundStyle(ResponseStyle.text)
                            .fixedSize(horizontal: false, vertical: true)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .textSelection(.enabled)
                    }
                    .accessibilityElement(children: .combine)
                }
            }
        case .quote(let runs):
            HStack(alignment: .top, spacing: 12) {
                RoundedRectangle(cornerRadius: 1.5).fill(ResponseStyle.metadata.opacity(0.5)).frame(width: 3)
                    .accessibilityHidden(true)
                Text(InlineText.attributed(runs))
                    .font(ResponseStyle.body)
                    .foregroundStyle(ResponseStyle.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .textSelection(.enabled)
            }
        case .code(let language, let text):
            CodeBlockView(language: language, text: text)
        case .table(let table):
            ResponseTableView(table: table)
        case .lineBreak:
            Color.clear.frame(height: 4).accessibilityHidden(true)
        }
    }
}

/// Inline runs to styled text. Links are active only for http and https, through the system's user-initiated
/// open; any other scheme stays readable text. Nothing here fetches or runs anything.
enum InlineText {
    static func attributed(_ runs: [InlineRun]) -> AttributedString {
        var result = AttributedString()
        for run in runs {
            var piece = AttributedString(run.text)
            var intent: InlinePresentationIntent = []
            if run.emphasis { intent.insert(.emphasized) }
            if run.strong { intent.insert(.stronglyEmphasized) }
            if run.code {
                intent.insert(.code)
                piece.font = ResponseStyle.code
            }
            if !intent.isEmpty { piece.inlinePresentationIntent = intent }
            // Links are never activated on this surface (spec 8.3: only an existing user-initiated
            // path may open one, and Home had none). The address is shown after its label as text,
            // never dropped; the text stays selectable, so it can be copied (#505 I31).
            if let link = run.link, link.absoluteString != run.text {
                piece += AttributedString(" (\(link.absoluteString))")
            }
            result += piece
        }
        return result
    }
}

private struct CodeBlockView: View {
    let language: String?
    let text: String

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if let language, !language.isEmpty {
                Text(language)
                    .font(ResponseStyle.metadataFont)
                    .foregroundStyle(ResponseStyle.metadata)
            }
            // Whitespace preserved; long lines scroll sideways so no character is lost.
            ScrollView(.horizontal) {
                Text(text)
                    .font(ResponseStyle.code)
                    .foregroundStyle(ResponseStyle.text)
                    .fixedSize(horizontal: true, vertical: true)
                    .textSelection(.enabled)
            }
            .scrollIndicators(.hidden)
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.fill, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .accessibilityElement(children: .combine)
    }
}

// MARK: Table

/// Stacked records on a phone (each cell keeps its header); a contained grid when there is room.
struct ResponseTableView: View {
    let table: DisplayTable
    @Environment(\.horizontalSizeClass) private var sizeClass
    @Environment(\.dynamicTypeSize) private var typeSize

    private func header(_ column: Int) -> String {
        column < table.headers.count && !table.headers[column].isEmpty ? table.headers[column] : "Column \(column + 1)"
    }

    private func columns(of row: [String]) -> Int { max(table.headers.count, row.count) }

    var body: some View {
        if sizeClass == .compact || typeSize.isAccessibilitySize {
            VStack(alignment: .leading, spacing: ResponseStyle.rowGap) {
                ForEach(Array(table.rows.enumerated()), id: \.offset) { _, row in
                    VStack(alignment: .leading, spacing: 6) {
                        ForEach(0..<columns(of: row), id: \.self) { column in
                            let value = column < row.count ? row[column] : ""
                            VStack(alignment: .leading, spacing: 1) {
                                Text(header(column))
                                    .font(ResponseStyle.metadataFont)
                                    .foregroundStyle(ResponseStyle.metadata)
                                Text(value.isEmpty ? "(empty)" : value)
                                    .font(ResponseStyle.body)
                                    .foregroundStyle(value.isEmpty ? ResponseStyle.metadata : ResponseStyle.text)
                                    .fixedSize(horizontal: false, vertical: true)
                            }
                            .accessibilityElement(children: .combine)
                        }
                    }
                    .responseGroup()
                }
            }
        } else {
            ScrollView(.horizontal) {
                Grid(alignment: .topLeading, horizontalSpacing: 20, verticalSpacing: 10) {
                    GridRow {
                        ForEach(0..<table.headers.count, id: \.self) { column in
                            Text(header(column)).font(.subheadline.weight(.semibold)).foregroundStyle(ResponseStyle.text)
                        }
                    }
                    Divider()
                    ForEach(Array(table.rows.enumerated()), id: \.offset) { _, row in
                        GridRow {
                            ForEach(0..<columns(of: row), id: \.self) { column in
                                Text(column < row.count ? row[column] : "")
                                    .font(.subheadline).foregroundStyle(ResponseStyle.text)
                                    .frame(maxWidth: 260, alignment: .leading)
                                    .fixedSize(horizontal: false, vertical: true)
                            }
                        }
                    }
                }
                .padding(ResponseStyle.groupPadding)
            }
            .scrollIndicators(.hidden)
            .background(ResponseStyle.group, in: RoundedRectangle(cornerRadius: ResponseStyle.groupRadius, style: .continuous))
        }
    }
}

// MARK: Rows, notice, receipt

/// Short items only. Shows the first five and "Show all N" for the rest of the already loaded set.
struct ResponseRowsView: View {
    let rows: [DisplayRow]
    @State private var showAll = false
    @Environment(\.dynamicTypeSize) private var typeSize
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private var visible: ArraySlice<DisplayRow> {
        showAll ? rows[...] : rows.prefix(ResponseStyle.collapsedRows)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(Array(visible.enumerated()), id: \.element.id) { index, row in
                if index > 0 { Hairline() }
                RowView(row: row, vertical: typeSize.isAccessibilitySize)
            }
            if rows.count > ResponseStyle.collapsedRows {
                Hairline()
                Button {
                    withAnimation(reduceMotion ? nil : .easeInOut(duration: 0.18)) { showAll.toggle() }
                } label: {
                    Text(showAll ? "Show fewer" : "Show all \(rows.count)")
                        .font(.subheadline.weight(.medium))
                        .foregroundStyle(ResponseStyle.text)
                        .frame(maxWidth: .infinity, minHeight: ResponseStyle.minTarget, alignment: .leading)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("response.rows.toggle")
            }
        }
        .padding(.horizontal, ResponseStyle.groupPadding)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(ResponseStyle.group, in: RoundedRectangle(cornerRadius: ResponseStyle.groupRadius, style: .continuous))
    }

    private struct RowView: View {
        let row: DisplayRow
        let vertical: Bool

        var body: some View {
            Group {
                if vertical {
                    VStack(alignment: .leading, spacing: 4) { title; status }
                } else {
                    HStack(alignment: .firstTextBaseline, spacing: 12) {
                        title
                        Spacer(minLength: 8)
                        status
                    }
                }
            }
            .padding(.vertical, 10)
            .frame(maxWidth: .infinity, minHeight: ResponseStyle.minTarget, alignment: .leading)
            .accessibilityElement(children: .combine)
        }

        private var title: some View {
            VStack(alignment: .leading, spacing: 2) {
                Text(row.title).font(ResponseStyle.body).foregroundStyle(ResponseStyle.text)
                    .fixedSize(horizontal: false, vertical: true)
                if let detail = row.detail {
                    Text(detail).font(ResponseStyle.metadataFont).foregroundStyle(ResponseStyle.metadata)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }

        @ViewBuilder private var status: some View {
            if let status = row.status {
                StatusText(tone: ResponseStyle.tone(forStatus: status), text: status, font: .footnote)
            }
        }
    }
}

struct ResponseNoticeView: View {
    let notice: DisplayNotice

    private var tone: StatusTone {
        switch notice.tone {
        case .info: .info
        case .warning: .warn
        case .problem: .bad
        }
    }

    private var symbol: String {
        switch notice.tone {
        case .info: "info.circle"
        case .warning: "exclamationmark.triangle"
        case .problem: "exclamationmark.octagon"
        }
    }

    private var name: String {
        switch notice.tone {
        case .info: "Note"
        case .warning: "Warning"
        case .problem: "Problem"
        }
    }

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Image(systemName: symbol).font(.subheadline).foregroundStyle(tone.color).accessibilityHidden(true)
            Text(notice.message)
                .font(.subheadline).foregroundStyle(ResponseStyle.text)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
                .textSelection(.enabled)
        }
        .padding(12)
        .background(Theme.fill, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(name): \(notice.message)")
    }
}

/// What a send actually returned: the operation, where it went and the exact state word. It describes an
/// outcome only; it cannot approve, retry or resend anything.
struct ResponseReceiptView: View {
    let receipt: DisplayReceipt

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            VStack(alignment: .leading, spacing: 2) {
                Text(receipt.operation).font(.headline).foregroundStyle(ResponseStyle.text)
                Text(receipt.destination).font(ResponseStyle.metadataFont).foregroundStyle(ResponseStyle.metadata)
                    .fixedSize(horizontal: false, vertical: true)
            }
            StatusText(tone: ResponseStyle.tone(forReceipt: receipt.state), text: receipt.state, font: .subheadline)
            if let detail = receipt.detail {
                Text(detail).font(.subheadline).foregroundStyle(ResponseStyle.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .responseGroup()
        .accessibilityElement(children: .combine)
    }
}

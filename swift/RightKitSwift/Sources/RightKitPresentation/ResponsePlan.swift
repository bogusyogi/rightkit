import Foundation

/// Deterministic grouping and order for a response: text first, in its original order, then the
/// supporting projections in the order given. Makes no model request and rewrites nothing.
public enum ResponsePlan {
    public static func compose(id: String, revision: UInt64, canonicalText: String,
                               projections: [ResponseContent]) -> ResponseDocument {
        var texts: [ResponseContent] = []
        var supporting: [ResponseContent] = []
        for projection in projections {
            if case .text = projection { texts.append(projection) } else { supporting.append(projection) }
        }
        // Canonical text is for copy/fallback; a supplied projection already is the body.
        if projections.isEmpty, !canonicalText.isEmpty {
            texts.append(.text(TextDocument.parse(canonicalText)))
        }
        let ordered = texts + supporting
        var blocks: [ResponseBlock] = []
        blocks.reserveCapacity(ordered.count)
        for (index, content) in ordered.enumerated() {
            let fallback: String
            if case .text(let document) = content {
                // A lone text block is exactly the canonical text; otherwise its own plain text.
                fallback = texts.count == 1 ? canonicalText : plain(document)
            } else {
                fallback = plain(content)
            }
            blocks.append(ResponseBlock(id: "\(id)/\(index)", fallbackText: fallback, content: content))
        }
        return ResponseDocument(id: id, revision: revision, canonicalText: canonicalText, blocks: blocks)
    }

    static func plain(_ content: ResponseContent) -> String {
        switch content {
        case .text(let document):
            return plain(document)
        case .rows(let rows):
            return rows.map { row in
                var line = row.title
                if let detail = row.detail, !detail.isEmpty { line += " - \(detail)" }
                if let status = row.status, !status.isEmpty { line += " (\(status))" }
                return line
            }.joined(separator: "\n")
        case .notice(let notice):
            return notice.message
        case .receipt(let receipt):
            var lines = ["\(receipt.operation) to \(receipt.destination): \(receipt.state)"]
            if let detail = receipt.detail, !detail.isEmpty { lines.append(detail) }
            return lines.joined(separator: "\n")
        }
    }

    static func plain(_ table: DisplayTable) -> String {
        ([table.headers] + table.rows).map { $0.joined(separator: " | ") }.joined(separator: "\n")
    }

    static func plain(_ document: TextDocument) -> String {
        func runs(_ runs: [InlineRun]) -> String { runs.map(\.text).joined() }
        var parts: [String] = []
        for element in document.elements {
            switch element {
            case .heading(_, let inline): parts.append(runs(inline))
            case .paragraph(let inline): parts.append(runs(inline))
            case .quote(let inline): parts.append(runs(inline))
            case .code(_, let text): parts.append(text)
            case .table(let table): parts.append(plain(table))
            case .lineBreak: parts.append("")
            case .list(let ordered, let start, let items):
                parts.append(items.enumerated().map { offset, item in
                    (ordered ? "\(start + offset). " : "- ") + runs(item)
                }.joined(separator: "\n"))
            }
        }
        return parts.joined(separator: "\n")
    }
}

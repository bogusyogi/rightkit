import SwiftUI
import RightKitPresentation

/// One response document in reading order: the blocks as composed (answer text first, then facts, rows or a
/// receipt), at the major-section gap. Ordinary prose stays open on the page; only grouped values sit on a
/// surface. With no blocks the original text is shown as plain wrapped text, so content is never lost.
struct ResponseView: View {
    let document: ResponseDocument
    /// An agent answer: lets a short first paragraph read as the lead.
    var lead = false

    var body: some View {
        if document.blocks.isEmpty {
            Text(document.canonicalText)
                .font(ResponseStyle.body)
                .foregroundStyle(ResponseStyle.text)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
                .textSelection(.enabled)
        } else {
            VStack(alignment: .leading, spacing: ResponseStyle.sectionGap) {
                ForEach(document.blocks, id: \.id) { block in
                    switch block.content {
                    case .text(let text): ResponseTextView(document: text, allowLead: lead)
                    case .rows(let rows): ResponseRowsView(rows: rows)
                    case .notice(let notice): ResponseNoticeView(notice: notice)
                    case .receipt(let receipt): ResponseReceiptView(receipt: receipt)
                    }
                }
            }
        }
    }
}

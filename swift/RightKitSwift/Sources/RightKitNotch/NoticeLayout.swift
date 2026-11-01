import SwiftUI

// The toast anatomy's numbers and its measured height, moved verbatim out of
// Features/NoticeCard.swift so the notch model can size a toast without the card view.

/// The numbers of the toast anatomy, in points. The Windows port matches them.
enum NoticeMetrics {
    /// 196pt (`Design.px(521)`).
    static let width = Design.px(521)
    static let padding: CGFloat = 11
    static let corner: CGFloat = 14
    /// From the notch's face to the tail's tip: 3pt (`Design.px(8)`).
    static let gap = Design.px(8)
    /// The neck's length beyond the gap (`Design.px(37)`, 14pt): with `gap`, the
    /// strand from the notch's face to the card is 17pt long.
    static let tailLength = Design.px(37)
    /// The neck's width where it leaves the notch and where it joins the card,
    /// and its pinch between them (as a bulb's neck: the control, not the waist).
    static let neckWidth: CGFloat = 30
    static let neckPinch: CGFloat = 12
    /// Pills, the close and the life ring's frame.
    static let button: CGFloat = 22
    static let glyph: CGFloat = 13
    /// Glyph to the title's first letter.
    static let glyphGap: CGFloat = 4
    static let rowHeight: CGFloat = 28
    static let rowGap: CGFloat = 3
    static let maxVisibleRows = 4

    /// Softened (Adrian, 2026-10-12): titles at 88% white, body and secondary
    /// text at 68%, so white text on the black card is not a harsh contrast.
    /// The Windows port matches these.
    static let titleOpacity = 0.88
    static let detailOpacity = 0.68
    static let titleInk = Color.white.opacity(titleOpacity)
    static let detailInk = Color.white.opacity(detailOpacity)
    static let warningInk = Color(hex: 0xE08A47)
    /// A limit reached, or a drive almost full.
    static let alertInk = Color(hex: 0xE5675F)
    static let doneTint = Color(hex: 0x3F9D68)
    static let problemTint = Color(hex: 0xD8762B)

    static let titleFont = Font.system(size: 12.5, weight: .semibold)
    static let detailFont = Font.system(size: 9.5)
    static let titleSize: CGFloat = 12.5
    static let titleWeight: Font.Weight = .semibold
    static let detailSize: CGFloat = 9.5
    static let detailWeight: Font.Weight = .regular
}

/// **Text bound** (#558 Mac counterpart): a toast, tile or row never measures or lays out more
/// text than it can show. A peer-sent message can be tens of thousands of characters; measuring
/// and shaping all of it on the main thread froze the Windows notch. Every caller that puts
/// peer-supplied or user-supplied text on screen passes it through `bound` first, so layout cost
/// stays constant however long the text is. The full text stays wherever it is kept (Copy uses it).
enum NoticeText {
    /// Characters kept for a toast title, detail and warning, a tile preview, and a one-line label.
    /// A detail shows at most 4 lines of about 40 characters; the limits leave room for narrow glyphs.
    static let titleLimit = 120
    static let detailLimit = 480
    static let warningLimit = 240
    static let tileLimit = 160
    static let labelLimit = 80

    /// `text` cut to at most `limit` unicode scalars (so never more than `limit` characters, and a
    /// single huge combining cluster is bounded too), ending in an ellipsis when cut. Cost is
    /// proportional to `limit`, not to the length of `text`.
    static func bound(_ text: String, _ limit: Int) -> String {
        // utf8.count is constant time for a native string; a string of at most `limit` bytes
        // cannot hold more than `limit` scalars.
        if text.utf8.count <= limit { return text }
        let scalars = text.unicodeScalars
        guard let cut = scalars.index(scalars.startIndex, offsetBy: limit, limitedBy: scalars.endIndex),
              cut < scalars.endIndex else { return text }
        return String(scalars[..<cut]) + "…"
    }

    /// Whether `bound(text, limit)` would cut (constant-time check for a log event).
    static func exceeds(_ text: String, _ limit: Int) -> Bool {
        let scalars = text.unicodeScalars
        return text.utf8.count > limit
            && scalars.index(scalars.startIndex, offsetBy: limit, limitedBy: scalars.endIndex)
                .map { $0 < scalars.endIndex } ?? false
    }

    /// The first line of `text`, at most `limit` characters: reads only the head of the string.
    static func firstLine(_ text: String, _ limit: Int) -> String {
        let head = String(text.unicodeScalars.prefix(limit * 4))
        let line = head.split(maxSplits: 1, omittingEmptySubsequences: true, whereSeparator: \.isNewline).first
            .map(String.init) ?? ""
        return bound(line, limit)
    }
}

/// **How tall a toast is**, measured so the panel's hit region and the drawn
/// card agree before SwiftUI has laid anything out.
struct NoticeLayout {
    let titleLines: Int
    let detailLines: Int
    let warningLines: Int
    let hasActionRow: Bool
    /// The × sits in the title row (a card with no action row).
    let closeInTitleRow: Bool
    let titleHeight: CGFloat
    let rowsHeight: CGFloat
    let shelfHeight: CGFloat
    let height: CGFloat

    static let titleLine = TextMeasuringRegistry.shared.lineHeight(
        size: NoticeMetrics.titleSize, weight: NoticeMetrics.titleWeight)
    static let detailLine = TextMeasuringRegistry.shared.lineHeight(
        size: NoticeMetrics.detailSize, weight: NoticeMetrics.detailWeight)
    /// Title row to detail, and the bar's slot below the text.
    static let titleToDetail: CGFloat = 4
    static let barSlot: CGFloat = 11
    static let rowsTop: CGFloat = 4
    static let checkboxSlot: CGFloat = 13
    static let actionsTop: CGFloat = 9

    /// How many lines `text` wraps to at `width`, at most `limit`.
    static func lines(_ text: String, size: CGFloat, weight: Font.Weight, width: CGFloat, limit: Int) -> Int {
        guard !text.isEmpty else { return 0 }
        // Measure only what a card of `limit` lines can show (~80 characters a line at the
        // narrowest glyphs), never the whole string.
        let text = NoticeText.bound(text, max(limit, 1) * 80)
        let measurer = TextMeasuringRegistry.shared
        let height = measurer.wrappedHeight(of: text, size: size, weight: weight, width: width)
        let count = Int(((height - 0.5) / measurer.lineHeight(size: size, weight: weight)).rounded(.up))
        return min(limit, max(1, count))
    }

    init(_ prompt: NoticePrompt) {
        let pad = NoticeMetrics.padding
        let width = NoticeMetrics.width
        let actionRow = !prompt.actions.isEmpty
        let closeInRow = !actionRow && prompt.closeChoice != nil
        hasActionRow = actionRow
        closeInTitleRow = closeInRow
        // The glyph column, and the × with its gap when it shares the row.
        let titleWidth = width - 2 * pad - NoticeMetrics.glyph - NoticeMetrics.glyphGap
            - (closeInRow ? NoticeMetrics.button + 6 : 0)
        titleLines = Self.lines(NoticeText.bound(prompt.title, NoticeText.titleLimit), size: NoticeMetrics.titleSize, weight: NoticeMetrics.titleWeight, width: titleWidth, limit: 2)
        let textWidth = width - 2 * pad
        detailLines = Self.lines(NoticeText.bound(prompt.detail, NoticeText.detailLimit), size: NoticeMetrics.detailSize, weight: NoticeMetrics.detailWeight, width: textWidth, limit: 4)
        warningLines = Self.lines(NoticeText.bound(prompt.warning ?? "", NoticeText.warningLimit), size: NoticeMetrics.detailSize, weight: NoticeMetrics.detailWeight, width: textWidth, limit: 2)
        titleHeight = max(CGFloat(max(titleLines, 1)) * Self.titleLine, closeInRow ? NoticeMetrics.button : 0)

        var rowsBlock: CGFloat = 0
        if let rows = prompt.rows {
            let shown = CGFloat(min(max(rows.items.count, 1), NoticeMetrics.maxVisibleRows))
            rowsBlock = shown * NoticeMetrics.rowHeight + (shown - 1) * NoticeMetrics.rowGap
        }
        rowsHeight = rowsBlock

        var shelfBlock: CGFloat = 0
        if let shelf = prompt.shelf, !shelf.isEmpty {
            let shown = CGFloat(min(shelf.count, NoticeMetrics.maxVisibleRows))
            shelfBlock = shown * NoticeMetrics.rowHeight + (shown - 1) * NoticeMetrics.rowGap
        }
        shelfHeight = shelfBlock

        var total = pad + titleHeight
        if detailLines > 0 { total += Self.titleToDetail + CGFloat(detailLines) * Self.detailLine }
        if warningLines > 0 {
            total += (detailLines > 0 ? 0 : Self.titleToDetail) + CGFloat(warningLines) * Self.detailLine
        }
        if prompt.progress != nil { total += Self.barSlot }
        if prompt.rows != nil { total += Self.rowsTop + rowsBlock }
        if shelfBlock > 0 { total += Self.rowsTop + shelfBlock }
        if prompt.checkbox != nil { total += Self.checkboxSlot }
        if actionRow { total += Self.actionsTop + NoticeMetrics.button }
        total += pad
        height = actionRow ? total : max(total, 2 * pad + NoticeMetrics.button)
    }
}

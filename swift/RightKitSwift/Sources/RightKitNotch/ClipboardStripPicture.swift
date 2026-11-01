#if os(macOS)
import AppKit
import SwiftUI

// The clipboard strip as a picture for the CI view renderer (docs/clipboard-spec.md section 10):
// `qa/notch-views.json` entries of kind `clipboard-strip`, drawn with the strip's own view and the
// settled card silhouette, on a notch drawn as a plain stub on the fixture's edge. The fixture
// file holds every string; nothing here invents text, and every date is computed from the
// fixture's `now`, in UTC, never from the clock.

@MainActor
enum ClipboardStripPicture {
    static func build(_ d: [String: Any]) -> AnyView? {
        func text(_ key: String) -> String? { d[key] as? String }
        func number(_ key: String) -> Double? { (d[key] as? NSNumber)?.doubleValue }

        let size = ClipCardSize(rawValue: text("size") ?? "medium") ?? .medium
        let edge = NotchEdge(rawValue: text("edge") ?? "top") ?? .top
        let neck = (text("placement") ?? "notch") == "notch"
        let k = ClipStripMetrics.clamp(CGFloat(number("scale") ?? 1))
        let now = Date(timeIntervalSince1970: number("now") ?? 1_791_800_000)
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC") ?? .gmt

        // The toast alone, when the strip is not open (history off; a paste without permission).
        let toast = text("toastMac") ?? text("toast")
        if (d["open"] as? NSNumber)?.boolValue == false {
            guard let toast else { return AnyView(NotchStub(edge: edge)) }
            return AnyView(arranged(edge: edge, notch: true, before: [], after: [toastCard(toast, action: text("action"), edge: edge)]))
        }

        let boards = (d["boards"] as? [[String: Any]] ?? []).map { board in
            ClipBoard(id: board["id"] as? String ?? "", name: board["name"] as? String ?? "",
                      colour: board["colour"] as? String ?? ClipBoard.palette[0].hex, order: 0, createdAt: 0)
        }
        let selectedBoard = text("board") ?? "history"
        var content = ClipStripContent()
        content.size = size
        content.k = k
        content.vertical = neck && edge.isVertical
        content.now = now
        content.calendar = calendar
        content.showNumbers = (d["showNumbers"] as? NSNumber)?.boolValue ?? true
        content.scrollOffset = CGFloat(number("scrollOffset") ?? 0)
        content.query = text("query") ?? ""
        content.chips = LauncherClipboard.parse(query: content.query).chips
        content.searchOpen = !content.query.isEmpty
        content.footer = text("footer")
        content.message = text("message")
        content.detail = text("detail")
        content.stripAction = text("stripAction")
        content.tabs = [
            ClipTab(id: "history", title: L10n.t("History"), colour: nil, selected: selectedBoard == "history"),
            ClipTab(id: "pinned", title: L10n.t("Pinned"), colour: nil, selected: selectedBoard == "pinned"),
        ] + boards.map { ClipTab(id: $0.id, title: $0.name, colour: $0.colour, selected: selectedBoard == $0.id) }

        let items = (d["items"] as? [[String: Any]] ?? []).map { entry -> ClipCardData in
            var card = ClipCardData(item: item(entry), appColour: entry["appColour"] as? String ?? "#8E8E93")
            card.stackCount = (entry["stackCount"] as? NSNumber)?.intValue ?? 1
            card.art = entry["thumb"] as? String
            card.boardDots = card.item.boards.compactMap { id in boards.first { $0.id == id }?.colour }
            return card
        }
        content.cards = items
        content.total = items.count
        content.selected = items.isEmpty ? -1 : Int(number("selected") ?? 0)

        let filters = d["filters"] as? [String: Any]
        if let filters {
            let kinds = (filters["types"] as? [String] ?? []).compactMap(kind(named:))
            content.filtersActive = true
            switch text("popover") {
            case "filters":
                var panel = ClipFilterPanel()
                panel.kinds = Set(kinds)
                var seen = Set<String>()
                panel.apps = items.compactMap { $0.item.sourceApp }.filter { seen.insert($0.id).inserted }
                panel.selectedApps = Set(filters["apps"] as? [String] ?? [])
                panel.date = dateID(filters["date"] as? String)
                content.popover = .filters(panel)
            default:
                break
            }
        }
        switch text("popover") {
        case "boardColour":
            let board = boards.first { $0.id == selectedBoard } ?? boards.first
            content.popover = .boardColour(board: board?.id ?? "", colour: board?.colour ?? "", hex: "")
        default:
            break
        }

        let across = ClipStripMetrics.across(vertical: content.vertical, size: size, k: k, footer: content.hasFooter)
        let length: CGFloat
        if content.vertical {
            let side = ClipStripMetrics.cardSide(size)
            length = (ClipStripMetrics.padding * 2 + ClipStripMetrics.header + ClipStripMetrics.headerGap
                + 3.5 * side + 3 * ClipStripMetrics.gap) * k
        } else {
            length = ClipStripMetrics.length(count: items.count, size: size, k: k, minimum: 420, limit: 1392)
        }
        let width = content.vertical ? across : length
        let height = content.vertical ? length : across
        let model = ClipboardStripModel(content: content)
        let strip = ClipboardStripView(model: model).frame(width: width, height: height)

        guard neck else {
            // `bottom` placement: pinned to the bottom of the display, no neck.
            return AnyView(
                strip
                    .background(RoundedRectangle(cornerRadius: ClipStripMetrics.corner * k).fill(Palette.notch))
                    .clipShape(RoundedRectangle(cornerRadius: ClipStripMetrics.corner * k))
                    .padding(.horizontal, 24)
                    .padding(.top, 40)
                    .padding(.bottom, 8))
        }

        let settled = NoticeSettled(width: width, height: height, direction: edge.tooltipDirection,
                                    tailOffset: 0, corner: ClipStripMetrics.corner * k) { strip }
        var after: [AnyView] = []
        if (d["preview"] as? NSNumber)?.boolValue == true, content.cards.indices.contains(max(content.selected, 0)) {
            after.append(AnyView(previewCard(for: content.cards[max(content.selected, 0)].item, k: k, edge: edge)))
        }
        var view = arranged(edge: edge, notch: true, before: [], after: [AnyView(settled)] + after)
        if text("popover") == "context" {
            // The native menu floats free of the panel: drawn over the strip and past its edge.
            let selected = content.cards.indices.contains(max(content.selected, 0)) ? content.cards[max(content.selected, 0)].item : nil
            let menu = ClipMenuFacsimile(
                items: ClipboardMenus.card(for: selected, inBoard: false, boards: [], actions: nil), k: k)
            view = AnyView(view.padding(.bottom, 200).overlay(alignment: .topLeading) {
                menu.offset(x: (ClipStripMetrics.padding + 40) * k, y: 32 + (NoticeEmergence.face + ClipStripMetrics.padding + ClipStripMetrics.header + 34) * k)
            })
        }
        return view
    }

    // MARK: Parts

    /// The notch, as a plain black stub on the edge, with the cards hanging from it.
    private static func arranged(edge: NotchEdge, notch: Bool, before: [AnyView], after: [AnyView]) -> AnyView {
        let stub = AnyView(NotchStub(edge: edge))
        let cards = after
        switch edge {
        case .top:
            return AnyView(VStack(spacing: -2) { stub; ForEach(Array(cards.enumerated()), id: \.offset) { $0.element } })
        case .bottom:
            return AnyView(VStack(spacing: -2) { ForEach(Array(cards.reversed().enumerated()), id: \.offset) { $0.element }; stub })
        case .left:
            return AnyView(HStack(spacing: -2) { stub; ForEach(Array(cards.enumerated()), id: \.offset) { $0.element } })
        case .right:
            return AnyView(HStack(spacing: -2) { ForEach(Array(cards.reversed().enumerated()), id: \.offset) { $0.element }; stub })
        }
    }

    private static func toastCard(_ title: String, action: String?, edge: NotchEdge) -> AnyView {
        let prompt = DiskImagePrompt(
            iconPath: "/Applications", title: title, detail: "",
            style: action == nil ? .done : .ask,
            primary: action.map { .init(choice: .noteAction, label: $0) })
        return AnyView(DiskImageCard(prompt: prompt, direction: edge.tooltipDirection))
    }

    private static func previewCard(for item: ClipItem, k: CGFloat, edge: NotchEdge) -> some View {
        var data = ClipPreviewData(kind: item.kind, text: item.preview, title: item.firstLine)
        data.imageSize = item.meta.width.flatMap { w in item.meta.height.map { CGSize(width: w, height: $0) } }
        data.colour = item.meta.hex
        let limit = ClipPreviewView.maxSize
        let lines = min(30, max(3, item.preview.split(separator: "\n", omittingEmptySubsequences: false).count + item.preview.count / 70))
        let width = limit.width * k
        let height = min(limit.height * k, 32 * k + CGFloat(lines) * 16 * k)
        let vertical = edge.isVertical
        return NoticeSettled(width: vertical ? height : width, height: vertical ? width : height,
                             direction: edge.tooltipDirection, tailOffset: 0,
                             corner: ClipStripMetrics.cardCorner * k + 2) {
            ClipPreviewView(data: data, k: k)
        }
    }

    /// 1 to 4 pixels' worth of notch: a black stub on the edge.
    private struct NotchStub: View {
        let edge: NotchEdge

        var body: some View {
            let long: CGFloat = 320
            let depth: CGFloat = 32
            let radius: CGFloat = 14
            switch edge {
            case .top:
                UnevenRoundedRectangle(bottomLeadingRadius: radius, bottomTrailingRadius: radius)
                    .fill(Palette.notch).frame(width: long, height: depth)
            case .bottom:
                UnevenRoundedRectangle(topLeadingRadius: radius, topTrailingRadius: radius)
                    .fill(Palette.notch).frame(width: long, height: depth)
            case .left:
                UnevenRoundedRectangle(bottomTrailingRadius: radius, topTrailingRadius: radius)
                    .fill(Palette.notch).frame(width: depth, height: long)
            case .right:
                UnevenRoundedRectangle(topLeadingRadius: radius, bottomLeadingRadius: radius)
                    .fill(Palette.notch).frame(width: depth, height: long)
            }
        }
    }

    // MARK: Fixture to model

    private static func int(_ value: Any?) -> Int? { (value as? NSNumber)?.intValue }

    private static func item(_ e: [String: Any]) -> ClipItem {
        let meta = e["meta"] as? [String: Any] ?? [:]
        var m = ClipMeta()
        m.chars = int(meta["chars"])
        m.lines = int(meta["lines"])
        m.language = meta["language"] as? String
        m.host = meta["host"] as? String
        m.scheme = meta["scheme"] as? String
        m.hex = meta["hex"] as? String
        m.original = meta["original"] as? String
        m.count = int(meta["count"])
        m.names = meta["names"] as? [String]
        m.anyMissing = (meta["anyMissing"] as? NSNumber)?.boolValue
        m.width = int(meta["width"])
        m.height = int(meta["height"])
        m.format = meta["format"] as? String
        m.screenshot = (meta["screenshot"] as? NSNumber)?.boolValue
        m.stackId = meta["stackId"] as? String
        m.hasHtml = (meta["hasHtml"] as? NSNumber)?.boolValue
        m.hasRtf = (meta["hasRtf"] as? NSNumber)?.boolValue
        let app = e["sourceApp"] as? [String: Any]
        let at = (e["lastUsedAt"] as? NSNumber)?.doubleValue ?? 0
        var item = ClipItem(
            id: e["id"] as? String ?? UUID().uuidString,
            kind: kind(named: e["kind"] as? String ?? "") ?? .text,
            preview: e["preview"] as? String ?? "",
            title: nil,
            sourceApp: app.map { ClipSourceApp(id: $0["id"] as? String ?? "", name: $0["name"] as? String ?? "") },
            createdAt: at, lastUsedAt: at)
        item.pinned = (e["pinned"] as? NSNumber)?.boolValue ?? false
        item.boards = e["boards"] as? [String] ?? []
        item.size = int(e["size"]) ?? 0
        item.meta = m
        return item
    }

    /// A kind by its key, or by the plural a fixture may use (`links`, `images`).
    private static func kind(named name: String) -> ClipKind? {
        if let kind = ClipKind(rawValue: name.lowercased()) { return kind }
        return ClipKind(rawValue: String(name.lowercased().dropLast()))
    }

    private static func dateID(_ name: String?) -> String {
        let value = (name ?? "any").lowercased()
        if value.contains("yesterday") { return "yesterday" }
        if value.contains("today") { return "today" }
        if value.contains("30") { return "last30" }
        if value.contains("7") { return "last7" }
        return "any"
    }
}
#endif
